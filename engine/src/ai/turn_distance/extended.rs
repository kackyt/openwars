//! 拡張経路（小規模マップ戦略・非V4）の実装。
//!
//! 親モジュールはmain互換の実装を持ち、本文の異なる関数は
//! `crate::ai::code_path::is_extended()` でここへ委譲する。

#[allow(unused_imports)]
use super::*;

/// 実ゲームの移動力繰越禁止・燃料消費・移動後間接攻撃禁止を考慮し、
/// 指定射程帯から攻撃可能になるまでの最短行動ターンを返します。
#[allow(clippy::too_many_arguments)]
pub(crate) fn calculate_action_distance_to_range(
    map: &Map,
    registry: &MasterDataRegistry,
    unit_positions: &HashMap<(usize, usize), OccupantInfo>,
    start: (usize, usize),
    target: (usize, usize),
    movement_type: MovementType,
    max_mp: u32,
    max_fuel: u32,
    interaction_min_range: u32,
    interaction_max_range: u32,
    player_id: PlayerId,
    cache: &mut ActionTurnDistanceCache,
) -> Option<ActionTurnDistance> {
    if !crate::ai::code_path::ext_enabled("turn_distance.rs::calculate_action_distance_to_range") {
        return super::calculate_action_distance_to_range(
            map,
            registry,
            unit_positions,
            start,
            target,
            movement_type,
            max_mp,
            max_fuel,
            interaction_min_range,
            interaction_max_range,
            player_id,
            cache,
        );
    }
    calculate_action_distance_to_range_with_origin_policy(
        map,
        registry,
        unit_positions,
        start,
        target,
        movement_type,
        max_mp,
        max_fuel,
        interaction_min_range,
        interaction_max_range,
        player_id,
        false,
        cache,
    )
}

/// 生産施設から生まれた間接攻撃unitが、次手番の生産口を譲るために一度退避してから
/// 射撃可能になるまでの最短行動ターンを返す。
#[allow(clippy::too_many_arguments)]
pub(crate) fn calculate_action_distance_to_range_after_leaving_start(
    map: &Map,
    registry: &MasterDataRegistry,
    unit_positions: &HashMap<(usize, usize), OccupantInfo>,
    start: (usize, usize),
    target: (usize, usize),
    movement_type: MovementType,
    max_mp: u32,
    max_fuel: u32,
    interaction_min_range: u32,
    interaction_max_range: u32,
    player_id: PlayerId,
    cache: &mut ActionTurnDistanceCache,
) -> Option<ActionTurnDistance> {
    calculate_action_distance_to_range_with_origin_policy(
        map,
        registry,
        unit_positions,
        start,
        target,
        movement_type,
        max_mp,
        max_fuel,
        interaction_min_range,
        interaction_max_range,
        player_id,
        true,
        cache,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn calculate_action_distance_to_range_with_origin_policy(
    map: &Map,
    registry: &MasterDataRegistry,
    unit_positions: &HashMap<(usize, usize), OccupantInfo>,
    start: (usize, usize),
    target: (usize, usize),
    movement_type: MovementType,
    max_mp: u32,
    max_fuel: u32,
    interaction_min_range: u32,
    interaction_max_range: u32,
    player_id: PlayerId,
    must_leave_start: bool,
    cache: &mut ActionTurnDistanceCache,
) -> Option<ActionTurnDistance> {
    super::calculate_action_distance_with_policy(
        map,
        registry,
        unit_positions,
        start,
        target,
        movement_type,
        max_mp,
        max_fuel,
        interaction_min_range,
        interaction_max_range,
        player_id,
        super::normalize_occupancy(unit_positions),
        super::ActionDistancePolicy {
            respect_enemy_zoc: true,
            coordinate_tiebreaker: true,
            must_leave_start,
        },
        cache,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resources::Terrain;

    #[test]
    fn action_distance_respects_turn_movement_boundaries() {
        let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
        let map = Map::new(
            6,
            1,
            Terrain::Forest,
            crate::resources::GridTopology::Square,
        );
        let registry = MasterDataRegistry::load().unwrap_or_default();
        let mut cache = ActionTurnDistanceCache::default();

        let distance = calculate_action_distance_to_range(
            &map,
            &registry,
            &HashMap::new(),
            (0, 0),
            (5, 0),
            MovementType::Tank,
            5,
            99,
            0,
            0,
            PlayerId(1),
            &mut cache,
        )
        .unwrap();

        assert_eq!(distance.turns, 3);
        assert_eq!(distance.used_mp, 10);
        assert_eq!(distance.used_fuel, 5);
        assert_eq!(distance.firing_position, GridPosition { x: 5, y: 0 });
    }

    #[test]
    fn action_distance_reports_selected_firing_position() {
        let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
        let map = Map::new(
            5,
            1,
            Terrain::Plains,
            crate::resources::GridTopology::Square,
        );
        let registry = MasterDataRegistry::load().unwrap_or_default();
        let mut cache = ActionTurnDistanceCache::default();

        let distance = calculate_action_distance_to_range(
            &map,
            &registry,
            &HashMap::new(),
            (0, 0),
            (4, 0),
            MovementType::Tank,
            3,
            99,
            1,
            1,
            PlayerId(1),
            &mut cache,
        )
        .unwrap();

        assert_eq!(distance.turns, 1);
        assert_eq!(distance.firing_position, GridPosition { x: 3, y: 0 });
        assert!(distance.requires_movement);
    }

    #[test]
    fn action_distance_enemy_zoc_ends_only_current_turn() {
        let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
        let map = Map::new(
            5,
            2,
            Terrain::Plains,
            crate::resources::GridTopology::Square,
        );
        let registry = MasterDataRegistry::load().unwrap_or_default();
        let mut cache = ActionTurnDistanceCache::default();
        let enemy = OccupantInfo {
            player_id: PlayerId(2),
            unit_type: crate::resources::UnitType::Infantry,
            is_transport: false,
            free_slots: 0,
            loadable_types: Vec::new(),
        };
        let unit_positions = HashMap::from([((1, 1), enemy)]);

        let distance = calculate_action_distance_to_range(
            &map,
            &registry,
            &unit_positions,
            (0, 0),
            (4, 0),
            MovementType::Tank,
            4,
            99,
            0,
            0,
            PlayerId(1),
            &mut cache,
        )
        .unwrap();

        // (1, 0) は敵 (1, 1) のZOC。T1はそこで停止し、T2に残り3マスを進む。
        assert_eq!(distance.turns, 2);
        assert_eq!(distance.used_mp, 4);
        assert_eq!(distance.used_fuel, 4);
        assert_eq!(distance.firing_position, GridPosition { x: 4, y: 0 });
    }

    #[test]
    fn action_distance_after_leaving_start_counts_factory_vacation_setup_turn() {
        let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
        let map = Map::new(
            5,
            2,
            Terrain::Plains,
            crate::resources::GridTopology::Square,
        );
        let registry = MasterDataRegistry::load().unwrap_or_default();
        let mut cache = ActionTurnDistanceCache::default();

        let distance = calculate_action_distance_to_range_after_leaving_start(
            &map,
            &registry,
            &HashMap::new(),
            (0, 0),
            (3, 0),
            MovementType::Tank,
            1,
            1,
            1,
            3,
            PlayerId(1),
            &mut cache,
        )
        .expect("生産地点の外にも合法な射撃地点がある");

        assert!(distance.requires_movement);
        assert_ne!(distance.firing_position, GridPosition { x: 0, y: 0 });
        assert_eq!(distance.turns, 2);
    }

    #[test]
    fn action_distance_cache_distinguishes_occupancy() {
        let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
        let map = Map::new(
            5,
            1,
            Terrain::Plains,
            crate::resources::GridTopology::Square,
        );
        let registry = MasterDataRegistry::load().unwrap_or_default();
        let empty = HashMap::new();
        let blocked = HashMap::from([(
            (2, 0),
            OccupantInfo {
                player_id: PlayerId(2),
                unit_type: crate::resources::UnitType::Infantry,
                is_transport: false,
                free_slots: 0,
                loadable_types: Vec::new(),
            },
        )]);
        // 同一目標のキャッシュと、異なる目標へ共有する始点キャッシュの両方を検証する。
        for targets in [[4, 4], [3, 4]] {
            for blocked_first in [false, true] {
                let mut cache = ActionTurnDistanceCache::default();
                for (index, is_blocked) in [blocked_first, !blocked_first].into_iter().enumerate() {
                    let result = calculate_action_distance_to_range(
                        &map,
                        &registry,
                        if is_blocked { &blocked } else { &empty },
                        (0, 0),
                        (targets[index], 0),
                        MovementType::Infantry,
                        3,
                        99,
                        1,
                        1,
                        PlayerId(1),
                        &mut cache,
                    );
                    assert_eq!(result.is_none(), is_blocked);
                }
            }
        }
    }
}
