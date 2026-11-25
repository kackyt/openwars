//! 需要価値 f・貪欲法・上界の単体テスト。map_42 後攻（P2）のロケットの例で期待値を固定する。
//!
//! 共通の設定:
//! - 対局の最終手番は T30。
//! - 森 (3,7) の被覆需要: 敵が T5 に着く。1手番あたり 100。
//! - 生産口の前線都市 (4,5)、生産口ではない前線都市 (3,5)、首都 (5,3)。

use super::*;

fn cell(x: usize, y: usize) -> GridPosition {
    GridPosition { x, y }
}

const LAST_TURN: u32 = 30;
const ROCKET_COST: u32 = 6200;
const INFANTRY_COST: u32 = 1000;

fn window(start: u32, end: u32) -> TurnWindow {
    TurnWindow { start, end }
}

fn demand(id: u32, start: u32, end: u32, per_turn: i64) -> Demand {
    Demand {
        id: DemandId(id),
        window: window(start, end),
        per_turn: FundsValue(per_turn),
    }
}

/// 森 (3,7) を撃つ需要（T5〜T30、1手番 100）。
fn forest_cover() -> Demand {
    demand(1, 5, LAST_TURN, 100)
}

/// ロケットを `facility` で `buy_turn` に買い、`site` に `travel_turns` で布陣させる候補。
fn rocket(
    demand: &Demand,
    facility: GridPosition,
    site: GridPosition,
    buy_turn: u32,
    travel_turns: u32,
) -> Candidate {
    Candidate {
        demand: demand.id,
        facility,
        cost: ROCKET_COST,
        buy_turn,
        occupy: site,
        timing: ServiceTiming::indirect_fire(buy_turn, travel_turns),
        transit_loss: FundsValue(0),
    }
}

/// 歩兵を `facility` で `buy_turn` に買い、`cell` を保持させる候補。
fn holder(demand: &Demand, facility: GridPosition, cell: GridPosition, buy_turn: u32) -> Candidate {
    Candidate {
        demand: demand.id,
        facility,
        cost: INFANTRY_COST,
        buy_turn,
        occupy: cell,
        timing: ServiceTiming::occupy(buy_turn, 1),
        transit_loss: FundsValue(0),
    }
}

fn no_block() -> HashMap<GridPosition, FundsValue> {
    HashMap::default()
}

fn value_of(demand: &Demand, candidate: &Candidate, ledger: &Ledger) -> i64 {
    marginal_value(demand, candidate, ledger, &no_block()).0
}

// ---- 手番の区間 ----

#[test]
fn turn_window_len_and_intersection() {
    assert_eq!(window(5, 30).len(), 26);
    assert_eq!(window(5, 5).len(), 1);
    assert_eq!(window(6, 5).len(), 0, "start > end は空");
    assert_eq!(window(1, 10).intersect(window(5, 30)), window(5, 10));
    assert_eq!(window(1, 4).intersect(window(5, 30)).len(), 0);
}

#[test]
fn service_timing_for_holders_and_indirect_fire() {
    // 保持型: T3 に買い 1 手番で着けば T4 から満たす。
    assert_eq!(
        ServiceTiming::occupy(3, 1),
        ServiceTiming {
            occupy_from: 4,
            serve_from: 4
        }
    );
    // 間接射撃型: T4 に着き、移動した手番は撃てないので T5 から。
    assert_eq!(
        ServiceTiming::indirect_fire(3, 1),
        ServiceTiming {
            occupy_from: 4,
            serve_from: 5
        }
    );
    // 生産施設そのものが砲座: T3 から居続け、T4 から撃てる。
    assert_eq!(
        ServiceTiming::indirect_fire(3, 0),
        ServiceTiming {
            occupy_from: 3,
            serve_from: 4
        }
    );
}

// ---- 時間: 遅く着くほど満たせる手番が減る ----

#[test]
fn buying_later_loses_exactly_the_missed_turns() {
    let need = forest_cover();
    let ledger = Ledger::default();
    let at = |buy_turn| {
        value_of(
            &need,
            &rocket(&need, cell(4, 5), cell(3, 5), buy_turn, 1),
            &ledger,
        )
    };
    // T3 購入 → T5 から撃てる: 26 手番 × 100。
    assert_eq!(at(3), 2600);
    // T4 購入 → T6 から: 1 手番失う。
    assert_eq!(at(4), 2500);
    // T6 購入 → T8 から: 3 手番失う。
    assert_eq!(at(6), 2300);
}

#[test]
fn a_farther_facility_costs_the_extra_travel_turns() {
    let need = forest_cover();
    // 首都 (5,3) から砲座まで 2 手番 → T6 から撃てる。
    let from_capital = rocket(&need, cell(5, 3), cell(3, 5), 3, 2);
    assert_eq!(value_of(&need, &from_capital, &Ledger::default()), 2500);
}

#[test]
fn a_later_deadline_makes_buying_now_or_next_turn_equal() {
    // 敵が T8 に着く需要なら、T3 でも T4 でも撃ち始めに間に合い、同じ価値になる。
    let need = demand(2, 8, LAST_TURN, 100);
    let ledger = Ledger::default();
    let now = value_of(&need, &rocket(&need, cell(4, 5), cell(3, 5), 3, 1), &ledger);
    let next = value_of(&need, &rocket(&need, cell(4, 5), cell(3, 5), 4, 1), &ledger);
    assert_eq!(now, 2300);
    assert_eq!(now, next);
}

#[test]
fn arriving_after_the_last_turn_is_worth_nothing() {
    let need = forest_cover();
    let too_late = rocket(&need, cell(4, 5), cell(3, 5), LAST_TURN, 1);
    assert_eq!(value_of(&need, &too_late, &Ledger::default()), 0);
}

// ---- 地理: 道中の危険 ----

#[test]
fn transit_loss_is_subtracted() {
    let need = forest_cover();
    let mut exposed = rocket(&need, cell(4, 5), cell(3, 5), 3, 1);
    exposed.transit_loss = FundsValue(300);
    assert_eq!(value_of(&need, &exposed, &Ledger::default()), 2300);
}

// ---- 占有: 同じマスには同時に 1 体 ----

#[test]
fn a_second_unit_on_the_same_site_adds_nothing() {
    let need = forest_cover();
    let first = rocket(&need, cell(4, 5), cell(3, 5), 3, 1);
    let mut ledger = Ledger::default();
    ledger.commit(&need, &first);
    // 別の需要でも、同じ砲座 (3,5) は T4 以降埋まっている。
    let other_need = demand(3, 5, LAST_TURN, 100);
    let second = rocket(&other_need, cell(6, 5), cell(3, 5), 3, 1);
    assert_eq!(value_of(&other_need, &second, &ledger), 0);
}

#[test]
fn a_partly_used_cell_counts_only_the_free_turns() {
    let need = forest_cover();
    let mut ledger = Ledger::default();
    // 既存の味方が (3,5) を T10 まで使う。
    ledger.claim_cell(cell(3, 5), window(4, 10));
    let late = rocket(&need, cell(4, 5), cell(3, 5), 3, 1);
    // T5〜T30 のうち空いている T11〜T30 の 20 手番。
    assert_eq!(value_of(&need, &late, &ledger), 2000);
}

#[test]
fn a_demand_already_served_from_another_site_adds_nothing() {
    let need = forest_cover();
    let mut ledger = Ledger::default();
    ledger.commit(&need, &rocket(&need, cell(4, 5), cell(3, 5), 3, 1));
    // 別の砲座 (2,5) から同じ森を撃っても、同じ需要は満たし済み。
    let duplicate = rocket(&need, cell(4, 5), cell(2, 5), 3, 1);
    assert_eq!(value_of(&need, &duplicate, &ledger), 0);
    // 別の需要なら満額。
    let other_need = demand(4, 5, LAST_TURN, 100);
    let other = rocket(&other_need, cell(4, 5), cell(2, 5), 3, 1);
    assert_eq!(value_of(&other_need, &other, &ledger), 2600);
}

// ---- 生産施設を塞ぐ損 ----

#[test]
fn a_battery_on_a_production_city_pays_for_the_blocked_turns() {
    let need = forest_cover();
    let block: HashMap<_, _> = [(cell(4, 5), FundsValue(40))].into_iter().collect();
    let ledger = Ledger::default();
    // 生産口 (4,5) で買ってそのまま撃つ: T4 から撃てるが需要は T5 から → 26 手番 × 100 = 2600。
    // T4〜T30 の 27 手番 (4,5) を塞ぐ → 27 × 40 = 1080 の損。
    let on_mouth = rocket(&need, cell(4, 5), cell(4, 5), 3, 0);
    assert_eq!(marginal_value(&need, &on_mouth, &ledger, &block).0, 1520);
    // 生産口ではない (3,5) へ 1 マス動かせば損はない。
    let beside = rocket(&need, cell(4, 5), cell(3, 5), 3, 1);
    assert_eq!(marginal_value(&need, &beside, &ledger, &block).0, 2600);
}

// ---- 貪欲法 ----

/// 予算 7,200 でロケット 1 門と歩兵 1 体が最良になる例。
/// - 0: ロケット（2,600 / 6,200）
/// - 1: 歩兵（900 / 1,000）
/// - 2: 歩兵（800 / 1,000）
fn rocket_or_two_infantry() -> (Vec<Demand>, Vec<Candidate>) {
    let cover = forest_cover();
    let hold_a = demand(10, 1, 9, 100);
    let hold_b = demand(11, 1, 8, 100);
    let candidates = vec![
        rocket(&cover, cell(4, 5), cell(3, 5), 3, 1),
        holder(&hold_a, cell(5, 5), cell(5, 7), 0),
        holder(&hold_b, cell(6, 5), cell(6, 7), 0),
    ];
    (vec![cover, hold_a, hold_b], candidates)
}

fn select(demands: &[Demand], candidates: &[Candidate], budget: u32, seeds: usize) -> Selection {
    let ledger = Ledger::default();
    let block = no_block();
    select_greedy(
        &SelectionInput {
            demands,
            candidates,
            budget,
            initial: &ledger,
            production_block_per_turn: &block,
        },
        seeds,
    )
}

#[test]
fn fixture_gains_are_as_documented() {
    let (demands, candidates) = rocket_or_two_infantry();
    let ledger = Ledger::default();
    let gains: Vec<_> = candidates
        .iter()
        .map(|c| {
            let d = demands.iter().find(|d| d.id == c.demand).unwrap();
            value_of(d, c, &ledger)
        })
        .collect();
    assert_eq!(gains, vec![2600, 900, 800]);
}

#[test]
fn plain_greedy_falls_back_to_the_best_single_item() {
    let (demands, candidates) = rocket_or_two_infantry();
    // 比率順の貪欲法は歩兵 2 体（1,700）で止まる。単品最良のロケット（2,600）の方が大きい。
    let result = select(&demands, &candidates, 7200, 0);
    assert_eq!(result.picked, vec![0]);
    assert_eq!(result.total, FundsValue(2600));
    assert_eq!(result.spent, ROCKET_COST);
}

#[test]
fn seeding_the_first_pick_finds_rocket_plus_infantry() {
    let (demands, candidates) = rocket_or_two_infantry();
    // ロケットを最初に固定すると、残り 1,000 で比率最良の歩兵が入る（3,500）。
    let result = select(&demands, &candidates, 7200, 1);
    assert_eq!(result.picked, vec![0, 1]);
    assert_eq!(result.total, FundsValue(3500));
    assert_eq!(result.spent, ROCKET_COST + INFANTRY_COST);
}

#[test]
fn one_facility_builds_only_one_unit_per_turn() {
    let hold_a = demand(10, 1, 9, 100);
    let hold_b = demand(11, 1, 8, 100);
    let hold_c = demand(12, 1, 7, 100);
    let candidates = vec![
        holder(&hold_a, cell(5, 5), cell(5, 7), 0),
        // 同じ施設 (5,5) を使う 2 体目は入らない。
        holder(&hold_b, cell(5, 5), cell(4, 7), 0),
        holder(&hold_c, cell(6, 5), cell(6, 7), 0),
    ];
    let result = select(&[hold_a, hold_b, hold_c], &candidates, 10_000, 0);
    assert_eq!(result.picked, vec![0, 2]);
}

#[test]
fn a_stale_gain_is_recomputed_after_a_site_is_taken() {
    let need = forest_cover();
    let other_need = demand(3, 5, LAST_TURN, 100);
    let hold = demand(10, 1, 9, 100);
    let mut second = rocket(&other_need, cell(6, 5), cell(3, 5), 3, 1);
    second.transit_loss = FundsValue(100);
    let candidates = vec![
        rocket(&need, cell(4, 5), cell(3, 5), 3, 1), // 2,600
        second,                                      // 2,500（同じ砲座）
        holder(&hold, cell(5, 5), cell(5, 7), 0),    // 900
    ];
    // 歩兵 → ロケット 0 の順に入り、同じ砲座の候補 1 は追加価値 0 になって入らない。
    let result = select(&[need, other_need, hold], &candidates, 20_000, 0);
    assert_eq!(result.picked, vec![0, 2]);
    assert_eq!(result.total, FundsValue(3500));
}

#[test]
fn candidates_with_no_positive_gain_are_never_picked() {
    let need = forest_cover();
    let block: HashMap<_, _> = [(cell(4, 5), FundsValue(1000))].into_iter().collect();
    let candidates = vec![rocket(&need, cell(4, 5), cell(4, 5), 3, 0)];
    let ledger = Ledger::default();
    let result = select_greedy(
        &SelectionInput {
            demands: &[need],
            candidates: &candidates,
            budget: 20_000,
            initial: &ledger,
            production_block_per_turn: &block,
        },
        1,
    );
    assert!(result.picked.is_empty());
    assert_eq!(result.total, FundsValue(0));
}

#[test]
fn existing_units_in_the_initial_ledger_are_respected() {
    let need = forest_cover();
    let candidates = vec![rocket(&need, cell(4, 5), cell(3, 5), 3, 1)];
    let mut ledger = Ledger::default();
    // 既存の味方が (3,5) に最後まで居座る。
    ledger.claim_cell(cell(3, 5), window(0, LAST_TURN));
    let block = no_block();
    let result = select_greedy(
        &SelectionInput {
            demands: &[need],
            candidates: &candidates,
            budget: 20_000,
            initial: &ledger,
            production_block_per_turn: &block,
        },
        0,
    );
    assert!(result.picked.is_empty());
}

// ---- 上界 ----

#[test]
fn fractional_upper_bound_fills_by_ratio() {
    let gains = [
        (FundsValue(2600), ROCKET_COST),
        (FundsValue(900), INFANTRY_COST),
        (FundsValue(800), INFANTRY_COST),
    ];
    // 900 + 800 を入れて残り 5,200。ロケットは 2,600 × 5,200 / 6,200 = 2,180（切り捨て）。
    assert_eq!(
        fractional_upper_bound(FundsValue(1000), &gains, 7200),
        FundsValue(1000 + 900 + 800 + 2180)
    );
}

#[test]
fn fractional_upper_bound_ignores_non_positive_gains() {
    let gains = [
        (FundsValue(-500), 1000),
        (FundsValue(0), 1000),
        (FundsValue(900), 1000),
    ];
    assert_eq!(
        fractional_upper_bound(FundsValue(0), &gains, 10_000),
        FundsValue(900)
    );
}

#[test]
fn upper_bound_is_never_below_the_greedy_result() {
    let (demands, candidates) = rocket_or_two_infantry();
    let ledger = Ledger::default();
    let gains: Vec<_> = candidates
        .iter()
        .map(|c| {
            let d = demands.iter().find(|d| d.id == c.demand).unwrap();
            (marginal_value(d, c, &ledger, &no_block()), c.cost)
        })
        .collect();
    let bound = fractional_upper_bound(FundsValue(0), &gains, 7200);
    let best = select(&demands, &candidates, 7200, 1);
    assert!(
        bound >= best.total,
        "bound={bound:?} greedy={:?}",
        best.total
    );
}
