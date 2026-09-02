//! GB版の固定長ユニット記録順をECS上で再現する。
//!
//! Bevyの`Entity`番号は生成実装の都合で変わり得るため、ROMが走査に使う
//! 「生産された順番」と同一視しない。生産命令を発行した時点で通し番号を予約し、
//! 次の行動フェーズで実際に生成されたEntityへ結び付ける。

use crate::components::{GridPosition, PlayerId};
use crate::events::ProduceUnitCommand;
use crate::resources::UnitType;
use bevy_ecs::prelude::*;
use std::collections::{HashMap, HashSet};

#[derive(Clone, Copy)]
struct PendingRecord {
    player_id: PlayerId,
    position: GridPosition,
    unit_type: UnitType,
    order: u32,
}

#[derive(Resource, Default)]
struct UnitRecordState {
    by_player: HashMap<PlayerId, HashMap<Entity, u32>>,
    pending: Vec<PendingRecord>,
    next_by_player: HashMap<PlayerId, u32>,
}

/// 生産命令の発行順を、ROMのユニット記録番号として予約する。
pub(crate) fn reserve_production_record(world: &mut World, command: &ProduceUnitCommand) {
    let mut state = world.get_resource_or_insert_with(UnitRecordState::default);
    let next = state.next_by_player.entry(command.player_id).or_default();
    let order = *next;
    *next = next.saturating_add(1);
    state.pending.push(PendingRecord {
        player_id: command.player_id,
        position: GridPosition {
            x: command.target_x,
            y: command.target_y,
        },
        unit_type: command.unit_type,
        order,
    });
}

/// 生成済みEntityを予約済み記録へ結び付け、現在の記録順を返す。
///
/// セーブデータなど予約を持たない盤面では、初回だけEntity順を代替値として採用する。
/// 以後は移動や搭載によって座標が変わっても番号を保持する。
pub(crate) fn synchronize_unit_records(
    world: &mut World,
    player_id: PlayerId,
    observed: &[(Entity, GridPosition, UnitType)],
) -> HashMap<Entity, u32> {
    let alive: HashSet<_> = observed.iter().map(|(entity, _, _)| *entity).collect();
    let mut state = world.get_resource_or_insert_with(UnitRecordState::default);
    let mut by_entity = state.by_player.remove(&player_id).unwrap_or_default();
    by_entity.retain(|entity, _| alive.contains(entity));

    for (entity, position, unit_type) in observed {
        if by_entity.contains_key(entity) {
            continue;
        }
        let pending_index = state.pending.iter().position(|record| {
            record.player_id == player_id
                && record.position == *position
                && record.unit_type == *unit_type
        });
        if let Some(index) = pending_index {
            let record = state.pending.remove(index);
            by_entity.insert(*entity, record.order);
        }
    }

    let mut unbound: Vec<_> = observed
        .iter()
        .filter(|(entity, _, _)| !by_entity.contains_key(entity))
        .map(|(entity, _, _)| *entity)
        .collect();
    unbound.sort_by_key(|entity| entity.index());
    for entity in unbound {
        let next = state.next_by_player.entry(player_id).or_default();
        by_entity.insert(entity, *next);
        *next = next.saturating_add(1);
    }

    let result = observed
        .iter()
        .filter_map(|(entity, _, _)| by_entity.get(entity).copied().map(|order| (*entity, order)))
        .collect();
    state.by_player.insert(player_id, by_entity);
    result
}

/// 輸送判断など、行動本体とは別のROM走査でも同じ記録順を参照する。
pub(crate) fn record_order(world: &World, entity: Entity) -> u32 {
    world
        .get_resource::<UnitRecordState>()
        .and_then(|state| {
            state
                .by_player
                .values()
                .find_map(|records| records.get(&entity))
        })
        .copied()
        .unwrap_or_else(|| entity.index())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn production_order_is_independent_from_entity_number() {
        let mut world = World::new();
        let first_command = ProduceUnitCommand {
            player_id: PlayerId(1),
            target_x: 4,
            target_y: 3,
            unit_type: UnitType::Infantry,
        };
        let second_command = ProduceUnitCommand {
            player_id: PlayerId(1),
            target_x: 5,
            target_y: 3,
            unit_type: UnitType::Recon,
        };
        reserve_production_record(&mut world, &first_command);
        reserve_production_record(&mut world, &second_command);

        let first_entity = Entity::from_raw(20);
        let second_entity = Entity::from_raw(3);
        let orders = synchronize_unit_records(
            &mut world,
            PlayerId(1),
            &[
                (second_entity, GridPosition { x: 5, y: 3 }, UnitType::Recon),
                (
                    first_entity,
                    GridPosition { x: 4, y: 3 },
                    UnitType::Infantry,
                ),
            ],
        );

        assert_eq!(orders[&first_entity], 0);
        assert_eq!(orders[&second_entity], 1);
    }
}
