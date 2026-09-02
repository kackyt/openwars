//! GB版AIの移動候補盤面に対応する一時データ。
//!
//! GB版は移動探索の結果をWRAMの盤面配列へ保存し、後続の行動判定がその配列を
//! 順に走査する。OpenWarsでは永続RAMを持たず、手番ごとに同じ意味の候補を生成する。

use crate::components::{GridPosition, PlayerId, UnitStats};
use crate::resources::{Map, MasterDataRegistry};
use crate::systems::movement::{OccupantInfo, calculate_reachable_tile_costs};
use std::collections::HashMap;

/// 一つの移動先候補。`movement_cost` はGB版の到達可能盤面の値に相当する。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CandidateTile {
    pub(crate) position: GridPosition,
    pub(crate) movement_cost: u32,
}

/// OpenWarsで合法な移動先だけから、GB版に対応する候補フィールドを構築する。
///
/// GB版と異なりOpenWarsではZOCが違法手を生むため、到達可能性の計算時点でZOCを
/// 適用する。以降のIQ別フィルタはこの集合を広げてはならない。
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_candidate_field(
    map: &Map,
    occupants: &HashMap<(usize, usize), OccupantInfo>,
    origin: GridPosition,
    stats: &UnitStats,
    fuel: u32,
    player_id: PlayerId,
    master_data: &MasterDataRegistry,
) -> Vec<CandidateTile> {
    let mut candidates: Vec<_> = calculate_reachable_tile_costs(
        map,
        occupants,
        (origin.x, origin.y),
        stats.movement_type,
        stats.max_movement,
        fuel,
        player_id,
        stats.unit_type,
        master_data,
    )
    .into_iter()
    .filter_map(|((x, y), movement_cost)| {
        // 自軍ユニットへの重なり移動は、輸送・合流以外の通常行動候補から除く。
        (GridPosition { x, y } == origin
            || occupants
                .get(&(x, y))
                .is_none_or(|occupant| occupant.player_id != player_id))
        .then_some(CandidateTile {
            position: GridPosition { x, y },
            movement_cost,
        })
    })
    .collect();

    // GB版は31列の作業盤面をアドレス昇順に走査するため、表示座標では行優先になる。
    // OpenWarsのBTreeMapは(x, y)順なので、ここでROMと同じ(y, x)順へ並べ直す。
    candidates.sort_by_key(|candidate| (candidate.position.y, candidate.position.x));
    candidates
}

/// V200の合流判定用に、同種の自軍ユニットが占有する到達可能マスだけを返す。
///
/// 通常行動の候補には自軍占有マスを混ぜず、ROMのIQ200分岐にある合流走査だけが
/// この関数を使う。到達可能性そのものは通常候補と同じZOC制約を適用する。
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_merge_candidate_field(
    map: &Map,
    occupants: &HashMap<(usize, usize), OccupantInfo>,
    origin: GridPosition,
    stats: &UnitStats,
    fuel: u32,
    player_id: PlayerId,
    master_data: &MasterDataRegistry,
) -> Vec<CandidateTile> {
    let mut candidates: Vec<_> = calculate_reachable_tile_costs(
        map,
        occupants,
        (origin.x, origin.y),
        stats.movement_type,
        stats.max_movement,
        fuel,
        player_id,
        stats.unit_type,
        master_data,
    )
    .into_iter()
    .filter_map(|((x, y), movement_cost)| {
        (GridPosition { x, y } != origin
            && occupants.get(&(x, y)).is_some_and(|occupant| {
                occupant.player_id == player_id && occupant.unit_type == stats.unit_type
            }))
        .then_some(CandidateTile {
            position: GridPosition { x, y },
            movement_cost,
        })
    })
    .collect();
    candidates.sort_by_key(|candidate| (candidate.position.y, candidate.position.x));
    candidates
}

/// 輸送ユニットへの搭載判定用に、搭載可能な自軍輸送ユニットのマスだけを返す。
///
/// 通常行動候補は自軍占有マスを除外するため、GB版の搭載命令に対応する走査を
/// 独立させる。到達可能性には通常候補と同じ地形・燃料・ZOC制約を適用する。
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_load_candidate_field(
    map: &Map,
    occupants: &HashMap<(usize, usize), OccupantInfo>,
    origin: GridPosition,
    stats: &UnitStats,
    fuel: u32,
    player_id: PlayerId,
    master_data: &MasterDataRegistry,
) -> Vec<CandidateTile> {
    let mut candidates: Vec<_> = calculate_reachable_tile_costs(
        map,
        occupants,
        (origin.x, origin.y),
        stats.movement_type,
        stats.max_movement,
        fuel,
        player_id,
        stats.unit_type,
        master_data,
    )
    .into_iter()
    .filter_map(|((x, y), movement_cost)| {
        let occupant = occupants.get(&(x, y))?;
        (GridPosition { x, y } != origin
            && occupant.player_id == player_id
            && occupant.is_transport
            && occupant.free_slots > 0
            && occupant.loadable_types.contains(&stats.unit_type))
        .then_some(CandidateTile {
            position: GridPosition { x, y },
            movement_cost,
        })
    })
    .collect();
    candidates.sort_by_key(|candidate| (candidate.position.y, candidate.position.x));
    candidates
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resources::master_data::UnitName;
    use crate::resources::{GridTopology, MovementType, Terrain, UnitType};

    #[test]
    fn candidate_field_uses_rom_row_major_order() {
        let map = Map::new(4, 4, Terrain::Plains, GridTopology::Hex);
        let master_data = MasterDataRegistry::load().unwrap();
        let stats = UnitStats {
            unit_type: UnitType::Infantry,
            movement_type: MovementType::Infantry,
            max_movement: 2,
            ..UnitStats::mock()
        };

        let candidates = build_candidate_field(
            &map,
            &HashMap::new(),
            GridPosition { x: 1, y: 1 },
            &stats,
            99,
            PlayerId(1),
            &master_data,
        );

        assert!(candidates.windows(2).all(|pair| {
            (pair[0].position.y, pair[0].position.x) <= (pair[1].position.y, pair[1].position.x)
        }));
    }

    #[test]
    fn merge_field_keeps_only_reachable_same_type_allies() {
        let map = Map::new(4, 4, Terrain::Plains, GridTopology::Hex);
        let master_data = MasterDataRegistry::load().unwrap();
        let stats = UnitStats {
            unit_type: UnitType::Infantry,
            movement_type: MovementType::Infantry,
            max_movement: 2,
            ..UnitStats::mock()
        };
        let mut occupants = HashMap::new();
        occupants.insert(
            (2, 1),
            OccupantInfo {
                player_id: PlayerId(1),
                is_transport: false,
                unit_type: UnitType::Infantry,
                loadable_types: Vec::new(),
                free_slots: 0,
            },
        );
        occupants.insert(
            (1, 2),
            OccupantInfo {
                player_id: PlayerId(1),
                is_transport: false,
                unit_type: UnitType::Recon,
                loadable_types: Vec::new(),
                free_slots: 0,
            },
        );

        let candidates = build_merge_candidate_field(
            &map,
            &occupants,
            GridPosition { x: 1, y: 1 },
            &stats,
            99,
            PlayerId(1),
            &master_data,
        );

        assert_eq!(
            candidates,
            vec![CandidateTile {
                position: GridPosition { x: 2, y: 1 },
                movement_cost: 1,
            }]
        );
    }

    #[test]
    fn load_field_keeps_reachable_transport_but_not_other_allies() {
        let map = Map::new(4, 4, Terrain::Plains, GridTopology::Hex);
        let master_data = MasterDataRegistry::load().unwrap();
        let stats = UnitStats {
            unit_type: UnitType::Infantry,
            movement_type: MovementType::Infantry,
            max_movement: 2,
            ..UnitStats::mock()
        };
        let mut occupants = HashMap::new();
        occupants.insert(
            (2, 1),
            OccupantInfo {
                player_id: PlayerId(1),
                is_transport: true,
                unit_type: UnitType::TransportHelicopter,
                loadable_types: vec![UnitType::Infantry],
                free_slots: 1,
            },
        );
        occupants.insert(
            (1, 2),
            OccupantInfo {
                player_id: PlayerId(1),
                is_transport: false,
                unit_type: UnitType::Infantry,
                loadable_types: Vec::new(),
                free_slots: 0,
            },
        );

        let candidates = build_load_candidate_field(
            &map,
            &occupants,
            GridPosition { x: 1, y: 1 },
            &stats,
            99,
            PlayerId(1),
            &master_data,
        );

        assert_eq!(
            candidates,
            vec![CandidateTile {
                position: GridPosition { x: 2, y: 1 },
                movement_cost: 1,
            }]
        );
    }

    #[test]
    fn map1_white_armored_car_uses_legal_zoc_fallback() {
        let master_data = MasterDataRegistry::load().unwrap();
        let (world, _) = crate::setup::initialize_world_from_master_data_with_topology(
            &master_data,
            "map_1",
            GridTopology::Hex,
        )
        .unwrap();
        let map = world.resource::<Map>().clone();
        let stats = master_data
            .create_unit_stats(&UnitName(UnitType::Recon.as_str().to_owned()))
            .unwrap();
        let mut occupants = HashMap::new();
        for position in [
            (3, 7),
            (5, 7),
            (7, 6),
            (5, 6),
            (4, 5),
            (1, 7),
            (7, 7),
            (2, 7),
            (8, 8),
            (6, 8),
        ] {
            occupants.insert(
                position,
                OccupantInfo {
                    player_id: PlayerId(1),
                    is_transport: false,
                    unit_type: UnitType::Infantry,
                    loadable_types: Vec::new(),
                    free_slots: 0,
                },
            );
        }

        let candidates = build_candidate_field(
            &map,
            &occupants,
            GridPosition { x: 3, y: 11 },
            &stats,
            70,
            PlayerId(2),
            &master_data,
        );
        let positions: std::collections::HashSet<_> = candidates
            .iter()
            .map(|candidate| candidate.position)
            .collect();

        // GB版IQ100の選択 (4, 7) とIQ200の選択 (6, 7) は、途中で敵ZOCへ入り、
        // さらに進む必要があるためOpenWarsでは違法。
        assert!(!positions.contains(&GridPosition { x: 4, y: 7 }));
        assert!(!positions.contains(&GridPosition { x: 6, y: 7 }));
        // 現在のV100/V200が選ぶ攻撃位置は、ZOCへ入った所で停止する合法候補として残る。
        assert!(positions.contains(&GridPosition { x: 7, y: 9 }));
        assert!(positions.contains(&GridPosition { x: 6, y: 9 }));
    }

    #[test]
    fn island_transport_pickup_does_not_ignore_forest_cost() {
        let master_data = MasterDataRegistry::load().unwrap();
        let (world, _) = crate::setup::initialize_world_from_master_data_with_topology(
            &master_data,
            "map_3",
            GridTopology::Hex,
        )
        .unwrap();
        let map = world.resource::<Map>().clone();
        let stats = master_data
            .create_unit_stats(&UnitName(UnitType::Infantry.as_str().to_owned()))
            .unwrap();
        let target = GridPosition { x: 20, y: 23 };
        let occupants = HashMap::from([(
            (target.x, target.y),
            OccupantInfo {
                player_id: PlayerId(2),
                is_transport: true,
                unit_type: UnitType::TransportHelicopter,
                loadable_types: vec![UnitType::Infantry],
                free_slots: 2,
            },
        )]);

        for origin in [GridPosition { x: 23, y: 24 }, GridPosition { x: 23, y: 23 }] {
            let candidates = build_load_candidate_field(
                &map,
                &occupants,
                origin,
                &stats,
                99,
                PlayerId(2),
                &master_data,
            );
            assert!(
                candidates
                    .iter()
                    .all(|candidate| candidate.position != target),
                "OpenWarsの森移動コストを無視してGB版の搭載数だけ合わせてはならない"
            );
        }

        let legal_alternative = build_load_candidate_field(
            &map,
            &occupants,
            GridPosition { x: 22, y: 22 },
            &stats,
            99,
            PlayerId(2),
            &master_data,
        );
        assert!(
            legal_alternative
                .iter()
                .any(|candidate| candidate.position == target),
            "GB版と同じ搭載命令はOpenWarsで到達可能な近傍歩兵へ割り当て直せる"
        );
    }

    #[test]
    fn island_transport_pickup_keeps_legal_alternatives() {
        let master_data = MasterDataRegistry::load().unwrap();
        let (world, _) = crate::setup::initialize_world_from_master_data_with_topology(
            &master_data,
            "map_3",
            GridTopology::Hex,
        )
        .unwrap();
        let map = world.resource::<Map>().clone();
        let stats = master_data
            .create_unit_stats(&UnitName(UnitType::Infantry.as_str().to_owned()))
            .unwrap();
        let target = GridPosition { x: 19, y: 23 };
        let occupants = HashMap::from([(
            (target.x, target.y),
            OccupantInfo {
                player_id: PlayerId(2),
                is_transport: true,
                unit_type: UnitType::TransportHelicopter,
                loadable_types: vec![UnitType::Infantry],
                free_slots: 2,
            },
        )]);

        for origin in [GridPosition { x: 21, y: 22 }, GridPosition { x: 21, y: 21 }] {
            let candidates = build_load_candidate_field(
                &map,
                &occupants,
                origin,
                &stats,
                99,
                PlayerId(2),
                &master_data,
            );
            assert!(
                candidates
                    .iter()
                    .any(|candidate| candidate.position == target),
                "搭載数を再現する代替歩兵もOpenWarsの移動規則で到達可能でなければならない"
            );
        }
    }
}
