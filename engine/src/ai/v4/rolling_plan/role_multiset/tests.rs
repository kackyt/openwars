use super::super::model::ProductionAttackProjection;
use super::super::small_map_tests::{input, stats};
use super::super::{Arc, PlannedPurchase, ProductionPlanOption, RollingPlanInput, UnitStats};
use super::*;
use std::fmt::Write as _;

// 施設・役割の対応を本番の役割カタログと同じ規則（同じ施設・役割は後勝ち）で組み立てる。
struct Catalog {
    roles: Vec<ProductionRole>,
    facilities: Vec<(GridPosition, Vec<usize>)>,
    facility_role_options: HashMap<(GridPosition, ProductionRole), usize>,
}

impl Catalog {
    fn new(input: &RollingPlanInput) -> Self {
        let mut catalog = Self {
            roles: Vec::new(),
            facilities: Vec::new(),
            facility_role_options: HashMap::default(),
        };
        for (index, option) in input.production_options.iter().enumerate() {
            let facility = option.purchase.facility;
            let role = match option.capture_target {
                Some(target) => ProductionRole::Capture {
                    target,
                    unit_type: option.purchase.unit_type,
                    cost: option.purchase.cost,
                },
                None => ProductionRole::Combat {
                    unit_type: option.purchase.unit_type,
                    cost: option.purchase.cost,
                },
            };
            match catalog
                .facilities
                .iter_mut()
                .find(|(pos, _)| *pos == facility)
            {
                Some((_, indices)) => indices.push(index),
                None => catalog.facilities.push((facility, vec![index])),
            }
            catalog
                .facility_role_options
                .insert((facility, role), index);
            if !catalog.roles.contains(&role) {
                catalog.roles.push(role);
            }
        }
        catalog
    }

    fn generate(
        &self,
        input: &RollingPlanInput,
        funds: u32,
        max_slots: usize,
    ) -> (Vec<Vec<ProductionRole>>, bool) {
        generate_role_multisets(
            &self.roles,
            input,
            &self.facilities,
            &self.facility_role_options,
            funds,
            max_slots,
        )
    }
}

fn option(
    x: usize,
    unit_type: UnitType,
    cost: u32,
    capture: Option<(GridPosition, Option<u32>)>,
) -> ProductionPlanOption {
    let mut unit_stats = stats(unit_type, cost, 6);
    if capture.is_some() {
        unit_stats = UnitStats {
            can_capture: true,
            movement_type: MovementType::Infantry,
            ..stats(unit_type, cost, 3)
        };
    }
    ProductionPlanOption {
        purchase: PlannedPurchase {
            facility: GridPosition { x, y: 0 },
            unit_type,
            build_turn: 0,
            cost,
        },
        stats: unit_stats,
        engageable_enemy_indices: vec![0],
        capture_target: capture.map(|(target, _)| target),
        capture_arrival_turn: capture.map(|_| 2),
        capture_completion_turn: capture.and_then(|(_, completion)| completion),
        capture_durability: capture.map(|_| 200),
        capture_blocking_enemy_index: None,
    }
}

fn projection(x: usize, reaches: bool) -> Vec<Option<ProductionAttackProjection>> {
    vec![reaches.then_some(ProductionAttackProjection {
        ready_turn: 1,
        firing_position: GridPosition { x, y: 0 },
        requires_movement: false,
    })]
}

fn base_input() -> RollingPlanInput {
    let mut input = input();
    input.production_options.clear();
    input.production_attack_projections.clear();
    input
}

fn target(x: usize) -> GridPosition {
    GridPosition { x, y: 0 }
}

// 出力全体の順序と内容を、64bit FNV-1aで固定する。
fn fingerprint(multisets: &[Vec<ProductionRole>]) -> u64 {
    let mut text = String::new();
    write!(text, "{multisets:?}").unwrap();
    text.bytes().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

#[test]
fn role_multiset_without_roles_or_slots_is_empty() {
    let mut input = base_input();
    input
        .production_options
        .push(option(0, UnitType::Recon, 4_000, None));
    let catalog = Catalog::new(&input);
    assert_eq!(catalog.generate(&input, 10_000, 0), (Vec::new(), false));
    let empty = Catalog::new(&base_input());
    assert_eq!(empty.generate(&input, 10_000, 3), (Vec::new(), false));
}

#[test]
fn role_multiset_enumerates_levels_in_role_order_and_rejects_shared_targets() {
    let mut input = base_input();
    input.production_options = vec![
        option(0, UnitType::Infantry, 1_000, Some((target(9), Some(3)))),
        option(0, UnitType::Recon, 4_000, None),
        option(1, UnitType::Mech, 1_500, Some((target(9), Some(3)))),
        option(1, UnitType::Recon, 4_000, None),
        option(1, UnitType::Infantry, 1_000, Some((target(8), Some(4)))),
    ];
    let catalog = Catalog::new(&input);
    let [capture_a, recon, mech_a, capture_b] = catalog.roles[..] else {
        panic!("4役割を想定する: {:?}", catalog.roles);
    };

    // 資金6,000・2枠では、同じ物件への二重占領と資金超過の組合せだけが落ちる。
    let expected = vec![
        vec![capture_a],
        vec![recon],
        vec![mech_a],
        vec![capture_b],
        vec![capture_a, recon],
        vec![capture_a, capture_b],
        vec![recon, mech_a],
        vec![recon, capture_b],
        vec![mech_a, capture_b],
    ];
    assert_eq!(catalog.generate(&input, 6_000, 2), (expected, false));
}

#[test]
fn role_multiset_limits_combat_copies_by_capacity_and_global_cap() {
    let mut input = base_input();
    for x in 0..5 {
        input
            .production_options
            .push(option(x, UnitType::Recon, 1_000, None));
    }
    let catalog = Catalog::new(&input);
    let recon = catalog.roles[0];
    // 5施設で生産できても、同一戦闘役は3体まで。
    assert_eq!(
        catalog.generate(&input, 100_000, 5),
        (vec![vec![recon], vec![recon; 2], vec![recon; 3]], false)
    );

    let mut input = base_input();
    for x in 0..2 {
        input
            .production_options
            .push(option(x, UnitType::Recon, 1_000, None));
    }
    input.production_options.push(option(
        2,
        UnitType::Infantry,
        1_000,
        Some((target(9), None)),
    ));
    let catalog = Catalog::new(&input);
    let [recon, capture] = catalog.roles[..] else {
        panic!("2役割を想定する: {:?}", catalog.roles);
    };
    // 生産可能施設が2つなら、空き枠があっても2体まで。資金ちょうども許容する。
    assert_eq!(
        catalog.generate(&input, 3_000, 3),
        (
            vec![
                vec![recon],
                vec![capture],
                vec![recon; 2],
                vec![recon, capture],
                vec![recon, recon, capture],
            ],
            false
        )
    );
}

#[test]
fn role_multiset_beam_truncation_keeps_the_current_frontier_order() {
    let mut input = base_input();
    input.map = Arc::new(super::super::Map::new(
        40,
        1,
        super::super::Terrain::Plains,
        crate::resources::GridTopology::Square,
    ));
    let chart = Arc::make_mut(&mut input.damage_chart);
    chart.insert_damage(UnitType::Recon, UnitType::Infantry, 60);
    chart.insert_damage(UnitType::Tank, UnitType::Infantry, 75);
    chart.insert_damage(UnitType::AntiAir, UnitType::Infantry, 105);
    chart.insert_damage(UnitType::Mech, UnitType::Infantry, 65);
    for x in 0..10 {
        let combat = [
            (UnitType::Recon, 4_000),
            (UnitType::Tank, 7_000),
            (UnitType::AntiAir, 8_000),
        ];
        for (unit_type, cost) in combat {
            input
                .production_options
                .push(option(x, unit_type, cost, None));
            input
                .production_attack_projections
                .push(projection(x, x % 3 != 2));
        }
        // 徒歩の戦闘役は非歩兵戦闘数に数えない。
        let mut mech = option(x, UnitType::Mech, 3_000, None);
        mech.stats.movement_type = MovementType::Infantry;
        input.production_options.push(mech);
        input
            .production_attack_projections
            .push(projection(x, true));
        // 2施設で共有する占領物件は最短完了turnを、完了不明の物件はu32::MAXを使う。
        let capture = match x {
            0 => (target(30), Some(6)),
            1 => (target(30), Some(4)),
            2 => (target(31), None),
            _ => (target(30 + x), Some(2 + (x as u32 % 3))),
        };
        input
            .production_options
            .push(option(x, UnitType::Infantry, 1_000, Some(capture)));
        input
            .production_attack_projections
            .push(projection(x, false));
    }
    // 射撃投影を持つ施設では投影が優先され、投影が無い末尾の施設ではengageableへ戻る。
    input
        .production_options
        .push(option(8, UnitType::Bomber, 20_000, None));
    input
        .production_attack_projections
        .push(projection(8, false));
    input
        .production_options
        .push(option(9, UnitType::Bcopters, 9_000, None));
    let catalog = Catalog::new(&input);

    let (multisets, truncated) = catalog.generate(&input, 60_000, catalog.facilities.len());

    assert!(truncated);
    assert_eq!(
        (multisets.len(), fingerprint(&multisets)),
        (EXPECTED_BEAM_COUNT, EXPECTED_BEAM_FINGERPRINT)
    );
}

// 段分割前の実装（HEAD 2425e31）で得た出力を固定する。
const EXPECTED_BEAM_COUNT: usize = 2_446;
const EXPECTED_BEAM_FINGERPRINT: u64 = 968_661_165_243_805_251;
