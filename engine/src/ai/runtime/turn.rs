//! V2〜V4の手番進行。
//!
//! 1回の呼び出しで命令を1件だけ発行し、発行できる命令が尽きたら手番を終える。
//! 段階は次の順で、最初に命令を出せた段階で返る。
//!
//! 1. 計画済みの生産命令の続き
//! 2. （手番の最初だけ）Squad編成と目標探索
//! 3. 生産施設上でFormingする作戦unitの退避
//! 4. 輸送Squadの1手
//! 5. 通常行動（戦術による最良の1手）
//! 6. （V4）Squadを失ったunitを再接続した後の通常行動
//! 7. （V4）通常行動で進めなかった作戦unitの生産施設からの退避
//! 8. 生産
//! 9. 手番終了（遊兵の計上と次フェーズの要求）
//!
//! 版・経路で異なるのは、通常行動を選ぶ戦術（[`TurnTactics`]）と、3の退避の対象だけである。

use crate::ai::code_path::{self, CodePath};
use crate::ai::command::AiCommand;
use crate::ai::context::tactical::transport_has_other_actionable_cargo;
use crate::ai::runtime::issue::execute_ai_command;
use crate::ai::runtime::production_queue::AiProductionCommandQueue;
use crate::ai::runtime::production_queue::AiProductionCooldown;
use crate::ai::runtime::production_queue::issue_production_command;
use crate::ai::runtime::production_queue::take_next_queued_production_command;
use crate::ai::runtime::state::AiActionCooldown;
use crate::ai::runtime::state::AiTurnProgress;
use crate::ai::tactics::relief::decide_forming_campaign_site_relief;
use crate::ai::tactics::small_map::squad_executor::decide_squad_action_v4;
use crate::ai::tactics::v2::decide_action as decide_ai_action_v2;

use crate::ai::squad::{MissionPhase, MissionType, SquadManager, TransportPhase};
use crate::collections::HashSet;
use crate::components::{GridPosition, PlayerId};
use bevy_ecs::prelude::*;

/// 通常行動を選ぶ戦術。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TurnTactics {
    /// V2/V3と通常経路のV4: 全unitを横断して最良の1手を選ぶ。
    Standard,
    /// 小規模マップのV4: 本拠防衛を先に判定し、SquadごとにDAG・占領契約に沿って1手を選ぶ。
    SmallMapSquads,
}

/// 1回の手番進行で共有する事実。
struct TurnStep {
    player_id: PlayerId,
    turn: u32,
    /// この手番に行動済みのEntity。
    acted: HashSet<Entity>,
    uses_v3_tactics: bool,
    is_v4: bool,
    tactics: TurnTactics,
}

impl TurnStep {
    fn begin(world: &World, player_id: PlayerId, path: CodePath) -> Self {
        let ai_version = crate::ai::resolve_player_ai_version(world, player_id);
        Self {
            player_id,
            turn: world
                .get_resource::<crate::resources::MatchState>()
                .map_or(0, |state| state.current_turn_number.0),
            acted: world
                .get_resource::<AiActionCooldown>()
                .map(|cooldown| cooldown.0.clone())
                .unwrap_or_default(),
            uses_v3_tactics: ai_version.uses_v3_tactics(),
            is_v4: ai_version == crate::ai::ai_version::AiVersion::V4,
            tactics: if path == CodePath::Extended {
                TurnTactics::SmallMapSquads
            } else {
                TurnTactics::Standard
            },
        }
    }

    /// 通常行動の前に、実行中のAttack/Capture任務まで生産施設から退避させるか。
    /// 小規模マップでは、工場から届く敵への合法な初撃を退避Waitが横取りしないよう、
    /// 前段ではForming中の輸送役・護衛待ち占領役だけを動かす。
    fn early_relief_includes_active_missions(&self) -> bool {
        self.is_v4 && self.tactics == TurnTactics::Standard
    }
}

/// V2〜V4のAIの手番を1命令だけ進める。命令を出したらその内容、手番を終えたらNone。
pub(crate) fn execute_turn_step(world: &mut World, player_id: PlayerId) -> Option<String> {
    // 手番の入口で実装経路を確定する。小規模マップのV4だけが拡張経路を使う。
    let path = code_path::for_player(world, player_id);
    let _code_path = code_path::enter(path);
    let step = TurnStep::begin(world, player_id, path);

    if has_queued_production(world, &step)
        && let Some(command) = take_next_queued_production_command(world, player_id, step.turn)
    {
        return issue_production_command(world, command);
    }
    prepare_squads(world, &step);
    crate::ai::wait_probe::stage(world, "early_production_relief");
    // 未完成の島嶼輸送パッケージが生産施設上でFormingすると、不足している次の
    // 輸送役を自分で生産不能にする。任務所属は維持したまま隣接待機地へ一歩だけ退避する。
    if step.uses_v3_tactics
        && let Some((entity, command)) = decide_forming_campaign_site_relief(
            world,
            player_id,
            &step.acted,
            step.early_relief_includes_active_missions(),
        )
    {
        return Some(issue_unit_command(world, entity, command));
    }
    // 戦艦は移動した手番に撃てないため、汎用の戦術評価より先に射撃・位置取りを決める。
    // 他unitの攻撃より先に撃つことで、後続の直接攻撃が撃破を取りやすくなる。
    if step.is_v4
        && let Some((entity, command)) =
            crate::ai::v4::maritime::decide_naval_action(world, player_id, &step.acted)
    {
        return Some(issue_unit_command(world, entity, command));
    }
    // 港・空港に居座る地上部隊を退かせ、艦船・航空機の生産枠を空ける（V4）。
    if step.is_v4
        && let Some((entity, command)) =
            crate::ai::v4::maritime::decide_port_relief(world, player_id, &step.acted)
    {
        return Some(issue_unit_command(world, entity, command));
    }
    // 島で隔てられた盤面では、仕事の無い陸塊に取り残された地上部隊を輸送船・輸送ヘリで
    // 運ぶ（V4）。担当しない輸送部隊は従来の輸送Squadへ残す。
    if step.is_v4
        && let Some((entity, command)) =
            crate::ai::v4::maritime::decide_sealift_action(world, player_id, &step.acted)
    {
        return Some(issue_transport_command(world, entity, command));
    }
    crate::ai::wait_probe::stage(world, "transport");
    if let Some((entity, command)) = select_transport_step(world, &step) {
        return Some(issue_transport_command(world, entity, command));
    }
    let decision_skip = decision_skip_entities(world, &step);
    crate::ai::wait_probe::stage(world, "normal_tactics");
    if let Some((entity, command)) = decide_normal_action(world, &step, &decision_skip) {
        return Some(issue_unit_command(world, entity, command));
    }
    if step.is_v4 {
        // Drop/Capture/作戦完了で手番途中にSquadを失ったEntityを、全通常行動が尽きた
        // 時点で再接続する。Reserve行動そのものが目標到達でSquadを完了させる場合もあるため、
        // 通常行動が尽きるたびに取り直す。行動候補が無ければ生産へ進む。
        crate::ai::squad::reconcile_v4_end_turn_reserves(world, player_id);
        crate::ai::wait_probe::stage(world, "reassigned_tactics");
        if let Some((entity, command)) = decide_reassigned_action(world, &step) {
            return Some(issue_unit_command(world, entity, command));
        }
        // 通常の作戦行動で進めなかった任務所属unitも、生産施設だけは塞ぎ続けない。
        // 先に通常行動を試した後のfallbackなので、前線へ進めるunitの移動距離は奪わない。
        crate::ai::wait_probe::stage(world, "late_production_relief");
        if let Some((entity, command)) =
            decide_forming_campaign_site_relief(world, player_id, &step.acted, true)
        {
            return Some(issue_unit_command(world, entity, command));
        }
    }
    issue_production_or_end_turn(world, &step)
}

fn has_queued_production(world: &World, step: &TurnStep) -> bool {
    world
        .get_resource::<AiProductionCommandQueue>()
        .is_some_and(|queue| {
            queue.player_id == Some(step.player_id)
                && queue.turn == step.turn
                && !queue.commands.is_empty()
        })
}

/// 手番の最初の呼び出しだけSquadを編成し、ビーム探索で目標を割り当てる。
/// V3以降は行動可能unitが無くても同じ手番に再編成しない（V1/V2は従来条件を維持）。
/// それ以外の呼び出しでは、降車・搭乗が実際に起きた場合だけSquadを更新する。
fn prepare_squads(world: &mut World, step: &TurnStep) {
    let squads_planned_this_turn = world
        .get_resource::<AiTurnProgress>()
        .is_some_and(|cache| cache.squads_planned(step.player_id));
    let should_plan_squads =
        step.acted.is_empty() && (!step.uses_v3_tactics || !squads_planned_this_turn);
    if should_plan_squads {
        crate::ai::squad::plan_squads(world, step.player_id);
        crate::ai::beam_search::run_squad_beam_search(world, step.player_id);
    } else if transport_boarding_changed(world) {
        crate::ai::squad::update_squads(world, step.player_id);
    }
}

/// 輸送Squadで降車済みのcargo、または全cargoの搭乗完了が起きたか。
fn transport_boarding_changed(world: &World) -> bool {
    let Some(manager) = world.get_resource::<SquadManager>() else {
        return false;
    };
    manager.squads.iter().any(|squad| {
        let delivered_cargo = matches!(
            squad.plan.phase,
            MissionPhase::Transport(TransportPhase::Transit | TransportPhase::Drop)
        ) && squad.cargo_entities.iter().any(|cargo| {
            world
                .get::<crate::components::Transporting>(*cargo)
                .is_none()
                && world.get::<GridPosition>(*cargo).is_some()
        });
        let pickup_completed = matches!(
            squad.plan.phase,
            MissionPhase::Transport(TransportPhase::Pickup)
        ) && !squad.cargo_entities.is_empty()
            && squad.cargo_entities.iter().all(|cargo| {
                squad.transport_entity.is_some_and(|transport| {
                    world
                        .get::<crate::components::Transporting>(*cargo)
                        .is_some_and(|transporting| transporting.0 == transport)
                })
            });
        delivered_cargo || pickup_completed
    })
}

/// 自軍の輸送Squadから、行動できる輸送役・cargoの次の1手を選ぶ。
/// V4では、命令を受けるEntityがそのSquadへ所属している場合だけ実行する。
fn select_transport_step(world: &mut World, step: &TurnStep) -> Option<(Entity, AiCommand)> {
    let mut manager = world.remove_resource::<SquadManager>()?;
    let mut transport_action = None;
    for mut squad in manager.edit_each() {
        if squad.plan.mission_type != MissionType::Transport
            || squad.plan.owner_id != Some(step.player_id)
            || !crate::ai::squad::squad_is_mutable_by_player(world, &squad, step.player_id)
        {
            continue;
        }
        // 輸送役と指名cargoは独立して行動できるため、いずれかが行動可能なら継続する。
        let transport_acted = squad
            .transport_entity
            .is_none_or(|entity| step.acted.contains(&entity));
        let all_cargo_acted = !squad.cargo_entities.is_empty()
            && squad
                .cargo_entities
                .iter()
                .all(|entity| step.acted.contains(entity));
        if transport_acted && all_cargo_acted {
            continue;
        }
        let Some((entity, command)) =
            crate::ai::squad::execute_transport_squad_step(world, &mut squad, &step.acted)
        else {
            continue;
        };
        if step.is_v4
            && !world
                .get_resource::<crate::ai::operation_assignment::UnitOperationRegistry>()
                .and_then(|registry| registry.assignment(entity))
                .is_some_and(|assignment| assignment.squad_id == Some(squad.id))
        {
            continue;
        }
        if !step.acted.contains(&entity) {
            transport_action = Some((entity, command));
            break;
        }
    }
    world.insert_resource(manager);
    transport_action
}

/// 通常行動で除外するEntity: 行動済みに加え、輸送Squadの輸送役とcargo。
/// Forming中は複数の輸送役をmembersへ束ねる。代表transport_entityだけを除外すると
/// 残りが通常行動へ漏れ、空荷で前線へ進んでしまう。
fn decision_skip_entities(world: &World, step: &TurnStep) -> HashSet<Entity> {
    let mut skip = step.acted.clone();
    if let Some(manager) = world.get_resource::<SquadManager>() {
        for squad in manager
            .squads
            .iter()
            .filter(|squad| squad.plan.mission_type == MissionType::Transport)
        {
            skip.extend(squad.members.iter().copied());
            if let Some(transport_entity) = *squad.transport_entity {
                skip.insert(transport_entity);
            }
            skip.extend(squad.cargo_entities.iter().copied());
        }
    }
    skip
}

/// 通常行動。小規模マップでは、Squadの目標同期より先に占領開始turnを失うと後から
/// 戦闘力を足しても収入差を埋められないため、本拠防衛（未割当歩兵の占領開始を含む）を先に判定する。
fn decide_normal_action(
    world: &mut World,
    step: &TurnStep,
    skip: &HashSet<Entity>,
) -> Option<(Entity, AiCommand)> {
    match step.tactics {
        TurnTactics::Standard => decide_ai_action_v2(world, step.player_id, skip),
        TurnTactics::SmallMapSquads => {
            crate::ai::v4::home_defense::decide_action(world, step.player_id, skip)
                .or_else(|| decide_squad_action_v4(world, step.player_id, skip))
        }
    }
}

/// 再接続後の通常行動。輸送Squadのunitも対象に含める（行動済みだけを除外する）。
fn decide_reassigned_action(world: &mut World, step: &TurnStep) -> Option<(Entity, AiCommand)> {
    match step.tactics {
        TurnTactics::Standard => decide_ai_action_v2(world, step.player_id, &step.acted),
        TurnTactics::SmallMapSquads => decide_squad_action_v4(world, step.player_id, &step.acted),
    }
}

/// 同じ手番で再選択しないEntityを行動済みcooldownへ登録する。
fn mark_acted(world: &mut World, entities: impl IntoIterator<Item = Entity>) {
    if let Some(mut cooldown) = world.get_resource_mut::<AiActionCooldown>() {
        cooldown.0.extend(entities);
    } else {
        world.insert_resource(AiActionCooldown(HashSet::from_iter(entities)));
    }
}

/// unit命令を適用して行動済みにし、ログ用の命令文字列を返す。
fn issue_unit_command(world: &mut World, entity: Entity, command: AiCommand) -> String {
    let command_text = format!("{:?}", command);

    execute_ai_command(world, entity, command);
    mark_acted(world, [entity]);
    command_text
}

/// 輸送Squadの命令を適用する。Dropは輸送役だけでなく降車したcargoの行動も消費するので
/// 両方を記録し、行動済みのcargoを遊兵として数えない。ただしUnload systemは未行動cargoが
/// 残る間は輸送役を行動完了にしないため、最後のcargoを降ろすまで輸送役は行動済みにしない。
fn issue_transport_command(world: &mut World, entity: Entity, command: AiCommand) -> String {
    let (dropped_cargo, transport_keeps_dropping) = match &command {
        AiCommand::Drop { cargo_entity, .. } => (
            Some(*cargo_entity),
            transport_has_other_actionable_cargo(world, entity, *cargo_entity),
        ),
        _ => (None, false),
    };
    let command_text = format!("{:?}", command);
    execute_ai_command(world, entity, command);
    let acted_transport = (!transport_keeps_dropping).then_some(entity);
    mark_acted(world, acted_transport.into_iter().chain(dropped_cargo));
    command_text
}

/// 生産命令を1件発行する。直前に失敗した命令と生産済みの施設は飛ばす。
/// 敵が行動しない同一生産フェーズでは全施設分を一度だけ計画し、残りを列に積む。
/// 発行できる命令が無ければ手番を終える。
fn issue_production_or_end_turn(world: &mut World, step: &TurnStep) -> Option<String> {
    let produced_sites = world
        .get_resource::<AiProductionCooldown>()
        .map(|cooldown| cooldown.0.clone())
        .unwrap_or_default();
    let (last_error, last_event) = world
        .get_resource::<crate::resources::ProductionDiagnostic>()
        .map_or((None, None), |diagnostic| {
            (diagnostic.last_error.clone(), diagnostic.last_event.clone())
        });
    let failed_last_time =
        |command_text: &str| last_error.is_some() && last_event.as_deref() == Some(command_text);

    let mut queue = world
        .remove_resource::<AiProductionCommandQueue>()
        .unwrap_or_default();
    if queue.player_id != Some(step.player_id) || queue.turn != step.turn {
        queue.player_id = Some(step.player_id);
        queue.turn = step.turn;
        queue.commands.clear();
    }
    // 直前の計画に残る別施設の命令を、重い戦略分析を再実行せず先に消費する。
    while let Some(command) = queue.commands.pop_front() {
        if produced_sites.contains(&(command.target_x, command.target_y)) {
            continue;
        }
        let command_text = format!("{:?}", command);
        if failed_last_time(&command_text) {
            continue;
        }
        world.insert_resource(queue);
        return issue_production_command(world, command);
    }

    let mut planned =
        super::super::production::decide_production(world, step.player_id).into_iter();
    while let Some(command) = planned.next() {
        if produced_sites.contains(&(command.target_x, command.target_y)) {
            continue;
        }
        let command_text = format!("{:?}", command);
        if failed_last_time(&command_text) {
            if let Some(mut cooldown) = world.get_resource_mut::<AiProductionCooldown>() {
                cooldown.0.insert((command.target_x, command.target_y));
            }
            continue;
        }
        queue.commands.extend(planned);
        world.insert_resource(queue);
        return issue_production_command(world, command);
    }
    world.insert_resource(queue);
    // 作戦生産が使い切らなかった資金で、海上戦域に寄与する艦船を補う（V4）。
    if step.is_v4
        && let Some(command) = crate::ai::v4::maritime::decide_supplementary_production(
            world,
            step.player_id,
            &produced_sites,
        )
        && !failed_last_time(&format!("{:?}", command))
    {
        return issue_production_command(world, command);
    }
    end_turn(world, step.player_id);
    None
}

/// 手番を終える。終了直前は「この手番に結局何が動かなかったか」が確定する唯一の点で、
/// 行動済みcooldownは手番境界で破棄されるため、ここで遊兵を計上する。
fn end_turn(world: &mut World, player_id: PlayerId) {
    let acted_entities = world
        .get_resource::<AiActionCooldown>()
        .map(|cooldown| cooldown.0.clone())
        .unwrap_or_default();
    let idle_audit = crate::ai::idle_audit::audit_idle_units(world, player_id, &acted_entities);
    let turn = world
        .get_resource::<crate::resources::MatchState>()
        .map_or(0, |state| state.current_turn_number.0);
    if let Some(mut diagnostics) =
        world.get_resource_mut::<crate::ai::idle_audit::IdleAuditDiagnostics>()
    {
        diagnostics.record(turn, idle_audit);
    } else {
        let mut diagnostics = crate::ai::idle_audit::IdleAuditDiagnostics::default();
        diagnostics.record(turn, idle_audit);
        world.insert_resource(diagnostics);
    }
    if let Some(mut events) = world.get_resource_mut::<Events<crate::events::NextPhaseCommand>>() {
        events.send(crate::events::NextPhaseCommand);
    }
}
