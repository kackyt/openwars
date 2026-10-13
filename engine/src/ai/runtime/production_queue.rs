//! AI再設計: 責務ごとに分離した共通処理。
#![allow(clippy::collapsible_if)]
#![allow(clippy::unnecessary_min_or_max)]
#![allow(clippy::unnecessary_map_or)]
use crate::collections::HashSet;
use crate::components::PlayerId;
use bevy_ecs::prelude::*;
use std::collections::VecDeque;

#[derive(Resource, Default)]
pub struct AiProductionCooldown(pub HashSet<(usize, usize)>);

/// 同じ手番に一度作った複数施設分の生産命令を順番に発行します。
/// 生産計画は予算と使用施設を全命令分シミュレート済みなので、1体ごとに盤面分析から
/// やり直さず、敵が行動しない同一生産フェーズでは計画列をそのまま消費します。
#[derive(Resource, Default)]
pub(crate) struct AiProductionCommandQueue {
    pub(crate) player_id: Option<PlayerId>,
    pub(crate) turn: u32,
    pub(crate) commands: VecDeque<crate::events::ProduceUnitCommand>,
}

/// 現在手番の一括生産計画から、まだ試行していない次の命令だけを取り出す。
/// 生産済みunitは同じ手番に行動できないため、キューが残っている間は通常行動を
/// 再探索せずとも、前回の「全行動候補なし」という判定を安全に引き継げる。
pub(crate) fn take_next_queued_production_command(
    world: &mut World,
    player_id: PlayerId,
    turn: u32,
) -> Option<crate::events::ProduceUnitCommand> {
    let cooldown = world
        .get_resource::<AiProductionCooldown>()
        .map(|resource| resource.0.clone())
        .unwrap_or_default();
    let (last_error, last_event) = world
        .get_resource::<crate::resources::ProductionDiagnostic>()
        .map(|diagnostic| (diagnostic.last_error.clone(), diagnostic.last_event.clone()))
        .unwrap_or_default();
    let mut queue = world
        .remove_resource::<AiProductionCommandQueue>()
        .unwrap_or_default();
    if queue.player_id != Some(player_id) || queue.turn != turn {
        queue.player_id = Some(player_id);
        queue.turn = turn;
        queue.commands.clear();
    }

    let mut command = None;
    while let Some(candidate) = queue.commands.pop_front() {
        let candidate_text = format!("{:?}", candidate);
        if cooldown.contains(&(candidate.target_x, candidate.target_y))
            || last_error.is_some() && last_event.as_deref() == Some(candidate_text.as_str())
        {
            continue;
        }
        command = Some(candidate);
        break;
    }
    world.insert_resource(queue);
    command
}

pub(crate) fn issue_production_command(
    world: &mut World,
    command: crate::events::ProduceUnitCommand,
) -> Option<String> {
    let command_text = format!("{:?}", command);
    // 旧ログは資金不足・施設喪失・終局後も「購入を試みた案」を含む。
    // 無効な案をイベントへ渡さず、監査と試行済み施設だけを記録して列を進める。
    // ここでNoneを返すと、呼出側は終了イベントのない手番を終わったと誤認する。
    let mut audit_only = false;
    if let Err(rejection) = crate::ai::production::commit::validate_issue(world, &command) {
        audit_only = matches!(
            rejection,
            crate::ai::production::commit::IssueRejection::GameOver
                | crate::ai::production::commit::IssueRejection::InsufficientFunds { .. }
                | crate::ai::production::commit::IssueRejection::FacilityUnavailable(_)
                | crate::ai::production::commit::IssueRejection::UnknownUnitType
        );
        let turn = world
            .get_resource::<crate::resources::MatchState>()
            .map_or(0, |state| state.current_turn_number.0);
        world
            .get_resource_or_insert_with(crate::ai::production::commit::IssueAudit::default)
            .record(turn, command.clone(), rejection);
        if !audit_only {
            return None;
        }
    }
    if let Some(mut cooldown) = world.get_resource_mut::<AiProductionCooldown>() {
        cooldown.0.insert((command.target_x, command.target_y));
    } else {
        world.insert_resource(AiProductionCooldown(HashSet::from_iter([(
            command.target_x,
            command.target_y,
        )])));
    }
    if audit_only {
        return Some(command_text);
    }
    world
        .get_resource_mut::<Events<crate::events::ProduceUnitCommand>>()?
        .send(command);
    Some(command_text)
}
