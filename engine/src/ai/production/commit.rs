//! 購入案の確定と発行直前の検証。
//!
//! 購入は次の状態を区別する。
//! - Planned: 当手番の配分で確定し、発行待ちの列へ入った購入案。
//! - Issued: 生産命令として発行した（ゲームSystemが適用するまで成功ではない）。
//! - ObservedSuccess / Failed: 生産完了イベントとの照合結果（照合は版別Registryが行う）。
//!
//! 計画を立てた時点から発行までの間に資金・施設の状態は変わり得るため、
//! 発行直前に `validate_issue` で実盤面に照らして再検証する。検証に失敗した購入案は
//! 発行せず（未発行の取消）、理由を `IssueAudit` に残す。

use crate::components::UnitStats;
use crate::events::ProduceUnitCommand;
use crate::resources::master_data::MasterDataRegistry;
use crate::resources::{MatchState, Phase, Players, UnitRegistry};
use bevy_ecs::prelude::*;

/// 購入の目的。継続計画の工程だけが工程参照を必須にし、形式だけの計画は作らない。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PurchasePurpose<StepRef> {
    /// 永続計画の工程を実行する購入。
    PlanStep(StepRef),
    /// 永続計画を持たない作戦枠（占領・輸送・迎撃・計画化されない戦闘）を埋める購入。
    OperationSlot,
    /// 実在敵への即時増援として、残額から追加する購入。
    ImmediateReinforcement,
    /// 空き施設と余剰資金で前線作戦へ送る動員購入。
    Mobilization,
}

/// 発行直前の検証で購入案を発行しない理由。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IssueRejection {
    /// 発行待ち列が別手番のもの。
    StaleTurn { planned: u32, current: u32 },
    /// 行動中のplayerと命令のplayerが一致しない。
    WrongPlayer,
    /// Main以外のフェーズでは生産を発行できない。
    WrongPhase,
    /// 終局後の発行を禁止する。
    GameOver,
    /// 命令のプレイヤーが存在しない。
    UnknownPlayer,
    /// 兵種の能力値が登録されていない。
    UnknownUnitType,
    /// 計画後に資金が減り、生産費を払えない。
    InsufficientFunds { required: u32, available: u32 },
    /// 施設の喪失・占有・生産不可など、ゲームの生産規則を満たさない。
    FacilityUnavailable(String),
}

/// 発行しなかった購入案の記録（診断用、判断には使わない）。
#[derive(Debug, Clone)]
pub struct RejectedIssue {
    pub turn: u32,
    pub command: ProduceUnitCommand,
    pub rejection: IssueRejection,
}

/// 発行直前の検証で取り消した購入案の監査記録。
#[derive(Resource, Debug, Default)]
pub struct IssueAudit {
    rejected: Vec<RejectedIssue>,
}

impl IssueAudit {
    /// 保持する件数の上限。診断用なので古い記録から捨てる。
    const CAPACITY: usize = 256;

    /// 取り消しを記録する。
    pub fn record(&mut self, turn: u32, command: ProduceUnitCommand, rejection: IssueRejection) {
        if self.rejected.len() >= Self::CAPACITY {
            self.rejected.remove(0);
        }
        self.rejected.push(RejectedIssue {
            turn,
            command,
            rejection,
        });
    }

    /// 記録済みの取り消し。
    pub fn rejected(&self) -> &[RejectedIssue] {
        &self.rejected
    }
}

/// 生産命令を、発行直前の実盤面（資金・施設）に照らして検証する。
///
/// ゲームSystemの生産規則（`can_produce_at`）と資金条件をそのまま使う。
/// 検証に必要なリソースが無いテスト用Worldでは、その項目の検証を省略する。
pub fn validate_issue(
    world: &mut World,
    command: &ProduceUnitCommand,
) -> Result<(), IssueRejection> {
    validate_turn(world, command, None)?;
    if let Some(players) = world.get_resource::<Players>() {
        let Some(player) = players.0.iter().find(|p| p.id == command.player_id) else {
            return Err(IssueRejection::UnknownPlayer);
        };
        let available = player.funds;
        if let Some(registry) = world.get_resource::<UnitRegistry>() {
            let Some(required) = registry
                .get_stats(command.unit_type)
                .map(|stats: &UnitStats| stats.cost)
            else {
                return Err(IssueRejection::UnknownUnitType);
            };
            if available < required {
                return Err(IssueRejection::InsufficientFunds {
                    required,
                    available,
                });
            }
        }
    }
    if world.contains_resource::<MasterDataRegistry>() {
        world
            .resource_scope(|world, master: Mut<MasterDataRegistry>| {
                crate::systems::production::can_produce_at(
                    world,
                    command.player_id,
                    command.target_x,
                    command.target_y,
                    command.unit_type,
                    &master,
                )
            })
            .map_err(IssueRejection::FacilityUnavailable)?;
    }
    Ok(())
}

/// 計画手番と発行時の手番を照合する。テスト用の部分Worldは存在する項目だけ検査する。
fn validate_turn(
    world: &World,
    command: &ProduceUnitCommand,
    planned_turn: Option<u32>,
) -> Result<(), IssueRejection> {
    if let Some(state) = world.get_resource::<MatchState>() {
        if state.game_over.is_some() {
            return Err(IssueRejection::GameOver);
        }
        if planned_turn.is_some_and(|turn| turn != state.current_turn_number.0) {
            return Err(IssueRejection::StaleTurn {
                planned: planned_turn.unwrap(),
                current: state.current_turn_number.0,
            });
        }
        if state.current_phase != Phase::Main {
            return Err(IssueRejection::WrongPhase);
        }
        if let Some(players) = world.get_resource::<Players>()
            && players
                .0
                .get(state.active_player_index.0)
                .is_none_or(|player| player.id != command.player_id)
        {
            return Err(IssueRejection::WrongPlayer);
        }
    }
    Ok(())
}

/// 手番つき命令の共通検証。汎用・campaignの両方で発行直前に使う。
pub fn validate_issue_for_turn(
    world: &mut World,
    turn: u32,
    command: &ProduceUnitCommand,
) -> Result<(), IssueRejection> {
    validate_turn(world, command, Some(turn))?;
    validate_issue(world, command)
}

/// 発行待ちの列から、検証を通る最初の購入案を取り出す。
///
/// 検証に失敗した案は発行せず `IssueAudit` へ記録して捨て、次の案を検証する。
/// 戻り値は発行する案と、取り消した案（呼び出し側が版別の予約を解く）の組。
pub fn next_issuable<T>(
    world: &mut World,
    turn: u32,
    queue: &mut std::collections::VecDeque<T>,
    command_of: impl Fn(&T) -> &ProduceUnitCommand,
) -> (Option<T>, Vec<T>) {
    let mut cancelled = Vec::new();
    while let Some(candidate) = queue.pop_front() {
        match validate_issue_for_turn(world, turn, command_of(&candidate)) {
            Ok(()) => return (Some(candidate), cancelled),
            Err(rejection) => {
                let command = command_of(&candidate).clone();
                world
                    .get_resource_or_insert_with(IssueAudit::default)
                    .record(turn, command, rejection);
                cancelled.push(candidate);
            }
        }
    }
    (None, cancelled)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::{GridPosition, PlayerId, Property};
    use crate::resources::{Player, Terrain, UnitType};
    use std::collections::VecDeque;
    /// プレイヤーIDから現在資金を読む。
    fn funds_of(world: &World, player_id: PlayerId) -> Option<u32> {
        world
            .get_resource::<Players>()?
            .0
            .iter()
            .find(|p| p.id == player_id)
            .map(|p| p.funds)
    }

    fn world_with_funds(funds: u32) -> World {
        let mut world = World::new();
        world.insert_resource(Players(vec![Player {
            id: PlayerId(1),
            name: "P1".to_string(),
            funds,
        }]));
        world
    }

    /// 軽歩兵だけを登録した能力値表。
    fn infantry_registry() -> UnitRegistry {
        let stats = UnitStats {
            unit_type: UnitType::Infantry,
            cost: 1000,
            max_movement: 3,
            movement_type: crate::resources::MovementType::Infantry,
            max_fuel: 99,
            max_ammo1: 10,
            ammo1_cost: 0,
            max_ammo2: 0,
            ammo2_cost: 0,
            min_range: 1,
            max_range: 1,
            daily_fuel_consumption: 0,
            can_capture: true,
            can_supply: false,
            max_cargo: 0,
            loadable_unit_types: vec![],
        };
        UnitRegistry(crate::collections::HashMap::from_iter([(
            UnitType::Infantry,
            stats,
        )]))
    }

    fn command(x: usize, unit_type: UnitType) -> ProduceUnitCommand {
        ProduceUnitCommand {
            player_id: PlayerId(1),
            target_x: x,
            target_y: 0,
            unit_type,
        }
    }

    #[test]
    fn rejects_unknown_player() {
        let mut world = world_with_funds(0);
        let mut cmd = command(0, UnitType::Infantry);
        cmd.player_id = PlayerId(9);
        assert_eq!(
            validate_issue(&mut world, &cmd),
            Err(IssueRejection::UnknownPlayer)
        );
    }

    #[test]
    fn rejects_when_funds_dropped_after_planning() {
        let mut world = world_with_funds(500);
        world.insert_resource(infantry_registry());
        let rejection = validate_issue(&mut world, &command(0, UnitType::Infantry));
        assert!(matches!(
            rejection,
            Err(IssueRejection::InsufficientFunds {
                required: 1000,
                available: 500
            })
        ));
        assert_eq!(funds_of(&world, PlayerId(1)), Some(500));
    }

    #[test]
    fn rejects_lost_facility_and_issues_next_valid_order() {
        let master = MasterDataRegistry::load().expect("マスターデータ");
        let mut world = world_with_funds(10_000);
        world.insert_resource(infantry_registry());
        world.insert_resource(master);
        world.spawn((
            GridPosition { x: 0, y: 0 },
            Property::new(Terrain::Capital, Some(PlayerId(1)), 100),
        ));
        // x=1 の工場は敵に奪われた
        world.spawn((
            GridPosition { x: 1, y: 0 },
            Property::new(Terrain::Factory, Some(PlayerId(2)), 100),
        ));
        world.spawn((
            GridPosition { x: 2, y: 0 },
            Property::new(Terrain::Factory, Some(PlayerId(1)), 100),
        ));
        let mut queue = VecDeque::from(vec![
            command(1, UnitType::Infantry),
            command(2, UnitType::Infantry),
        ]);
        let (issued, cancelled) = next_issuable(&mut world, 3, &mut queue, |cmd| cmd);
        assert_eq!(issued.map(|cmd| cmd.target_x), Some(2));
        assert_eq!(cancelled.len(), 1);
        assert!(queue.is_empty());
        let audit = world.resource::<IssueAudit>();
        assert_eq!(audit.rejected().len(), 1);
        assert_eq!(audit.rejected()[0].turn, 3);
        assert!(matches!(
            audit.rejected()[0].rejection,
            IssueRejection::FacilityUnavailable(_)
        ));
    }

    #[test]
    fn stale_turn_budget_facility_loss_and_wrong_player_are_rejected() {
        let mut world = world_with_funds(5000);
        world.insert_resource(infantry_registry());
        world.insert_resource(MatchState::default());
        let cmd = command(0, UnitType::Infantry);
        assert_eq!(
            validate_issue_for_turn(&mut world, 0, &cmd),
            Err(IssueRejection::StaleTurn {
                planned: 0,
                current: 1
            })
        );
        assert!(validate_issue_for_turn(&mut world, 1, &cmd).is_ok());
        world.resource_mut::<MatchState>().active_player_index.0 = 1;
        assert_eq!(
            validate_issue(&mut world, &cmd),
            Err(IssueRejection::WrongPlayer)
        );
        world.resource_mut::<MatchState>().active_player_index.0 = 0;
        world.resource_mut::<MatchState>().current_phase = Phase::EndTurn;
        assert_eq!(
            validate_issue(&mut world, &cmd),
            Err(IssueRejection::WrongPhase)
        );
        world.resource_mut::<MatchState>().current_phase = Phase::Main;
        world.resource_mut::<Players>().0[0].funds = 999;
        assert!(matches!(
            validate_issue(&mut world, &cmd),
            Err(IssueRejection::InsufficientFunds { .. })
        ));
        world.resource_mut::<Players>().0[0].funds = 5000;
        world.insert_resource(MasterDataRegistry::load().unwrap());
        world.spawn((
            GridPosition { x: 0, y: 0 },
            Property::new(Terrain::Factory, Some(PlayerId(2)), 100),
        ));
        assert!(matches!(
            validate_issue(&mut world, &cmd),
            Err(IssueRejection::FacilityUnavailable(_))
        ));
        world.resource_mut::<MatchState>().game_over =
            Some(crate::resources::GameOverCondition::Draw);
        assert_eq!(
            validate_issue(&mut world, &cmd),
            Err(IssueRejection::GameOver)
        );
    }

    #[test]
    fn issue_validation_does_not_spend_or_spawn_and_rechecks_after_application() {
        let master = MasterDataRegistry::load().unwrap();
        let (mut world, mut schedule) =
            crate::setup::initialize_world_from_master_data(&master, "map_1").unwrap();
        let player =
            world.resource::<Players>().0[world.resource::<MatchState>().active_player_index.0].id;
        let facility = world
            .query::<(&GridPosition, &Property)>()
            .iter(&world)
            .find(|(_, property)| {
                property.owner_id == Some(player) && property.terrain == Terrain::Capital
            })
            .map(|(pos, _)| *pos)
            .unwrap();
        let cmd = ProduceUnitCommand {
            player_id: player,
            target_x: facility.x,
            target_y: facility.y,
            unit_type: UnitType::Infantry,
        };
        let funds = funds_of(&world, player).unwrap();
        let count = world.entities().len();
        let turn = world.resource::<MatchState>().current_turn_number.0;
        validate_issue_for_turn(&mut world, turn, &cmd).unwrap();
        validate_issue_for_turn(&mut world, turn, &cmd).unwrap();
        assert_eq!(funds_of(&world, player), Some(funds));
        assert_eq!(world.entities().len(), count);
        world.send_event(cmd.clone());
        schedule.run(&mut world);
        assert!(funds_of(&world, player).unwrap() < funds);
        assert_eq!(world.entities().len(), count + 1);
        assert!(matches!(
            validate_issue_for_turn(&mut world, turn, &cmd),
            Err(IssueRejection::FacilityUnavailable(_))
        ));
    }

    #[test]
    fn stale_campaign_queue_is_audited_without_spending_or_spawning() {
        use crate::ai::production::turn_state::{ProductionTurnState, next_campaign_command};
        let mut world = world_with_funds(5000);
        world.insert_resource(MatchState::default());
        let mut state = ProductionTurnState::default();
        state.set_campaign_production_plan(
            PlayerId(1),
            0,
            vec![command(0, UnitType::Infantry)],
            true,
        );
        world.insert_resource(state);
        assert!(next_campaign_command(&mut world, PlayerId(1)).is_none());
        assert_eq!(funds_of(&world, PlayerId(1)), Some(5000));
        assert_eq!(world.entities().len(), 0);
        assert!(matches!(
            world.resource::<IssueAudit>().rejected()[0].rejection,
            IssueRejection::StaleTurn { .. }
        ));
    }

    #[test]
    fn runtime_rechecks_before_publishing_campaign_or_generic_command() {
        use crate::ai::runtime::production_queue::{
            AiProductionCooldown, issue_production_command,
        };
        let mut world = world_with_funds(5000);
        world.insert_resource(MatchState {
            current_phase: Phase::EndTurn,
            ..MatchState::default()
        });
        world.insert_resource(Events::<ProduceUnitCommand>::default());
        assert!(issue_production_command(&mut world, command(0, UnitType::Infantry)).is_none());
        assert!(world.resource::<Events<ProduceUnitCommand>>().is_empty());
        assert!(!world.contains_resource::<AiProductionCooldown>());
        assert_eq!(funds_of(&world, PlayerId(1)), Some(5000));
        assert_eq!(world.entities().len(), 0);
        assert_eq!(
            world.resource::<IssueAudit>().rejected()[0].rejection,
            IssueRejection::WrongPhase
        );
    }

    #[test]
    fn terminal_purchase_log_does_not_publish_or_mark_a_plan_issued() {
        use crate::ai::production::turn_state::ProductionTurnState;
        use crate::ai::runtime::production_queue::issue_production_command;
        let mut world = world_with_funds(5000);
        world.insert_resource(MatchState {
            game_over: Some(crate::resources::GameOverCondition::Draw),
            ..MatchState::default()
        });
        world.insert_resource(Events::<ProduceUnitCommand>::default());
        let cmd = command(0, UnitType::Infantry);
        let mut state = ProductionTurnState::default();
        state.set_campaign_production_plan(PlayerId(1), 1, vec![cmd.clone()], true);
        let attempt = state
            .take_issuable_campaign_command(&mut world, PlayerId(1))
            .unwrap();
        assert_eq!(
            issue_production_command(&mut world, attempt),
            Some(format!("{cmd:?}"))
        );
        assert!(
            state
                .take_issuable_campaign_command(&mut world, PlayerId(1))
                .is_none()
        );
        assert!(world.resource::<Events<ProduceUnitCommand>>().is_empty());
        assert_eq!(funds_of(&world, PlayerId(1)), Some(5000));
        assert_eq!(world.entities().len(), 0);
        assert_eq!(
            world.resource::<IssueAudit>().rejected()[0].rejection,
            IssueRejection::GameOver
        );
    }

    #[test]
    fn rejected_queued_purchase_records_attempt_and_then_ends_turn() {
        use crate::ai::runtime::production_queue::{
            AiProductionCommandQueue, AiProductionCooldown,
        };
        use crate::ai::runtime::turn::execute_turn_step;
        let master = MasterDataRegistry::load().unwrap();
        let (mut world, _) =
            crate::setup::initialize_world_from_master_data(&master, "map_1").unwrap();
        let player = world.resource::<Players>().0[0].id;
        world.resource_mut::<Players>().0[0].funds = 185;
        let mut settings = crate::ai::PlayerAiSettings::default();
        settings.set_version(player, crate::ai::AiVersion::V3);
        world.insert_resource(settings);
        let site = world
            .query::<(&GridPosition, &Property)>()
            .iter(&world)
            .find(|(_, property)| {
                property.owner_id == Some(player) && property.terrain == Terrain::Capital
            })
            .map(|(position, _)| *position)
            .unwrap();
        let command = ProduceUnitCommand {
            player_id: player,
            target_x: site.x,
            target_y: site.y,
            unit_type: UnitType::Infantry,
        };
        world.insert_resource(AiProductionCommandQueue {
            player_id: Some(player),
            turn: 1,
            commands: VecDeque::from([command.clone()]),
        });
        let before = world.entities().len();
        // 拒否は終了ではなく、旧実装と同じ1件の購入試行として返す。
        assert_eq!(
            execute_turn_step(&mut world, player),
            Some(format!("{command:?}"))
        );
        assert!(world.resource::<Events<ProduceUnitCommand>>().is_empty());
        assert_eq!(funds_of(&world, player), Some(185));
        assert_eq!(world.entities().len(), before);
        assert!(
            world
                .resource::<AiProductionCooldown>()
                .0
                .contains(&(site.x, site.y))
        );
        assert!(matches!(
            world.resource::<IssueAudit>().rejected()[0].rejection,
            IssueRejection::InsufficientFunds { .. }
        ));
        assert!(execute_turn_step(&mut world, player).is_none());
        assert!(
            !world
                .resource::<Events<crate::events::NextPhaseCommand>>()
                .is_empty()
        );
    }
}
