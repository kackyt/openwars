//! 物件単位の「行動フェーズETA」と `DirectCapture` / `Interdict` / `Recapture` の
//! 実行可能性を判定する純粋関数群。
//!
//! マップ名・兵種名・固定スコア・マップ寸法分岐を含まない。ここに現れるのは
//! 「到着フェーズ」「占領力」「耐久値」「撃破可否」といった盤面から観測できる量だけである。
//!
//! 時刻はターン数ではなく `ActionPhase`（round, player_order）で表し、
//! 同じターン数でも先手（player_order が小さい方）が先着するよう辞書順で比較する。

use crate::ai::turn_distance::{ActionTurnDistanceCache, calculate_action_distance_to_range};
use crate::components::PlayerId;
use crate::resources::master_data::MasterDataRegistry;
use crate::resources::{Map, MovementType};
use std::collections::HashMap;

/// 行動フェーズ。`(round, player_order)` の辞書順で比較する。
///
/// `round` はターン番号、`player_order` はそのラウンド内の手番順（0 が先手、1 が後手）。
/// 「同じ 2 ターン到着」でも先手と後手を同着にしないための値オブジェクト。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ActionPhase {
    /// ラウンド（ターン番号）
    pub round: u32,
    /// ラウンド内の手番順。0 が先手、1 が後手。
    pub player_order: u32,
}

impl ActionPhase {
    /// 指定ラウンド・手番順のフェーズを作る。
    pub fn new(round: u32, player_order: u32) -> Self {
        Self {
            round,
            player_order,
        }
    }

    /// 同じ手番順のまま `n` 個先の自分フェーズへ進める。`n == 0` は現フェーズそのもの。
    pub fn after_own_phases(&self, n: u32) -> Self {
        Self {
            round: self.round + n,
            player_order: self.player_order,
        }
    }
}

/// 拠点を占領しきるまでに必要な占領行動回数。
///
/// 占領力（`capture_power`）は 1 行動あたりのポイント減少量で、
/// エンジンは `(HP の表示値) * 10` を採用する（`systems/property.rs`）。
pub fn capture_actions_needed(durability: u32, capture_power: u32) -> u32 {
    durability.div_ceil(capture_power.max(1))
}

/// 拠点上へ到達するフェーズ。
///
/// `arrival_turns` は「拠点上へ到達するまでの自分の行動フェーズ数」。0 は既に到達済み。
/// 移動は 1 フェーズ目を現在フェーズとして数えるため、`n >= 1` は `n - 1` 個先の
/// 自分フェーズへ着く。
pub fn arrival_phase(current: ActionPhase, arrival_turns: u32) -> ActionPhase {
    current.after_own_phases(arrival_turns.saturating_sub(1))
}

/// 占領役が拠点を占領し終えるフェーズ。
///
/// 最初の占領行動は到着フェーズに行い、残りの `(占領回数 - 1)` 回を後続の自分フェーズで行う。
pub fn direct_capture_phase(
    current: ActionPhase,
    arrival_turns: u32,
    durability: u32,
    capture_power: u32,
) -> ActionPhase {
    let start = arrival_phase(current, arrival_turns);
    let actions = capture_actions_needed(durability, capture_power);
    start.after_own_phases(actions.saturating_sub(1))
}

/// 直接占領競争の勝敗。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureVerdict {
    /// 自軍が先に占領完了する。
    DirectCaptureWins,
    /// 敵が先に占領完了する（直取りは成立しない）。
    EnemyWinsFirst,
}

/// 自軍・敵の占領完了フェーズを比較して、直取りが成立するかを判定する。
pub fn contest_verdict(
    my_completion: ActionPhase,
    enemy_completion: ActionPhase,
) -> CaptureVerdict {
    if my_completion < enemy_completion {
        CaptureVerdict::DirectCaptureWins
    } else {
        CaptureVerdict::EnemyWinsFirst
    }
}

/// 攻撃役が敵の占領完了前に攻撃を入れられるか（`Interdict`）。
///
/// `attack_ready_phase` は攻撃役が射程内へ到達するフェーズ。敵の占領完了と同時か
/// それより後では妨害にならないため、strict に「より前」を要求する。
pub fn interdict_feasible(attack_ready_phase: ActionPhase, enemy_completion: ActionPhase) -> bool {
    attack_ready_phase < enemy_completion
}

/// 撃破役と占領役がそろって初めて成立する奪還（`Recapture`）の完了フェーズ。
///
/// - `eliminator_ready`: 撃破役が敵を排除できるフェーズ（排除不能なら `None`）
/// - `capture_arrival`: 占領役が拠点上へ到達するフェーズ（到達不能なら `None`）
///
/// どちらかが欠ける計画は完了に数えず `None` を返す。敵を排除してからでないと
/// 占領役は拠点上へ入れないため、撃破完了と占領役到着の遅い方から占領を始める。
pub fn recapture_phase(
    eliminator_ready: Option<ActionPhase>,
    capture_arrival: Option<ActionPhase>,
    durability: u32,
    capture_power: u32,
) -> Option<ActionPhase> {
    let eliminator = eliminator_ready?;
    let capture_start = capture_arrival?;
    let start = eliminator.max(capture_start);
    let actions = capture_actions_needed(durability, capture_power);
    Some(start.after_own_phases(actions.saturating_sub(1)))
}

/// 直取り目的の生産判断。速度だけで兵種を選ばない。
///
/// 「買わない場合」と「買った場合」の自軍完了フェーズ、および敵の完了フェーズから、
/// 買うことで現状より早まり、かつ敵より先に占領完了できる場合だけ採用する。
/// 速くなっても敵より後着のままなら `false` を返す。
pub fn buy_for_direct_capture(
    without: ActionPhase,
    with: ActionPhase,
    enemy_completion: ActionPhase,
) -> bool {
    with < without && with < enemy_completion
}

/// 直接占領を完了するまでの「自分フェーズ数」を相対値で返す。
///
/// 到着フェーズに最初の占領を行い、残り `(占領回数 - 1)` 回を後続フェーズで行うため、
/// `arrival_turns + capture_actions_needed(durability, power) - 1` になる。
pub fn completion_offset(arrival_turns: u32, durability: u32, capture_power: u32) -> u32 {
    arrival_turns
        .saturating_add(capture_actions_needed(durability, capture_power).saturating_sub(1))
}

/// 生産予定turnと、生産地点から行動成立位置までに必要な行動フェーズ数を、
/// 現在手番からの成立offsetへ変換する。
///
/// `action_distance == 0` でも生産した手番には行動できないため次フェーズ（+1）になる。
/// 一方 `action_distance == 1` は、その次フェーズの移動後に直接攻撃・占領できるので
/// さらに1を足してはならない。従来の `build_turn + 1 + action_distance` は、移動を
/// 始められる次フェーズを距離側と生産待機側で二重に数えていた。
pub fn produced_action_offset(build_turn: u32, action_distance: u32) -> u32 {
    build_turn.saturating_add(action_distance.max(1))
}

/// 直接占領競争の相対判定。
///
/// 同じオフセットでも先手（`order` が小さい方）が先に完了する。`order` は 0 が先手、
/// 1 が後手。敵の占領完了と同じかそれより後では直取りできないため、strict に比較する。
pub fn direct_capture_race_winner(
    my_arrival: u32,
    my_power: u32,
    my_order: u32,
    enemy_arrival: u32,
    enemy_power: u32,
    enemy_order: u32,
    durability: u32,
) -> bool {
    let mine = completion_offset(my_arrival, durability, my_power);
    let theirs = completion_offset(enemy_arrival, durability, enemy_power);
    mine < theirs || (mine == theirs && my_order < enemy_order)
}

/// 占領候補の適合度を「既に割り当て済みの占領役より何ターン早く到着するか」で表す。
///
/// `candidate_arrival` は候補（生産後は行動不能なので +1 済み）が物件へ到達するまでの
/// 自分フェーズ数。`committed_best` は既存占領役の最短到達フェーズ数（未割り当てなら `None`）。
///
/// 既存より遅い・同等の候補は `None`（支配フェーズを改善しない＝機械的に買い増さない）。
pub fn capture_speed_fitness(candidate_arrival: u32, committed_best: Option<u32>) -> Option<f32> {
    let best = committed_best.unwrap_or(u32::MAX);
    if candidate_arrival >= best {
        return None;
    }
    Some(best.saturating_sub(candidate_arrival).max(1) as f32)
}

/// 現在HPから 1 占領行動あたりの占領力（アクションパワー）を返す。
///
/// エンジンは `get_display_hp() * 10 = ((current + 9) / 10) * 10` を占領力とする。
/// 満タン（内部HP 100）なら 100、ダメージを受けると 10 刻みで下がる。
pub fn capture_power_from_hp(hp: u32) -> u32 {
    ((hp.saturating_add(9)) / 10) * 10
}

/// 完了フェーズ（相対オフセット）の先後判定。同じオフセットでも先手（`order` が小さい方）が
/// 先に完了する。`order` は 0 が先手、1 が後手。
pub fn completion_race_won(
    my_completion: u32,
    my_order: u32,
    enemy_completion: u32,
    enemy_order: u32,
) -> bool {
    my_completion < enemy_completion
        || (my_completion == enemy_completion && my_order < enemy_order)
}

/// 地形・移動力・燃料を考慮して、指定ユニットが拠点上へ到達するまでの自分の行動フェーズ数を返す。
/// 到達不能なら `None`。
///
/// 占領は「その物件マスに立つ」ことなので、射程帯は 0..=0 で測る。移動力繰越禁止・
/// 移動後間接攻撃禁止は `calculate_action_distance_to_range` が含む。占有・ZOC は
/// ここでは考慮しない（分析文書の map_2 ETA も「占有なしの経路計算」で測っている）。
/// 敵が物件上に立つ場合は排除後に進入するため、単純な速度競争の入力としては
/// 「地形的に何フェーズで到達できるか」だけを返す。
///
/// 既存ユニットの移動フェーズ数を返すため、生産直後（この手番に動けない）候補は
/// 呼び出し側で 1 を足す。
#[allow(clippy::too_many_arguments)]
pub fn capture_arrival_turns(
    map: &Map,
    registry: &MasterDataRegistry,
    start: (usize, usize),
    target: (usize, usize),
    movement_type: MovementType,
    max_mp: u32,
    max_fuel: u32,
    player_id: PlayerId,
    cache: &mut ActionTurnDistanceCache,
) -> Option<u32> {
    // 燃料 0 は「燃料制限なし」のセンチネル（テスト用モック等）。実ユニットは
    // 非ゼロ燃料を持ち、経路探索が燃料切れを正しく扱う。
    let effective_fuel = if max_fuel == 0 { u32::MAX } else { max_fuel };
    let empty_occupancy = HashMap::new();
    calculate_action_distance_to_range(
        map,
        registry,
        &empty_occupancy,
        start,
        target,
        movement_type,
        max_mp,
        effective_fuel,
        0,
        0,
        player_id,
        cache,
    )
    .map(|distance| distance.turns)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resources::GridTopology;
    use crate::resources::Terrain;

    #[test]
    fn produced_unit_moves_and_acts_on_the_next_phase_without_double_counting() {
        assert_eq!(produced_action_offset(0, 0), 1);
        assert_eq!(produced_action_offset(0, 1), 1);
        assert_eq!(produced_action_offset(0, 2), 2);
        assert_eq!(produced_action_offset(3, 1), 4);
    }

    /// map_2 初期局面では 3 中立拠点の `DirectCapture` がすべて「V4 後着」になる。
    ///
    /// 到着フェーズ数は分析文書（`docs/07-project-management/plans/
    /// v4-v200-small-map-expansion-analysis.md`）の地形込み実測 ETA をそのまま用いる:
    ///   中央 (7,5): V200 1T / V4 2T
    ///   左  (3,10): V200 2T / V4 2T（同ターンだが先手 V200 が先着）
    ///   右  (10,3): V200 2T / V4 2T（同ターンだが先手 V200 が先着）
    #[test]
    fn map2_three_neutral_properties_are_all_lost_to_first_mover() {
        let v200 = ActionPhase::new(1, 0); // 先手
        let v4 = ActionPhase::new(1, 1); // 後手

        let cases = [
            (1u32, 2u32), // 中央 (7,5)
            (2u32, 2u32), // 左 (3,10)
            (2u32, 2u32), // 右 (10,3)
        ];
        for (v200_arrival, v4_arrival) in cases {
            let enemy_done = direct_capture_phase(v200, v200_arrival, 200, 100);
            let my_done = direct_capture_phase(v4, v4_arrival, 200, 100);
            assert!(
                my_done > enemy_done,
                "V4 は後着でなければならない: my={my_done:?} enemy={enemy_done:?}"
            );
            assert_eq!(
                contest_verdict(my_done, enemy_done),
                CaptureVerdict::EnemyWinsFirst
            );
        }
    }

    /// 移動ターン数が同じでも、先手（player_order が小さい方）が先着する。
    #[test]
    fn same_arrival_turns_first_mover_completes_first() {
        let first = ActionPhase::new(1, 0);
        let second = ActionPhase::new(1, 1);

        let first_done = direct_capture_phase(first, 2, 200, 100);
        let second_done = direct_capture_phase(second, 2, 200, 100);

        assert_eq!(first_done, ActionPhase::new(3, 0));
        assert_eq!(second_done, ActionPhase::new(3, 1));
        assert!(first_done < second_done);
        assert_eq!(
            contest_verdict(second_done, first_done),
            CaptureVerdict::EnemyWinsFirst
        );
    }

    /// 占領完了前に攻撃できない対象は `Interdict` にしない。
    #[test]
    fn interdict_requires_attack_before_capture_completion() {
        // 敵（先手）は 1 フェーズで到着し、満タン歩兵（占領力 100）で都市（耐久 200）を
        // (2,0) に占領完了する。
        let enemy_done = direct_capture_phase(ActionPhase::new(1, 0), 1, 200, 100);
        assert_eq!(enemy_done, ActionPhase::new(2, 0));

        // 完了と同時かそれより後では妨害にならない。
        assert!(!interdict_feasible(ActionPhase::new(2, 0), enemy_done));
        assert!(!interdict_feasible(ActionPhase::new(3, 1), enemy_done));

        // 完了より前のフェーズなら妨害可能。
        assert!(interdict_feasible(ActionPhase::new(1, 1), enemy_done));
    }

    /// 撃破役と占領役がそろわない計画は `Recapture` 完了として数えない。
    #[test]
    fn recapture_requires_both_eliminator_and_capture_unit() {
        let durability = 200;
        let power = 100;

        // 撃破役なし → 完了に数えない。
        assert_eq!(
            recapture_phase(None, Some(ActionPhase::new(2, 0)), durability, power),
            None
        );
        // 占領役が到達できない → 完了に数えない。
        assert_eq!(
            recapture_phase(Some(ActionPhase::new(2, 0)), None, durability, power),
            None
        );

        // 両方そろう → 完了フェーズが決まる。
        // start = max(撃破 (2,0), 占領到着 (2,0)) = (2,0)、占領 2 回 → (3,0)。
        let done = recapture_phase(
            Some(ActionPhase::new(2, 0)),
            Some(ActionPhase::new(2, 0)),
            durability,
            power,
        );
        assert_eq!(done, Some(ActionPhase::new(3, 0)));

        // 占領役が撃破より遅いなら、そちらまで待つ。
        // start = max(撃破 (2,0), 占領到着 (4,0)) = (4,0) → (5,0)。
        let done_later = recapture_phase(
            Some(ActionPhase::new(2, 0)),
            Some(ActionPhase::new(4, 0)),
            durability,
            power,
        );
        assert_eq!(done_later, Some(ActionPhase::new(5, 0)));
    }

    /// 速い兵種でも支配フェーズが改善しなければ、速度を理由に生産しない。
    #[test]
    fn faster_unit_that_does_not_flip_the_race_is_not_bought_for_speed() {
        // 敵は (3,1) に占領完了する。
        let enemy_done = ActionPhase::new(3, 1);

        // 現状の占領役: 到着 4 フェーズ → 完了 (5,0)。
        let without = direct_capture_phase(ActionPhase::new(1, 0), 4, 200, 100);
        // 速い兵種: 到着 3 フェーズ → 完了 (4,0)。
        let with = direct_capture_phase(ActionPhase::new(1, 0), 3, 200, 100);

        assert_eq!(without, ActionPhase::new(5, 0));
        assert_eq!(with, ActionPhase::new(4, 0));

        // 速くはなったがなお敵より後 → 速度を理由に買わない。
        assert!(!buy_for_direct_capture(without, with, enemy_done));

        // 敵より先へ反転できる兵種だけを買う。
        let flip = direct_capture_phase(ActionPhase::new(1, 0), 1, 200, 100);
        assert_eq!(flip, ActionPhase::new(2, 0));
        assert!(buy_for_direct_capture(without, flip, enemy_done));
    }

    /// 到着ターンは格子距離ではなく実地形・実移動力から算出される。
    /// 一様な平地では、格子距離 ÷ 移動力の切り上げと一致することを配線テストで確認する。
    #[test]
    fn capture_arrival_turns_wire_through_exact_pathfinder() {
        let registry = MasterDataRegistry::load().unwrap();
        let map = Map {
            width: 9,
            height: 3,
            tiles: vec![Terrain::Plains; 9 * 3],
            topology: GridTopology::Square,
        };
        let mut cache = ActionTurnDistanceCache::default();

        let turns = capture_arrival_turns(
            &map,
            &registry,
            (0, 1),
            (8, 1),
            MovementType::Infantry,
            3,
            99,
            PlayerId(1),
            &mut cache,
        )
        .expect("平地の歩兵は目標へ到達できる");

        assert_eq!(turns, map.distance(0, 1, 8, 1).div_ceil(3));
    }

    /// 直接占領の競争判定: 同じ到着でも先手が勝つ。
    #[test]
    fn direct_capture_race_tie_goes_to_first_mover() {
        // 両者とも到着 2 フェーズ・占領力 100・都市耐久 200 → 完了 2 回分のオフセットは等しい。
        let durability = 200;
        let my_order = 1; // 後手
        let enemy_order = 0; // 先手

        // 後手は先手と同着 → 負け。
        assert!(!direct_capture_race_winner(
            2,
            100,
            my_order,
            2,
            100,
            enemy_order,
            durability
        ));
        // 先手は同着でも勝ち。
        assert!(direct_capture_race_winner(
            2,
            100,
            enemy_order,
            2,
            100,
            my_order,
            durability
        ));
        // 後手が 1 フェーズ早ければ逆転。
        assert!(direct_capture_race_winner(
            1,
            100,
            my_order,
            2,
            100,
            enemy_order,
            durability
        ));
    }

    /// 完了オフセットは到着＋(占領回数-1)。
    #[test]
    fn completion_offset_accounts_for_capture_actions() {
        // 都市(200)・占領力 100 → 2 回。到着 2 → 完了オフセット 3。
        assert_eq!(completion_offset(2, 200, 100), 3);
        // 首都(400)・占領力 100 → 4 回。到着 0 → 完了オフセット 3。
        assert_eq!(completion_offset(0, 400, 100), 3);
    }

    /// 占領候補は既存より早い場合だけ適合し、早いほど高得点。
    #[test]
    fn capture_speed_fitness_requires_improvement_over_committed() {
        // 未割り当て → どの候補も適合する。
        assert!(capture_speed_fitness(3, None).is_some());

        // 既存が到着 4 → 到着 3 は改善（1 手早い）。
        assert_eq!(capture_speed_fitness(3, Some(4)), Some(1.0));
        // 既存と同じ・遅い → 適合しない（買い増さない）。
        assert_eq!(capture_speed_fitness(4, Some(4)), None);
        assert_eq!(capture_speed_fitness(5, Some(4)), None);
    }

    /// 占領力は満タン 100、ダメージで 10 刻みに下がる。
    #[test]
    fn capture_power_derives_from_display_hp() {
        assert_eq!(capture_power_from_hp(100), 100);
        assert_eq!(capture_power_from_hp(54), 60);
        assert_eq!(capture_power_from_hp(0), 0);
    }

    /// 完了フェーズの先後判定: 同着は先手が勝つ。
    #[test]
    fn completion_race_tie_goes_to_first_mover() {
        // 後手 (order 1) vs 先手 (order 0) の同着 → 後手負け。
        assert!(!completion_race_won(3, 1, 3, 0));
        // 先手 vs 後手の同着 → 先手勝ち。
        assert!(completion_race_won(3, 0, 3, 1));
        // 早い側が勝つ。
        assert!(completion_race_won(2, 1, 3, 0));
    }
}
