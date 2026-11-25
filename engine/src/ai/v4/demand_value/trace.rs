//! F_TRACE: 1手番の購入を共通の点数 f で測り、貪欲法と上界と並べて出力する（計画書 Step 2、診断専用）。
//!
//! `OPENWARS_F_TRACE=1` のときだけ動き、生産の判断には一切使わない（既定の挙動は変わらない）。
//! - `actual`: V4（または強制購入）が実際に買う組。
//! - `alt`: `OPENWARS_F_TRACE_ALT`（強制購入と同じ形式の JSON）にその手番の行があれば、その組。
//! - `greedy`: f を使った貪欲法（`seeds = 0`）が選ぶ組。
//! - `*_delay_f`: 同じ組を1手番遅れて買った場合の f（購入期限の確認用）。
//! - `bound`: 施設ごとに最良の追加価値と最安の費用を組み合わせた、最適解の上界。

use super::board::{DemandBoard, PurchaseRow, build};
use super::{
    Candidate, FundsValue, Ledger, SelectionInput, fractional_upper_bound, marginal_value,
    select_greedy,
};
use crate::collections::HashMap;
use crate::components::{GridPosition, PlayerId};
use crate::resources::{MatchState, UNIT_TYPE_MAP, UnitType};
use bevy_ecs::prelude::World;
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// F_TRACE が有効か。
pub(crate) fn enabled() -> bool {
    std::env::var("OPENWARS_F_TRACE").as_deref() == Ok("1")
}

/// 評価する購入1件（兵種と生産施設）。
pub(crate) type Purchase = (UnitType, GridPosition);

/// 同じ購入を1手番遅らせた候補にする（購入・着く・満たし始め・離れる手番をすべて1つ後ろへ）。
fn delayed(candidate: Candidate) -> Candidate {
    Candidate {
        buy_turn: candidate.buy_turn + 1,
        timing: super::ServiceTiming {
            occupy_from: candidate.timing.occupy_from + 1,
            serve_from: candidate.timing.serve_from + 1,
        },
        release_after: candidate.release_after.map(|turn| turn + 1),
        ..candidate
    }
}

/// 既存部隊と `extra` の候補で貪欲法を回した合計と、選んだ候補の添字。
fn run(board: &DemandBoard, extra: &[Candidate], budget: u32) -> (FundsValue, Vec<usize>) {
    let candidates: Vec<Candidate> = board.existing.iter().chain(extra).copied().collect();
    let ledger = Ledger::default();
    let selection = select_greedy(
        &SelectionInput {
            demands: &board.demands,
            candidates: &candidates,
            budget: board.existing_units.saturating_add(budget),
            initial: &ledger,
            production_block_per_turn: &board.block,
        },
        0,
    );
    (selection.total, selection.picked)
}

/// 購入の組 `picks` の f（既存部隊だけの合計との差）。`delay` なら1手番遅らせて測る。
fn value_of_set(board: &DemandBoard, baseline: FundsValue, picks: &[Purchase], delay: bool) -> i64 {
    let extra: Vec<Candidate> = board
        .purchases
        .iter()
        .filter(|row| picks.contains(&(row.unit_type, row.candidate.facility)))
        .map(|row| {
            if delay {
                delayed(row.candidate)
            } else {
                row.candidate
            }
        })
        .collect();
    // 買うと決めた組なので予算では絞らない。
    run(board, &extra, u32::MAX / 2).0.0 - baseline.0
}

/// 強制購入と同じ形式の行（`[x, y, "兵種"]`）を、この手番に買える施設へ割り当てる。
/// 指定施設で買えなければ、同じ兵種を買える最寄りの施設に置き換える（強制購入と同じ規則）。
fn alt_purchases(
    board: &DemandBoard,
    rows: &[(usize, usize, String)],
    map: &crate::resources::Map,
) -> Vec<Purchase> {
    let mut used = Vec::new();
    let mut result = Vec::new();
    for (x, y, name) in rows {
        let Some(unit_type) = UNIT_TYPE_MAP
            .iter()
            .map(|(kind, _)| *kind)
            .find(|kind| format!("{kind:?}") == *name)
        else {
            continue;
        };
        let mut sites: Vec<GridPosition> = board
            .purchases
            .iter()
            .filter(|row| row.unit_type == unit_type)
            .map(|row| row.candidate.facility)
            .filter(|site| !used.contains(site))
            .collect();
        sites.sort_unstable_by_key(|site| (map.distance(site.x, site.y, *x, *y), site.y, site.x));
        if let Some(site) = sites.first() {
            used.push(*site);
            result.push((unit_type, *site));
        }
    }
    result
}

fn alt_rows(turn: u32) -> Option<Vec<(usize, usize, String)>> {
    let path = std::env::var("OPENWARS_F_TRACE_ALT").ok()?;
    let plan: BTreeMap<String, Vec<(usize, usize, String)>> =
        serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    plan.get(&turn.to_string()).cloned()
}

fn purchases_json(picks: &[Purchase]) -> Vec<Value> {
    picks
        .iter()
        .map(|(kind, pos)| json!([pos.x, pos.y, format!("{kind:?}")]))
        .collect()
}

/// 施設ごとに最良の追加価値と最安の費用を組み合わせた上界（どの最適解も施設ごとに1体まで）。
fn upper_bound(board: &DemandBoard, baseline_picked: &[usize]) -> FundsValue {
    let mut ledger = Ledger::default();
    for &index in baseline_picked {
        let candidate = &board.existing[index];
        if let Some(demand) = board.demands.iter().find(|d| d.id == candidate.demand) {
            ledger.commit(demand, candidate);
        }
    }
    let mut by_facility: HashMap<GridPosition, (FundsValue, u32)> = HashMap::default();
    for row in &board.purchases {
        let Some(demand) = board.demands.iter().find(|d| d.id == row.candidate.demand) else {
            continue;
        };
        let gain = marginal_value(demand, &row.candidate, &ledger, &board.block);
        if gain.0 <= 0 {
            continue;
        }
        let entry = by_facility
            .entry(row.candidate.facility)
            .or_insert((gain, row.candidate.cost));
        entry.0 = entry.0.max(gain);
        entry.1 = entry.1.min(row.candidate.cost);
    }
    let mut items: Vec<_> = by_facility.into_iter().collect();
    items.sort_unstable_by_key(|(pos, _)| (pos.y, pos.x));
    let gains: Vec<_> = items.into_iter().map(|(_, item)| item).collect();
    fractional_upper_bound(FundsValue(0), &gains, board.funds)
}

/// 1手番の購入 `actual` を測り、`F_TRACE {...}` を標準エラーへ1行出力する。
pub(crate) fn trace_purchases(world: &World, player: PlayerId, label: &str, actual: &[Purchase]) {
    if !enabled() {
        return;
    }
    let started = std::time::Instant::now();
    let Some(board) = build(world, player) else {
        return;
    };
    let Some(map) = world.get_resource::<crate::resources::Map>() else {
        return;
    };
    let turn = world
        .get_resource::<MatchState>()
        .map_or(board.turn, |state| state.current_turn_number.0);
    let (baseline, baseline_picked) = run(&board, &[], 0);

    // 貪欲法: 全購入候補から予算内で選ぶ。
    let all: Vec<Candidate> = board.purchases.iter().map(|row| row.candidate).collect();
    let (greedy_total, greedy_picked) = run(&board, &all, board.funds);
    let existing = board.existing.len();
    let greedy_rows: Vec<&PurchaseRow> = greedy_picked
        .iter()
        .filter(|&&index| index >= existing)
        .map(|&index| &board.purchases[index - existing])
        .collect();
    let greedy: Vec<Purchase> = greedy_rows
        .iter()
        .map(|row| (row.unit_type, row.candidate.facility))
        .collect();
    let greedy_f = greedy_total.0 - baseline.0;
    let bound = upper_bound(&board, &baseline_picked);

    let alt = alt_rows(turn).map(|rows| alt_purchases(&board, &rows, map));
    let detail = json!({
        "turn": turn,
        "player": player.0,
        "label": label,
        "funds": board.funds,
        "demands": board.demands.len(),
        "candidates": board.purchases.len(),
        "baseline": baseline.0,
        "actual": purchases_json(actual),
        "actual_f": value_of_set(&board, baseline, actual, false),
        "actual_delay_f": value_of_set(&board, baseline, actual, true),
        "alt": alt.as_deref().map(purchases_json),
        "alt_f": alt.as_deref().map(|picks| value_of_set(&board, baseline, picks, false)),
        "alt_delay_f": alt.as_deref().map(|picks| value_of_set(&board, baseline, picks, true)),
        "greedy": purchases_json(&greedy),
        "greedy_kinds": greedy_rows.iter().map(|row| format!("{:?}", row.kind)).collect::<Vec<_>>(),
        "greedy_f": greedy_f,
        "greedy_delay_f": value_of_set(&board, baseline, &greedy, true),
        "bound": bound.0,
        "ratio": if bound.0 > 0 { greedy_f as f64 / bound.0 as f64 } else { 1.0 },
        "elapsed_ms": started.elapsed().as_secs_f64() * 1000.0,
    });
    eprintln!("F_TRACE {detail}");
}
