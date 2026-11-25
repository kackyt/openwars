//! 需要ベースの共通点数 f と、列挙しない購入選択（遅延評価の貪欲法・上界）。
//!
//! 計画書 `docs/07-project-management/plans/v4-demand-based-production.md` の 3.2〜3.4 節に対応する。
//! 需要ごとに「場所・有効期間・1手番あたりの価値」を持たせ、部隊が満たせる手番数で価値を測る。
//! 同じマスには同時に1体しか入れないこと（占有）と、生産施設を塞ぐ損もここで数える。
//!
//! 実装上の約束:
//! - 全組合せの列挙はしない（施設 × 兵種で爆発するため）。
//! - 調整用の定数を足さない。価値は呼び出し側がルール（ダメコン・地形防御・収入）から渡す。

// Step 3 で生産へ接続するまで呼び出し元がないため、一時的に許可する。
#![allow(dead_code)]

use std::cmp::Ordering;
use std::collections::BinaryHeap;

use crate::collections::{HashMap, HashSet};
use crate::components::GridPosition;

#[cfg(test)]
mod tests;

/// 資金換算の価値。損（負の値）も表せる。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub(crate) struct FundsValue(pub(crate) i64);

/// 両端を含む手番の区間。`start > end` なら空。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TurnWindow {
    pub(crate) start: u32,
    pub(crate) end: u32,
}

impl TurnWindow {
    /// 区間に含まれる手番数。
    pub(crate) fn len(self) -> u32 {
        self.end
            .checked_sub(self.start)
            .map_or(0, |span| span.saturating_add(1))
    }

    /// 2つの区間の共通部分。
    pub(crate) fn intersect(self, other: Self) -> Self {
        Self {
            start: self.start.max(other.start),
            end: self.end.min(other.end),
        }
    }
}

/// 需要の識別子。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct DemandId(pub(crate) u32);

/// 需要1つ: 有効期間のあいだ、1手番満たすごとに `per_turn` の価値がある。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Demand {
    pub(crate) id: DemandId,
    /// `start` = 敵がその場所に着く手番、`end` = 対局の最終手番。
    pub(crate) window: TurnWindow,
    pub(crate) per_turn: FundsValue,
}

/// 部隊が需要を満たすまでの時間的な流れ。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ServiceTiming {
    /// 部隊がとどまるマスに着く手番。
    pub(crate) occupy_from: u32,
    /// 需要を満たし始める手番。
    pub(crate) serve_from: u32,
}

impl ServiceTiming {
    /// 着いた手番から満たす部隊（陣地線のマス保持・占領など）。
    ///
    /// 買った手番には動けないため、着くのは `購入手番 + 移動手番数`（移動 0 なら購入手番）。
    pub(crate) fn occupy(buy_turn: u32, travel_turns: u32) -> Self {
        let arrival = buy_turn.saturating_add(travel_turns);
        Self {
            occupy_from: arrival,
            serve_from: arrival,
        }
    }

    /// 移動した手番には撃てない間接部隊。着いた次の手番から満たす。
    pub(crate) fn indirect_fire(buy_turn: u32, travel_turns: u32) -> Self {
        let mut timing = Self::occupy(buy_turn, travel_turns);
        timing.serve_from = timing.serve_from.saturating_add(1);
        timing
    }
}

/// 購入候補1つ（兵種・施設・満たす需要・とどまるマスの組）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Candidate {
    pub(crate) demand: DemandId,
    /// 生産する施設。同じ手番に1施設で1体しか作れない。
    pub(crate) facility: GridPosition,
    /// 購入費。0 より大きいこと。
    pub(crate) cost: u32,
    pub(crate) buy_turn: u32,
    /// 部隊がとどまるマス（砲座・陣地線のマス・占領対象など）。
    pub(crate) occupy: GridPosition,
    pub(crate) timing: ServiceTiming,
    /// 道中で敵の攻撃圏を通る場合の期待損害。
    pub(crate) transit_loss: FundsValue,
}

/// 確定済みの占有と、需要ごとに満たし済みの手番の記録。
#[derive(Debug, Clone, Default)]
pub(crate) struct Ledger {
    cells: HashMap<GridPosition, Vec<TurnWindow>>,
    served: HashMap<DemandId, Vec<TurnWindow>>,
}

impl Ledger {
    /// マスを区間のあいだ使用中にする（既存の味方部隊の登録にも使う）。
    pub(crate) fn claim_cell(&mut self, cell: GridPosition, window: TurnWindow) {
        if window.start <= window.end {
            self.cells.entry(cell).or_default().push(window);
        }
    }

    /// 候補を確定する: とどまるマスを `[occupy_from, 需要の終端]` で占有し、
    /// 需要の `[max(serve_from, 需要の始端), 需要の終端]` を満たし済みにする。
    pub(crate) fn commit(&mut self, demand: &Demand, candidate: &Candidate) {
        self.claim_cell(
            candidate.occupy,
            TurnWindow {
                start: candidate.timing.occupy_from,
                end: demand.window.end,
            },
        );
        let served = service_window(demand, candidate);
        if served.start <= served.end {
            self.served.entry(demand.id).or_default().push(served);
        }
    }
}

/// 候補を今の記録に追加したときの価値（追加価値）。
///
/// ```text
/// 満たす手番 = [max(serve_from, 需要の始端), 需要の終端] のうち、
///              とどまるマスが空いていて、需要がまだ満たされていない手番
/// 塞ぐ手番   = [occupy_from, 需要の終端] のうち、購入手番より後で、とどまるマスが空いている手番
/// 価値 = per_turn × 満たす手番数 − 塞ぐコスト(マス) × 塞ぐ手番数 − transit_loss
/// ```
///
/// `production_block_per_turn` は生産施設のマスを1手番塞ぐごとに失う価値（生産施設でないマスは含めない）。
/// 部隊は需要の終端までそのマスにとどまるものとして数える。
pub(crate) fn marginal_value(
    demand: &Demand,
    candidate: &Candidate,
    ledger: &Ledger,
    production_block_per_turn: &HashMap<GridPosition, FundsValue>,
) -> FundsValue {
    evaluate(demand, candidate, ledger, production_block_per_turn).0
}

fn service_window(demand: &Demand, candidate: &Candidate) -> TurnWindow {
    demand.window.intersect(TurnWindow {
        start: candidate.timing.serve_from,
        end: demand.window.end,
    })
}

fn evaluate(
    demand: &Demand,
    candidate: &Candidate,
    ledger: &Ledger,
    production_block_per_turn: &HashMap<GridPosition, FundsValue>,
) -> (FundsValue, FundsValue) {
    let cells = ledger
        .cells
        .get(&candidate.occupy)
        .map_or(&[][..], Vec::as_slice);
    let served = ledger.served.get(&demand.id).map_or(&[][..], Vec::as_slice);
    let served_turns = free_turns(
        service_window(demand, candidate),
        cells.iter().chain(served),
    );

    let block = production_block_per_turn
        .get(&candidate.occupy)
        .copied()
        .unwrap_or_default();
    let blocked_turns = if block.0 != 0 && candidate.buy_turn < demand.window.end {
        free_turns(
            TurnWindow {
                start: candidate.timing.occupy_from.max(candidate.buy_turn + 1),
                end: demand.window.end,
            },
            cells.iter(),
        )
    } else {
        0
    };

    let service_value = i128::from(demand.per_turn.0) * i128::from(served_turns);
    let block_loss = i128::from(block.0) * i128::from(blocked_turns);
    let transit_loss = i128::from(candidate.transit_loss.0);
    // 空き手番が減ると負の寄与は小さくなるため、将来の上界には正の寄与だけを残す。
    let gain = service_value - block_loss - transit_loss;
    let upper_bound = service_value.max(0) + (-block_loss).max(0) - transit_loss;
    (funds(gain), funds(upper_bound))
}

fn free_turns<'a>(window: TurnWindow, claims: impl Iterator<Item = &'a TurnWindow>) -> u64 {
    let start = u64::from(window.start);
    let end = u64::from(window.end) + 1;
    let mut claims: Vec<_> = claims
        .map(|claim| window.intersect(*claim))
        .filter(|claim| claim.start <= claim.end)
        .collect();
    claims.sort_unstable_by_key(|claim| claim.start);

    // 占有と充足済みの区間が重なっていても、同じ手番は一度だけ除外する。
    let mut free = 0;
    let mut cursor = start;
    for claim in claims {
        free += u64::from(claim.start).saturating_sub(cursor);
        cursor = cursor.max(u64::from(claim.end) + 1);
    }
    free + end.saturating_sub(cursor)
}

fn funds(value: i128) -> FundsValue {
    FundsValue(value.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64)
}

/// 貪欲法の結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Selection {
    /// 選んだ候補の添字（`candidates` 内）。昇順。
    pub(crate) picked: Vec<usize>,
    /// 選んだ候補の追加価値の合計（選んだ順に記録を更新しながら数えた値）。
    pub(crate) total: FundsValue,
    pub(crate) spent: u32,
}

/// 選択の入力。
pub(crate) struct SelectionInput<'a> {
    pub(crate) demands: &'a [Demand],
    pub(crate) candidates: &'a [Candidate],
    pub(crate) budget: u32,
    /// 既存部隊の占有を登録済みの記録。
    pub(crate) initial: &'a Ledger,
    pub(crate) production_block_per_turn: &'a HashMap<GridPosition, FundsValue>,
}

/// 「追加価値 ÷ コスト」が最大の候補を1つずつ足す貪欲法（遅延評価で再計算を減らす）。
///
/// - 予算を超える候補、同じ手番に使用済みの施設の候補、追加価値が 0 以下の候補は足さない。
/// - 比率が同じなら添字の小さい候補を先に足す。
/// - `seeds = 0`: 貪欲法の結果と「予算内で追加価値が最大の単品」のうち、合計の大きい方を返す。
/// - `seeds = 1`: 加えて、各候補を最初の1つに固定して貪欲法を続けた結果も比べ、最大を返す。
///
/// 合計が同じなら、`seeds = 0` の貪欲法の結果を優先する。
pub(crate) fn select_greedy(input: &SelectionInput<'_>, seeds: usize) -> Selection {
    let demands: HashMap<_, _> = input
        .demands
        .iter()
        .map(|demand| (demand.id, demand))
        .collect();
    let mut queue = BinaryHeap::new();
    let mut first_picks = Vec::new();
    for (index, candidate) in input.candidates.iter().enumerate() {
        if candidate.cost > input.budget {
            continue;
        }
        let Some(&demand) = demands.get(&candidate.demand) else {
            continue;
        };
        let (gain, bound) = evaluate(
            demand,
            candidate,
            input.initial,
            input.production_block_per_turn,
        );
        if bound.0 > 0 {
            queue.push(QueueEntry {
                priority: Priority {
                    value: bound,
                    cost: candidate.cost,
                    index,
                },
                gain,
                revision: 0,
            });
        }
        if gain.0 > 0 {
            first_picks.push(Priority {
                value: gain,
                cost: candidate.cost,
                index,
            });
        }
    }

    let mut best = greedy_run(input, &demands, queue.clone(), None);
    // 同点なら通常の貪欲法を優先する。単品同士は入力順で比較する。
    for first in &first_picks {
        if first.value > best.total {
            best = Selection {
                picked: vec![first.index],
                total: first.value,
                spent: first.cost,
            };
        }
    }
    if seeds > 0 {
        for first in first_picks {
            let seeded = greedy_run(input, &demands, queue.clone(), Some(first));
            if seeded.total > best.total {
                best = seeded;
            }
        }
    }
    best
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Priority {
    value: FundsValue,
    cost: u32,
    index: usize,
}

impl Ord for Priority {
    fn cmp(&self, other: &Self) -> Ordering {
        compare_ratio((self.value, self.cost), (other.value, other.cost))
            .then_with(|| other.index.cmp(&self.index))
            .then_with(|| self.value.cmp(&other.value))
            .then_with(|| self.cost.cmp(&other.cost))
    }
}

impl PartialOrd for Priority {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

fn compare_ratio(left: (FundsValue, u32), right: (FundsValue, u32)) -> Ordering {
    (i128::from(left.0.0) * i128::from(right.1)).cmp(&(i128::from(right.0.0) * i128::from(left.1)))
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct QueueEntry {
    priority: Priority,
    gain: FundsValue,
    revision: usize,
}

fn greedy_run(
    input: &SelectionInput<'_>,
    demands: &HashMap<DemandId, &Demand>,
    mut queue: BinaryHeap<QueueEntry>,
    mut next: Option<Priority>,
) -> Selection {
    let mut ledger = input.initial.clone();
    let mut used = HashSet::default();
    let mut picked = Vec::new();
    let mut total = 0;
    let mut remaining = input.budget;
    let mut evaluated = Vec::new();

    loop {
        if let Some(choice) = next.take() {
            let candidate = &input.candidates[choice.index];
            ledger.commit(demands[&candidate.demand], candidate);
            used.insert((candidate.facility, candidate.buy_turn));
            picked.push(choice.index);
            remaining -= choice.cost;
            total += i128::from(choice.value.0);
        }

        let revision = picked.len();
        // キュー先頭の上界が暫定首位を超えうる間だけ評価する。
        // 評価済み候補は退避し、次の確定後に使う上界を保持する。
        while let Some(head) = queue.peek() {
            if next.is_some_and(|best| head.priority <= best) {
                break;
            }
            let mut entry = queue.pop().expect("先頭の候補が存在する");
            let candidate = &input.candidates[entry.priority.index];
            if candidate.cost > remaining
                || used.contains(&(candidate.facility, candidate.buy_turn))
            {
                continue;
            }
            if entry.revision != revision {
                let (gain, bound) = evaluate(
                    demands[&candidate.demand],
                    candidate,
                    &ledger,
                    input.production_block_per_turn,
                );
                entry.gain = gain;
                entry.priority.value = bound;
                entry.revision = revision;
                if bound.0 <= 0 {
                    continue;
                }
            }
            let actual = Priority {
                value: entry.gain,
                ..entry.priority
            };
            if actual.value.0 > 0 && next.is_none_or(|best| actual > best) {
                next = Some(actual);
            }
            evaluated.push(entry);
        }

        let Some(choice) = next else {
            break;
        };
        queue.extend(
            evaluated
                .drain(..)
                .filter(|entry| entry.priority.index != choice.index),
        );
    }

    picked.sort_unstable();
    Selection {
        picked,
        total: funds(total),
        spent: input.budget - remaining,
    }
}

/// 最適解の上界（診断専用）。
///
/// 被覆型の点数では、最適解は「今の解の点数 + 各候補の追加価値を予算内で比率の高い順に詰めた合計
/// （最後の1つは予算の残り割合だけ入れる）」を超えない。施設と占有の制約は無視する（緩和）ため緩い。
/// - `gains`: 今の解に対する各候補の `(追加価値, コスト)`。追加価値が 0 以下の候補は無視する。
/// - 割合で入れる分は切り捨てる。
pub(crate) fn fractional_upper_bound(
    current: FundsValue,
    gains: &[(FundsValue, u32)],
    budget: u32,
) -> FundsValue {
    let mut positive: Vec<_> = gains
        .iter()
        .copied()
        .filter(|(value, _)| value.0 > 0)
        .collect();
    positive.sort_unstable_by(|&left, &right| compare_ratio(right, left));

    let mut total = i128::from(current.0);
    let mut remaining = budget;
    for (value, cost) in positive {
        if cost <= remaining {
            total += i128::from(value.0);
            remaining -= cost;
        } else {
            total += i128::from(value.0) * i128::from(remaining) / i128::from(cost);
            break;
        }
    }
    funds(total)
}
