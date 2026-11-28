//! 小さな最小費用割当（ハンガリー法）。AI の割当問題（占領役と拠点など）で共通に使う。
//!
//! 計算量は O(n²m)。小規模マップの割当（n, m ≤ 20 程度）では無視できる。

/// 最小費用の割当（ハンガリー法、O(n²m)）。行 i に割り当てた列を返す（割り当てなしは None）。
///
/// 行が列より多い場合は、費用 `unassigned` の「割り当てなし」の列を補って正方以上にする。
/// 割当てられない組には `unassigned` を入れておけば、可能な組の数が最大の割当の中で費用が最小になる
/// （`unassigned` が他の費用の合計より十分大きいとき）。
pub(crate) fn min_cost_assignment(costs: &[Vec<u64>], unassigned: u64) -> Vec<Option<usize>> {
    let rows = costs.len();
    if rows == 0 {
        return Vec::new();
    }
    let real_columns = costs[0].len();
    let columns = real_columns.max(rows);
    let cost = |i: usize, j: usize| -> i128 {
        if j < real_columns {
            i128::from(costs[i][j])
        } else {
            i128::from(unassigned)
        }
    };
    // 1 始まりのポテンシャル法（e-maxx の実装に準拠）
    let mut u = vec![0_i128; rows + 1];
    let mut v = vec![0_i128; columns + 1];
    let mut owner = vec![0_usize; columns + 1];
    let mut way = vec![0_usize; columns + 1];
    for i in 1..=rows {
        owner[0] = i;
        let mut j0 = 0;
        let mut min_v = vec![i128::MAX; columns + 1];
        let mut used = vec![false; columns + 1];
        loop {
            used[j0] = true;
            let i0 = owner[j0];
            let mut delta = i128::MAX;
            let mut j1 = 0;
            for j in 1..=columns {
                if used[j] {
                    continue;
                }
                let current = cost(i0 - 1, j - 1) - u[i0] - v[j];
                if current < min_v[j] {
                    min_v[j] = current;
                    way[j] = j0;
                }
                if min_v[j] < delta {
                    delta = min_v[j];
                    j1 = j;
                }
            }
            for j in 0..=columns {
                if used[j] {
                    u[owner[j]] += delta;
                    v[j] -= delta;
                } else {
                    min_v[j] -= delta;
                }
            }
            j0 = j1;
            if owner[j0] == 0 {
                break;
            }
        }
        loop {
            let j1 = way[j0];
            owner[j0] = owner[j1];
            j0 = j1;
            if j0 == 0 {
                break;
            }
        }
    }
    let mut result = vec![None; rows];
    for j in 1..=columns {
        if owner[j] != 0 && j - 1 < real_columns {
            result[owner[j] - 1] = Some(j - 1);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 全組合せを試した最小費用（テスト用の小さな行列だけで使う）。
    fn brute_force(costs: &[Vec<u64>]) -> u64 {
        fn go(costs: &[Vec<u64>], row: usize, used: &mut Vec<bool>) -> u64 {
            if row == costs.len() {
                return 0;
            }
            let mut best = u64::MAX;
            for j in 0..costs[row].len() {
                if !used[j] {
                    used[j] = true;
                    best = best.min(costs[row][j] + go(costs, row + 1, used));
                    used[j] = false;
                }
            }
            best
        }
        go(costs, 0, &mut vec![false; costs[0].len()])
    }

    #[test]
    fn assignment_matches_the_optimum_on_small_matrices() {
        let cases = vec![
            vec![vec![4, 1, 3], vec![2, 0, 5], vec![3, 2, 2]],
            vec![vec![2, 3, 9, 1], vec![4, 1, 1, 7], vec![8, 5, 2, 3]],
            vec![vec![1, 1], vec![1, 1]],
        ];
        for costs in cases {
            let result = min_cost_assignment(&costs, u64::MAX / 4);
            let total: u64 = result
                .iter()
                .enumerate()
                .map(|(i, j)| costs[i][j.unwrap()])
                .sum();
            assert_eq!(total, brute_force(&costs), "{costs:?}");
            // 同じ列を 2 行に割り当てない
            let mut seen = std::collections::HashSet::new();
            assert!(result.iter().flatten().all(|j| seen.insert(*j)));
        }
    }

    #[test]
    fn surplus_rows_are_left_unassigned() {
        // 行 3・列 2: 1 行は割り当てなし。費用 1+1 の組を選ぶ
        let costs = vec![vec![1, 9], vec![9, 1], vec![5, 5]];
        let result = min_cost_assignment(&costs, u64::MAX / 4);
        assert_eq!(result, vec![Some(0), Some(1), None]);
    }

    #[test]
    fn maximizes_the_number_of_feasible_pairs_before_cost() {
        // 0 番は列 0 だけ可能、1 番は列 0・1 が可能。安い (1,0) を取ると 0 番が余るので、
        // 可能な組の数を優先して (0,0),(1,1) を選ぶ
        let none = u64::MAX / 4;
        let costs = vec![vec![3, none], vec![1, 2]];
        assert_eq!(min_cost_assignment(&costs, none), vec![Some(0), Some(1)]);
    }
}
