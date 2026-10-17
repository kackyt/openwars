//! 役割列 × 生産施設列の最小費用割当ソルバ。
//!
//! Rolling Plan の生産探索は、部隊編成（役割マルチセット）1件ごとに
//! 「どの役割をどの生産施設で作るか」を最小費用で解いている。
//! 以前の実装は施設indexの昇順・分枝限定DFSによる完全探索で、
//! 施設11・マルチセット約3,900件の局面では1手番の思考時間の約92%を
//! この再帰が占めていた（役割数mに対し最悪 n!/(n-m)! 葉）。
//! ここでは外部仕様を一切変えないまま、逐次最短路法（ポテンシャル付き
//! ハンガリアン法）で O(m·n²) に置き換える。
//!
//! # 厳守する契約（旧DFSの外部仕様そのまま）
//!
//! 入力は `cost_matrix`（m行×n列、`None` は使用禁止辺）と施設数 `n`。
//! 出力は長さmの施設indexベクタ（重複なし）か `None`。
//!
//! 1. `None` のセルは **使用禁止辺**。大きな定数で代用してはならない
//!    （代用すると「割当不能＝`None`」の返却が消えてしまう）。
//! 2. 役割数 m が施設数 n を超える場合は `None`。
//! 3. いずれかの役割行が全 `None` の場合、および全役割を別施設へ
//!    配置するマッチングが存在しない場合は `None`。
//! 4. 総コストが最小。
//! 5. 同点の場合は **施設indexベクタが辞書順最小** のものを返す。
//!
//! 5番目が単なる好みではなく契約である理由: 呼び出し元の
//! `optimize_multiset_facility_assignment` は、解いた割当の「施設の位置」から
//! 生産スロットと占領目標を決める。同点解のどれを返すかで実際にユニットが
//! 湧く場所が変わり、以降の配備・進軍がまるごと変わってしまう。
//! 旧DFSは「施設indexの昇順走査 ＋ 厳密改善時のみ最良解を更新」により
//! 常に辞書順最小の最適解を返していたので、新実装も同じ解を選ぶ必要がある。

/// 未割当を表す番兵値。
const UNASSIGNED: usize = usize::MAX;

/// 旧DFSが使用済み施設を `u32` ビットマスクで保持していたことに由来する施設数上限。
const DFS_MAX_FACILITIES: usize = 32;

/// 役割列（行）と生産施設列（列）の最小費用割当を解く。
///
/// 同点時は施設indexベクタが辞書順最小のものを返す（モジュールコメント参照）。
pub(super) fn solve_min_cost_assignment(
    cost_matrix: &[Vec<Option<u64>>],
    num_facilities: usize,
) -> Option<Vec<usize>> {
    solve_with_stats(cost_matrix, num_facilities).0
}

/// 本体。第2要素は内部ループの実行回数で、計算量ガードのテストから参照する。
fn solve_with_stats(
    cost_matrix: &[Vec<Option<u64>>],
    num_facilities: usize,
) -> (Option<Vec<usize>>, u64) {
    let m = cost_matrix.len();
    let n = num_facilities;
    let mut ops: u64 = 0;

    // 空編成は「割当0件で成立」。旧DFSも best_cost=0 で空ベクタを返していた。
    if m == 0 {
        return (Some(Vec::new()), ops);
    }
    // 役割数が施設数を超えると、全役割を別施設へ配置できない。
    if m > n {
        return (None, ops);
    }

    // 各行の最大コストを集計して合成コストの上限を見積もる。
    // 全 `None` の行があれば、その役割を置ける施設が無いので解なし。
    let mut max_total_cost: u128 = 0;
    for row in cost_matrix {
        let Some(row_max) = row.iter().take(n).flatten().copied().max() else {
            return (None, ops);
        };
        max_total_cost = max_total_cost.saturating_add(row_max as u128);
    }

    // 合成コスト = 実コスト * SCALE + 辞書順タイブレーク項、SCALE = n^m。
    // タイブレーク項の総和は Σ_{k<m} (n-1)·n^k = n^m - 1 < SCALE なので
    // 実コストの桁へ繰り上がらない。よって合成コストの最小化が
    // 「総コスト最小 → 施設indexベクタの辞書順最小」と完全に一致する。
    let scale = match (n as u128).checked_pow(m as u32) {
        Some(scale) => scale,
        None => return (solve_by_dfs(cost_matrix, n), ops),
    };
    // 旧DFSは `saturating_add` ＋ `next_cost >= best_cost` の枝刈りにより、
    // 総コストが u64::MAX に達する経路を必ず捨てていた。ここでは
    // そもそも飽和し得ない範囲でだけ新経路を使い、それ以外は旧DFSに委ねる。
    if max_total_cost >= u64::MAX as u128 {
        return (solve_by_dfs(cost_matrix, n), ops);
    }
    match max_total_cost
        .checked_mul(scale)
        .and_then(|bound| bound.checked_add(scale))
    {
        Some(bound) if bound <= i128::MAX as u128 => {}
        // 合成コストが i128 に収まらない極端な規模。旧DFSにフォールバックする。
        _ => return (solve_by_dfs(cost_matrix, n), ops),
    }

    // pow[k] = n^k。役割 i のタイブレーク重みは pow[m - 1 - i]
    // （＝施設indexを基数nの各桁として並べたもの）。
    let mut pow: Vec<u128> = Vec::with_capacity(m);
    pow.push(1);
    for k in 1..m {
        pow.push(pow[k - 1] * n as u128);
    }

    // 合成コスト表を m×n のフラット配列として先に作る。内側ループは O(m·n²) 回
    // 参照されるため、そのつど計算すると走査コストが跳ね上がる。`None` は使用禁止辺。
    let mut composite = vec![None; m * n];
    for (row, cells) in cost_matrix.iter().enumerate() {
        let weight = pow[m - 1 - row];
        for (col, cell) in cells.iter().take(n).enumerate() {
            if let Some(cost) = cell {
                composite[row * n + col] =
                    Some((*cost as u128 * scale + col as u128 * weight) as i128);
            }
        }
    }

    const INF: i128 = i128::MAX;

    // ポテンシャル。被約コスト rc(i,j) = c(i,j) - pot_row[i] - pot_col[j] を
    // 常に非負（マッチ辺は 0）に保つことで、Dijkstra が使える。
    let mut pot_row = vec![0i128; m];
    let mut pot_col = vec![0i128; n];
    let mut col_match = vec![UNASSIGNED; n];
    let mut row_match = vec![UNASSIGNED; m];

    // 役割を1件ずつマッチングへ追加し、そのつど最短増加路で拡張する
    // （逐次最短路法。各段階で「その本数での最小費用マッチング」が保たれる）。
    // Dijkstra の作業配列。役割ごとに確保し直すとマルチセット数（数千件）×役割数の
    // ヒープ確保になるため、1本だけ持って毎ラウンド初期化する。
    let mut dist = vec![INF; n];
    let mut prev_row = vec![UNASSIGNED; n];
    let mut settled = vec![false; n];

    for start_row in 0..m {
        dist.iter_mut().for_each(|d| *d = INF);
        prev_row.iter_mut().for_each(|p| *p = UNASSIGNED);
        settled.iter_mut().for_each(|s| *s = false);

        let base = start_row * n;
        for col in 0..n {
            ops += 1;
            if let Some(cost) = composite[base + col] {
                let reduced = cost - pot_row[start_row] - pot_col[col];
                if reduced < dist[col] {
                    dist[col] = reduced;
                    prev_row[col] = start_row;
                }
            }
        }

        let mut free_col = UNASSIGNED;
        loop {
            // 未確定列のうち最小距離のものを確定させる（密グラフなので線形走査）。
            let mut next_col = UNASSIGNED;
            let mut next_dist = INF;
            for (col, (&d, &is_settled)) in dist.iter().zip(settled.iter()).enumerate() {
                ops += 1;
                if !is_settled && d < next_dist {
                    next_dist = d;
                    next_col = col;
                }
            }
            if next_col == UNASSIGNED {
                // 到達可能な列が尽きた = 増加路なし。
                break;
            }
            settled[next_col] = true;
            if col_match[next_col] == UNASSIGNED {
                free_col = next_col;
                break;
            }
            // マッチ済みの列からは、その相手役割を経由して他の列へ伸ばせる
            // （マッチ辺の被約コストは 0 なので距離は増えない）。
            let via_row = col_match[next_col];
            let via_base = via_row * n;
            for col in 0..n {
                ops += 1;
                if settled[col] {
                    continue;
                }
                let Some(cost) = composite[via_base + col] else {
                    continue;
                };
                let candidate = next_dist + (cost - pot_row[via_row] - pot_col[col]);
                if candidate < dist[col] {
                    dist[col] = candidate;
                    prev_row[col] = via_row;
                }
            }
        }

        if free_col == UNASSIGNED {
            // 全役割を別施設へ配置する割当が存在しない。
            return (None, ops);
        }
        let delta = dist[free_col];

        // ポテンシャル更新。未到達の行・列は delta でクランプする。これにより
        // 被約コストの非負性が保たれ、かつ未マッチ列のポテンシャルが 0 のまま
        // 維持される（＝「最も近い空き列」を素直に選んでよくなる）。
        for (row, pot) in pot_row.iter_mut().enumerate() {
            let reached = if row == start_row {
                0
            } else {
                match row_match[row] {
                    col if col != UNASSIGNED && settled[col] => dist[col],
                    _ => delta,
                }
            };
            *pot += delta - reached;
        }
        for ((pot, &d), &is_settled) in pot_col.iter_mut().zip(dist.iter()).zip(settled.iter()) {
            let reached = if is_settled { d } else { delta };
            *pot += reached - delta;
        }

        // 増加路に沿ってマッチングを反転させる。
        let mut col = free_col;
        loop {
            let row = prev_row[col];
            let displaced = row_match[row];
            col_match[col] = row;
            row_match[row] = col;
            if row == start_row {
                break;
            }
            col = displaced;
        }
    }

    (Some(row_match), ops)
}

/// 旧実装（施設indexの昇順・分枝限定DFS）。
///
/// 合成コストが表現できない極端な規模でのフォールバックと、
/// 差分テストのオラクルを兼ねる。
fn solve_by_dfs(cost_matrix: &[Vec<Option<u64>>], num_facilities: usize) -> Option<Vec<usize>> {
    if cost_matrix.len() > num_facilities {
        return None;
    }
    // 旧実装は使用済み施設を u32 ビットマスクで持つため 33 施設以上を扱えない。
    // ここへ来るのは「合成コストが i128 を溢れる規模」に限られ、それは
    // 役割26件以上かつ施設33件以上でしか起こらないため実質到達しない。
    // 万一到達しても panic させず、その編成を候補から落とすだけに留める。
    if num_facilities > DFS_MAX_FACILITIES {
        return None;
    }

    let mut best_cost = u64::MAX;
    let mut best_assignment = Vec::new();
    let mut current_assignment = Vec::with_capacity(cost_matrix.len());
    dfs_min_cost_assignment(
        0,
        0,
        0,
        &mut current_assignment,
        &mut best_cost,
        &mut best_assignment,
        cost_matrix,
        num_facilities,
    );

    if best_cost == u64::MAX {
        return None;
    }
    Some(best_assignment)
}

/// 役割列と施設列の最小費用二部マッチング（Branch and Bound）。
#[allow(clippy::too_many_arguments)]
fn dfs_min_cost_assignment(
    role_idx: usize,
    used_facilities_mask: u32,
    current_cost: u64,
    current_assignment: &mut Vec<usize>,
    best_cost: &mut u64,
    best_assignment: &mut Vec<usize>,
    cost_matrix: &[Vec<Option<u64>>],
    num_facilities: usize,
) {
    if role_idx == cost_matrix.len() {
        if current_cost < *best_cost {
            *best_cost = current_cost;
            *best_assignment = current_assignment.clone();
        }
        return;
    }

    for fac_idx in 0..num_facilities {
        if (used_facilities_mask & (1 << fac_idx)) != 0 {
            continue;
        }
        let Some(cost) = cost_matrix[role_idx][fac_idx] else {
            continue;
        };
        let next_cost = current_cost.saturating_add(cost);
        if next_cost >= *best_cost {
            continue;
        }
        current_assignment.push(fac_idx);
        dfs_min_cost_assignment(
            role_idx + 1,
            used_facilities_mask | (1 << fac_idx),
            next_cost,
            current_assignment,
            best_cost,
            best_assignment,
            cost_matrix,
            num_facilities,
        );
        current_assignment.pop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// テスト用の決定的擬似乱数（xorshift64）。外部クレートを増やさないための最小実装。
    struct Xorshift(u64);

    impl Xorshift {
        fn next_u64(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.0 = x;
            x
        }

        fn below(&mut self, bound: u64) -> u64 {
            self.next_u64() % bound
        }
    }

    /// 割当の総コストを求める。禁止辺を踏んでいれば `None`。
    fn total_cost(cost_matrix: &[Vec<Option<u64>>], assignment: &[usize]) -> Option<u64> {
        let mut sum = 0u64;
        for (row, &col) in assignment.iter().enumerate() {
            sum = sum.saturating_add(cost_matrix[row][col]?);
        }
        Some(sum)
    }

    #[test]
    fn matches_dfs_oracle_on_randomized_matrices() {
        let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
        let mut rng = Xorshift(0x5DEE_CE66_D1B2_7F01);
        // オラクル（指数時間DFS）が現実的に回る n ≤ 8 に限定する。
        for n in 1..=8usize {
            for m in 0..=n {
                // 密度とコスト幅を変えて、疎行列・同値多発の双方を踏む。
                for &(none_ratio, cost_span) in &[
                    (0u64, 1u64),
                    (0, 5),
                    (0, 1000),
                    (40, 5),
                    (70, 3),
                    (90, 1000),
                ] {
                    for _ in 0..12 {
                        let matrix: Vec<Vec<Option<u64>>> = (0..m)
                            .map(|_| {
                                (0..n)
                                    .map(|_| {
                                        if rng.below(100) < none_ratio {
                                            None
                                        } else {
                                            Some(rng.below(cost_span))
                                        }
                                    })
                                    .collect()
                            })
                            .collect();

                        let actual = solve_min_cost_assignment(&matrix, n);
                        let expected = solve_by_dfs(&matrix, n);
                        assert_eq!(
                            actual, expected,
                            "n={n} m={m} none_ratio={none_ratio} span={cost_span} matrix={matrix:?}"
                        );

                        // 戻り値が施設indexの重複しない割当であることも確認する。
                        if let Some(assignment) = actual {
                            assert_eq!(assignment.len(), m);
                            let mut sorted = assignment.clone();
                            sorted.sort_unstable();
                            sorted.dedup();
                            assert_eq!(sorted.len(), m, "施設が重複している: {assignment:?}");
                            assert!(total_cost(&matrix, &assignment).is_some(), "禁止辺を踏んだ");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn returns_lexicographically_smallest_assignment_on_ties() {
        let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
        // 全要素が同一コストなら、辞書順最小の [0, 1, 2] でなければならない。
        // ここが崩れると同点時に生産施設が変わり、下流の配備先まで変化する。
        let matrix = vec![vec![Some(7); 4], vec![Some(7); 4], vec![Some(7); 4]];
        assert_eq!(solve_min_cost_assignment(&matrix, 4), Some(vec![0, 1, 2]));
    }

    #[test]
    fn prefers_smaller_total_cost_over_lexicographic_order() {
        let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
        // 辞書順は総コストより優先されない。[0,1] は 10+10=20、[1,0] は 1+1=2。
        let matrix = vec![vec![Some(10), Some(1)], vec![Some(1), Some(10)]];
        assert_eq!(solve_min_cost_assignment(&matrix, 2), Some(vec![1, 0]));
    }

    #[test]
    fn rejects_more_roles_than_facilities() {
        let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
        let matrix = vec![vec![Some(1)], vec![Some(1)]];
        assert_eq!(solve_min_cost_assignment(&matrix, 1), None);
    }

    #[test]
    fn rejects_row_without_any_usable_facility() {
        let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
        let matrix = vec![vec![Some(1), Some(2)], vec![None, None]];
        assert_eq!(solve_min_cost_assignment(&matrix, 2), None);
        assert_eq!(solve_by_dfs(&matrix, 2), None);
    }

    #[test]
    fn rejects_when_forbidden_edges_block_a_complete_matching() {
        let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
        // 2役割とも施設0しか使えないので、別施設への同時配置は不可能。
        // 禁止辺を大きな定数で代用していると、ここで誤って Some を返してしまう。
        let matrix = vec![vec![Some(1), None], vec![Some(1), None]];
        assert_eq!(solve_min_cost_assignment(&matrix, 2), None);
        assert_eq!(solve_by_dfs(&matrix, 2), None);
    }

    #[test]
    fn handles_empty_and_single_cell_matrices() {
        let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
        let empty: Vec<Vec<Option<u64>>> = Vec::new();
        assert_eq!(solve_min_cost_assignment(&empty, 3), Some(Vec::new()));
        assert_eq!(solve_by_dfs(&empty, 3), Some(Vec::new()));

        let single = vec![vec![Some(42)]];
        assert_eq!(solve_min_cost_assignment(&single, 1), Some(vec![0]));

        let single_forbidden = vec![vec![None]];
        assert_eq!(solve_min_cost_assignment(&single_forbidden, 1), None);
    }

    #[test]
    fn saturated_square_matrix_uses_every_facility() {
        let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
        // m == n の飽和ケース。対角が明確に安いので一意に定まる。
        let matrix = vec![
            vec![Some(1), Some(9), Some(9)],
            vec![Some(9), Some(1), Some(9)],
            vec![Some(9), Some(9), Some(1)],
        ];
        assert_eq!(solve_min_cost_assignment(&matrix, 3), Some(vec![0, 1, 2]));
    }

    #[test]
    fn stays_within_polynomial_operation_budget() {
        let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
        // 全同値の 12×12。旧DFSは 12! ≈ 4.8e8 葉を走査しうるが、
        // 逐次最短路法は m·n² のオーダーに収まる。
        // 壁時計時間ではなく内部ループ回数で判定し、CIの不安定要因にしない。
        let n = 12usize;
        let matrix = vec![vec![Some(5u64); n]; n];
        let (assignment, ops) = solve_with_stats(&matrix, n);

        assert_eq!(assignment, Some((0..n).collect::<Vec<_>>()));
        let budget = 4 * (n * n * n) as u64;
        assert!(
            ops <= budget,
            "内部ループ回数が多すぎる: ops={ops} budget={budget}"
        );
    }

    #[test]
    fn falls_back_to_dfs_when_composite_cost_overflows() {
        let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
        // n^m が u128 を溢れる規模ではフォールバック経路に入る。
        // 施設数が旧DFSの u32 マスク上限を超えるため、panic せず None を返す。
        let n = 40usize;
        let m = 30usize;
        let matrix = vec![vec![Some(1u64); n]; m];
        assert!((n as u128).checked_pow(m as u32).is_none());
        assert_eq!(solve_min_cost_assignment(&matrix, n), None);
    }
}
