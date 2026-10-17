use super::super::model::OpeningProductionPolicy;
use super::super::small_map_tests::{input, stats};
use super::super::{Arc, GridPosition, PlannedPurchase, ProductionPlanOption, UnitType};
use super::*;

// 段分割前の実装（HEAD 5cf5fb7）の計画結果全体を、Debug表現の64bit FNV-1aで固定する。
fn fingerprint(plan: &ForcePackagePlan) -> u64 {
    format!("{plan:?}")
        .bytes()
        .fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
        })
}

fn option(x: usize, build_turn: u32, unit_type: UnitType, cost: u32) -> ProductionPlanOption {
    ProductionPlanOption {
        purchase: PlannedPurchase {
            facility: GridPosition { x, y: 0 },
            unit_type,
            build_turn,
            cost,
        },
        stats: stats(unit_type, cost, 6),
        engageable_enemy_indices: vec![0],
        capture_target: None,
        capture_arrival_turn: None,
        capture_completion_turn: None,
        capture_durability: None,
        capture_blocking_enemy_index: None,
    }
}

// 3施設×3手番に、同じ枠で劣後する高額の同兵種を混ぜた入力。
fn searched_input() -> RollingPlanInput {
    let mut input = input();
    input.production_options.clear();
    // 偵察車は安く被弾も無いが与ダメージが攻撃ヘリより低く、相互に支配しない。
    Arc::make_mut(&mut input.damage_chart).insert_damage(UnitType::Recon, UnitType::Infantry, 30);
    for build_turn in 0..3 {
        for x in 0..3 {
            input
                .production_options
                .push(option(x, build_turn, UnitType::Bcopters, 7_500));
            input
                .production_options
                .push(option(x, build_turn, UnitType::Recon, 4_200));
            input
                .production_options
                .push(option(x, build_turn, UnitType::Bcopters, 9_000));
        }
    }
    input.current_funds = 30_000;
    input.income_per_turn = 10_000;
    input.hard_deadline = Some(4);
    input.search_beam_width = 4;
    input
}

#[test]
fn force_package_standard_beam_search_is_characterized() {
    let plan = plan_force_package(&searched_input()).expect("標準探索の計画");
    assert!(plan.search_truncated);
    assert_eq!(plan.candidates_pruned, 9);
    assert_eq!(fingerprint(&plan), EXPECTED_STANDARD);
}

#[test]
fn force_package_interdiction_priority_is_characterized() {
    let mut input = searched_input();
    input.prioritize_deadline_interdiction = true;
    input.interdiction_deadlines = vec![(0, 3)];
    input.deadline_target_index = Some(0);
    let plan = plan_force_package(&input).expect("妨害優先の計画");
    assert!(plan.search_truncated);
    assert_eq!(fingerprint(&plan), EXPECTED_INTERDICTION);
}

#[test]
fn force_package_small_map_ordering_is_characterized() {
    let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
    let mut input = searched_input();
    input.opening_policy = OpeningProductionPolicy::SmallMapExpansion;
    let plan = plan_force_package(&input).expect("小規模順位の計画");
    assert!(plan.search_truncated);
    assert_eq!(fingerprint(&plan), EXPECTED_SMALL_MAP);
}

#[test]
fn force_package_early_returns_are_characterized() {
    let mut no_enemy = searched_input();
    no_enemy.enemies.clear();
    let plan = plan_force_package(&no_enemy).expect("敵なしでも計画を返す");
    assert_eq!(plan.candidates_considered, 1);
    assert_eq!(fingerprint(&plan), EXPECTED_NO_ENEMY);

    let mut no_option = searched_input();
    no_option.production_options.clear();
    let plan = plan_force_package(&no_option).expect("生産枠なしでも計画を返す");
    assert_eq!(plan.candidates_considered, 1);
    assert_eq!(fingerprint(&plan), EXPECTED_NO_OPTION);
}

#[test]
fn force_package_infeasible_selection_is_characterized() {
    let mut input = searched_input();
    input.current_funds = 8_000;
    input.income_per_turn = 0;
    input.hard_deadline = Some(2);
    let plan = plan_force_package(&input).expect("実行不能でも最善努力案を返す");
    assert!(!plan.feasible, "最善努力案の補完経路を通る入力であること");
    assert_eq!(fingerprint(&plan), EXPECTED_INFEASIBLE);
}

const EXPECTED_STANDARD: u64 = 17_484_225_368_735_642_693;
const EXPECTED_INTERDICTION: u64 = 13_760_800_904_259_797_372;
const EXPECTED_SMALL_MAP: u64 = 17_770_427_080_801_935_578;
const EXPECTED_NO_ENEMY: u64 = 10_860_942_530_314_345_895;
const EXPECTED_NO_OPTION: u64 = 309_614_520_729_983_265;
const EXPECTED_INFEASIBLE: u64 = 8_282_824_046_907_035_861;
