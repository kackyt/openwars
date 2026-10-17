//! 勝利Roadmapのaudit責務。

use super::{
    CampaignStepKind, EventReader, GridPosition, IslandMap, MatchState,
    PropertyCaptureProgressedEvent, PropertyCapturedEvent, Res, ResMut, UnitAttackedEvent,
    UnitDestroyedEvent, UnitLoadedEvent, UnitMovedEvent, UnitSuppliedEvent, UnitUnloadedEvent,
    UnitWaitedEvent, VictoryRoadmapRegistry,
};

/// action result Eventを、担当EntityのStrategicOperationへ照合する。
#[allow(clippy::too_many_arguments)]
pub(crate) fn audit_victory_roadmap_system(
    match_state: Res<MatchState>,
    island_map: Option<Res<IslandMap>>,
    mut moved: EventReader<UnitMovedEvent>,
    mut attacked: EventReader<UnitAttackedEvent>,
    mut loaded: EventReader<UnitLoadedEvent>,
    mut unloaded: EventReader<UnitUnloadedEvent>,
    mut capture_progressed: EventReader<PropertyCaptureProgressedEvent>,
    mut captured: EventReader<PropertyCapturedEvent>,
    mut supplied: EventReader<UnitSuppliedEvent>,
    mut waited: EventReader<UnitWaitedEvent>,
    mut destroyed: EventReader<UnitDestroyedEvent>,
    mut registry: ResMut<VictoryRoadmapRegistry>,
    campaign_execution: Option<
        ResMut<crate::ai::v4::campaign_execution::V4CampaignExecutionRegistry>,
    >,
    operation_assignments: Option<ResMut<crate::ai::operation_assignment::UnitOperationRegistry>>,
) {
    let turn = match_state.current_turn_number.0;
    for event in moved.read() {
        if let Some(island_map) = island_map.as_deref() {
            registry.record_move(event.entity, event.from, event.to, turn, island_map);
        } else {
            registry.observe_planned_move(event.entity, event.to, turn);
        }
    }
    for event in attacked.read() {
        registry.complete_planned_step(
            event.attacker,
            turn,
            CampaignStepKind::Attack,
            None,
            Some(event.defender),
        );
    }
    for event in loaded.read() {
        registry
            .transport_manifests
            .entry(event.transport)
            .or_default()
            .insert(event.cargo);
        if !registry.complete_planned_step(
            event.cargo,
            turn,
            CampaignStepKind::Load,
            None,
            Some(event.transport),
        ) {
            registry.complete_planned_step(
                event.transport,
                turn,
                CampaignStepKind::Load,
                None,
                Some(event.cargo),
            );
        }
    }
    for event in unloaded.read() {
        if let Some(manifest) = registry.transport_manifests.get_mut(&event.transport) {
            manifest.remove(&event.cargo);
        }
        if !registry.complete_planned_step(
            event.transport,
            turn,
            CampaignStepKind::Drop,
            None,
            Some(event.cargo),
        ) {
            registry.complete_planned_step(
                event.cargo,
                turn,
                CampaignStepKind::Drop,
                None,
                Some(event.transport),
            );
        }
    }
    for event in capture_progressed.read() {
        let matched = registry.complete_planned_step(
            event.unit,
            turn,
            CampaignStepKind::Capture,
            Some(GridPosition {
                x: event.x,
                y: event.y,
            }),
            None,
        );
        if matched
            && event.completed
            && let Some(binding) = registry.entity_bindings.get(&event.unit).copied()
            && let Some(operation) = registry.operations.get_mut(&binding.operation_id)
        {
            operation.execution.completed_captures =
                operation.execution.completed_captures.saturating_add(1);
        }
    }
    for event in captured.read() {
        let position = GridPosition {
            x: event.x,
            y: event.y,
        };
        // PropertyCapturedEventには実行Entityが無いため、これ単独では作戦進捗にしない。
        // 対応するPropertyCaptureProgressedEventを照合済みの場合だけ所有数の再計測で反映する。
        let _belongs_to_active_operation = registry.operations.values().any(|operation| {
            operation.active && operation.objective_properties.contains(&position)
        });
    }
    for event in supplied.read() {
        registry.complete_planned_step(
            event.supplier,
            turn,
            CampaignStepKind::Supply,
            None,
            Some(event.target),
        );
    }
    for event in waited.read() {
        registry.complete_planned_step(event.entity, turn, CampaignStepKind::Wait, None, None);
    }
    let mut campaign_execution = campaign_execution;
    let mut operation_assignments = operation_assignments;
    for event in destroyed.read() {
        if let Some(execution) = campaign_execution.as_deref_mut() {
            execution.mark_destroyed(event.entity, turn);
        }
        if let Some(assignments) = operation_assignments.as_deref_mut() {
            crate::ai::planning::assignment::OperationLedger::open(assignments)
                .release_destroyed(event.entity);
        }
        registry.record_destroyed_entity(event.entity, turn);
    }
}
