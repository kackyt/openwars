//! 都市防衛ロジックの単体・局面テスト（反例ケース網羅）。

use super::city_defense::plan_city_defense;
use crate::collections::HashSet;
use crate::components::GridPosition;
use crate::resources::master_data::MasterDataRegistry;
use crate::resources::{GridTopology, Map, Terrain};

fn test_registry() -> MasterDataRegistry {
    MasterDataRegistry::load().unwrap()
}

#[test]
fn single_chokepoint_bridge_is_selected() {
    let master = test_registry();
    // 歩兵は川を渡れるため、橋が唯一の地上通路になるよう海峡で分ける。
    let mut map = Map::new(7, 5, Terrain::Plains, GridTopology::Square);
    for y in 0..5 {
        map.set_terrain(3, y, Terrain::Sea).unwrap();
    }
    map.set_terrain(3, 2, Terrain::Bridge).unwrap();

    let city = GridPosition { x: 1, y: 2 };
    let enemies = vec![(GridPosition { x: 5, y: 2 }, 3)];
    let occupied = HashSet::default();
    let indirect_ranges = vec![(2, 3)]; // 局面の仮想射程。兵種の射程は固定しない。

    let plan = plan_city_defense(
        &map,
        &master,
        city,
        &enemies,
        1, // 直接部隊 1体
        &indirect_ranges,
        &occupied,
    )
    .expect("防衛計画が生成されること");

    // 海峡の橋またはその隣の出口を塞げば、都市へ至る地上経路を遮断できる。
    // より敵側の出口を選ぶ実装も、同じ遮断条件を満たすなら許す。
    assert_eq!(plan.chokepoints.len(), 1);
    assert!(matches!(
        plan.chokepoints[0],
        GridPosition { x: 3 | 4, y: 2 }
    ));
    assert!(
        plan.is_fully_blocked,
        "海峡を経由する地上経路を完全遮断する"
    );
    assert_eq!(
        plan.support_city,
        Some(city),
        "都市から距離2の橋を射程内に収めるため、都市が支援位置になる"
    );
}

#[test]
fn counterexample_flanking_bypass_prevents_insufficient_chokepoint() {
    let master = test_registry();
    // 7x5 マップ。都市は (1, 2)。x=3 の y=0..4 が川だが、y=4 だけ平地（迂回路）。
    // (3, 2) は橋。敵は (5, 2)。
    let mut map = Map::new(7, 5, Terrain::Plains, GridTopology::Square);
    for y in 0..4 {
        map.set_terrain(3, y, Terrain::River).unwrap();
    }
    map.set_terrain(3, 2, Terrain::Bridge).unwrap();
    // y=4 は平地のまま（迂回路が存在）

    let city = GridPosition { x: 1, y: 2 };
    let enemies = vec![(GridPosition { x: 5, y: 2 }, 3)];
    let occupied = HashSet::default();

    // 直接部隊が1体だけの場合、橋(3, 2)だけ塞いでもy=4から迂回される
    let plan = plan_city_defense(&map, &master, city, &enemies, 1, &[], &occupied)
        .expect("防衛計画が生成されること");

    // 1体では完全遮断できない
    assert!(!plan.is_fully_blocked);
}

#[test]
fn multiple_entrances_are_all_selected_when_units_available() {
    let master = test_registry();
    // 5x5 マップ。都市は (2, 2)。
    // 北 (2, 0) と 東 (4, 2) に敵がいる。
    // 北からの侵入と東からの侵入の2つの入口がある。
    let map = Map::new(5, 5, Terrain::Plains, GridTopology::Square);
    let city = GridPosition { x: 2, y: 2 };
    let enemies = vec![
        (GridPosition { x: 2, y: 0 }, 2),
        (GridPosition { x: 4, y: 2 }, 2),
    ];
    let occupied = HashSet::default();

    let plan = plan_city_defense(&map, &master, city, &enemies, 2, &[], &occupied)
        .expect("防衛計画が生成されること");

    assert_eq!(plan.chokepoints.len(), 2, "2箇所の入口が選定されること");
    // 都市自身 (2, 2) は絶対に選ばれない
    assert!(
        !plan.chokepoints.contains(&city),
        "都市自身は封鎖地点に選ばれない"
    );
}

#[test]
fn enemy_already_in_the_only_available_ring_does_not_count_as_a_friendly_screen() {
    let master = test_registry();
    // 入口が2つあるが直接部隊が1体しかない場合
    let map = Map::new(5, 5, Terrain::Plains, GridTopology::Square);
    let city = GridPosition { x: 2, y: 2 };
    // 北の敵は距離1 (2, 1)、東の敵は距離2 (4, 2)
    let enemies = vec![
        (GridPosition { x: 2, y: 1 }, 2),
        (GridPosition { x: 4, y: 2 }, 2),
    ];
    let occupied = HashSet::default();

    let plan = plan_city_defense(&map, &master, city, &enemies, 1, &[], &occupied);
    assert!(
        plan.is_none_or(|plan| {
            !plan.is_fully_blocked
                && plan
                    .chokepoints
                    .iter()
                    .all(|cell| !enemies.iter().any(|(enemy, _)| enemy == cell))
        }),
        "敵自身が占めるマスを友軍の封鎖地点として数えない"
    );
}

#[test]
fn counterexample_enclosed_unreachable_area_is_ignored() {
    let master = test_registry();
    // 都市の西側は海で囲まれており、敵は東側からしか来られない
    let mut map = Map::new(5, 5, Terrain::Plains, GridTopology::Square);
    for y in 0..5 {
        map.set_terrain(0, y, Terrain::Sea).unwrap();
        map.set_terrain(1, y, Terrain::Sea).unwrap();
    }
    let city = GridPosition { x: 2, y: 2 };
    let enemies = vec![(GridPosition { x: 4, y: 2 }, 2)];
    let occupied = HashSet::default();

    let plan = plan_city_defense(&map, &master, city, &enemies, 1, &[], &occupied)
        .expect("防衛計画が生成されること");

    // 海のある西側（x <= 1）のセルは一切選ばれず、東側のみ選ばれること
    for cp in &plan.chokepoints {
        assert!(cp.x >= 2, "海側の到達不能セルは選ばれない");
    }
}

#[test]
fn city_itself_is_never_selected_as_screen() {
    let master = test_registry();
    let map = Map::new(5, 5, Terrain::Plains, GridTopology::Square);
    let city = GridPosition { x: 2, y: 2 };
    let enemies = vec![(GridPosition { x: 2, y: 4 }, 2)];
    let occupied = HashSet::default();

    let plan = plan_city_defense(&map, &master, city, &enemies, 1, &[], &occupied)
        .expect("防衛計画が生成されること");

    assert_ne!(
        plan.chokepoints.first(),
        Some(&city),
        "都市自身を遮蔽地点として選択しない"
    );
}

#[test]
fn occupied_or_repair_city_is_respected_for_indirect_support() {
    let master = test_registry();
    let mut map = Map::new(7, 5, Terrain::Plains, GridTopology::Square);
    for y in 0..5 {
        map.set_terrain(3, y, Terrain::River).unwrap();
    }
    map.set_terrain(3, 2, Terrain::Bridge).unwrap();

    let city = GridPosition { x: 1, y: 2 };
    let enemies = vec![(GridPosition { x: 5, y: 2 }, 3)];
    let mut occupied = HashSet::default();
    occupied.insert(city); // 都市が修理・他部隊で占有されている
    let indirect_ranges = vec![(2, 3)];

    let plan = plan_city_defense(
        &map,
        &master,
        city,
        &enemies,
        1,
        &indirect_ranges,
        &occupied,
    )
    .expect("防衛計画が生成されること");

    assert_eq!(
        plan.support_city, None,
        "占有または回復予約中の都市は間接部隊の支援位置に選ばれない"
    );
}

/// 直接部隊がいないのに、仮想的な封鎖線や、その支援都市を確保できたと扱わない。
#[test]
fn no_direct_units_do_not_create_a_fictitious_screen() {
    let master = test_registry();
    let map = Map::new(7, 1, Terrain::Plains, GridTopology::Square);
    let city = GridPosition { x: 1, y: 0 };
    let enemies = [(GridPosition { x: 6, y: 0 }, 2)];
    if let Some(plan) = plan_city_defense(
        &map,
        &master,
        city,
        &enemies,
        0,
        &[(2, 3)],
        &HashSet::default(),
    ) {
        assert!(plan.chokepoints.is_empty());
        assert!(!plan.is_fully_blocked);
        assert!(plan.support_city.is_none());
    }
}
