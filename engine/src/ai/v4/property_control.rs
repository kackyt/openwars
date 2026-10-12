//! 物件単位の「行動フェーズETA」と `DirectCapture` / `Interdict` / `Recapture` の
//! 実行可能性を判定する純粋関数群。
//!
//! マップ名・兵種名・固定スコア・マップ寸法分岐を含まない。ここに現れるのは
//! 「到着フェーズ」「占領力」「耐久値」「撃破可否」といった盤面から観測できる量だけである。
//!
//! 時刻はターン数ではなく `ActionPhase`（round, player_order）で表し、
//! 同じターン数でも先手（player_order が小さい方）が先着するよう辞書順で比較する。

use crate::ai::turn_distance::{ActionTurnDistanceCache, calculate_action_distance_to_range};
use crate::collections::HashMap;
use crate::components::PlayerId;
use crate::resources::master_data::MasterDataRegistry;
use crate::resources::{Map, MovementType};
/// 行動フェーズ。`(round, player_order)` の辞書順で比較する。
///
/// `round` はターン番号、`player_order` はそのラウンド内の手番順（0 が先手、1 が後手）。
/// 「同じ 2 ターン到着」でも先手と後手を同着にしないための値オブジェクト。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct ActionPhase {
    /// ラウンド（ターン番号）
    pub round: u32,
    /// ラウンド内の手番順。0 が先手、1 が後手。
    pub player_order: u32,
}

impl ActionPhase {
    /// 指定ラウンド・手番順のフェーズを作る。
    pub(crate) fn new(round: u32, player_order: u32) -> Self {
        Self {
            round,
            player_order,
        }
    }
}

/// 拠点を占領しきるまでに必要な占領行動回数。
///
/// 占領力（`capture_power`）は 1 行動あたりのポイント減少量で、
/// エンジンは `(HP の表示値) * 10` を採用する（`systems/property.rs`）。
pub(crate) fn capture_actions_needed(durability: u32, capture_power: u32) -> u32 {
    durability.div_ceil(capture_power.max(1))
}

/// 攻撃役が敵の占領完了前に攻撃を入れられるか（`Interdict`）。
///
/// `attack_ready_phase` は攻撃役が射程内へ到達するフェーズ。敵の占領完了と同時か
/// それより後では妨害にならないため、strict に「より前」を要求する。
pub(crate) fn interdict_feasible(
    attack_ready_phase: ActionPhase,
    enemy_completion: ActionPhase,
) -> bool {
    attack_ready_phase < enemy_completion
}

/// 直接占領を完了するまでの「自分フェーズ数」を相対値で返す。
///
/// 到着フェーズに最初の占領を行い、残り `(占領回数 - 1)` 回を後続フェーズで行うため、
/// `arrival_turns + capture_actions_needed(durability, power) - 1` になる。
pub(crate) fn completion_offset(arrival_turns: u32, durability: u32, capture_power: u32) -> u32 {
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
pub(crate) fn produced_action_offset(build_turn: u32, action_distance: u32) -> u32 {
    build_turn.saturating_add(action_distance.max(1))
}

/// 占領候補の適合度を「既に割り当て済みの占領役より何ターン早く到着するか」で表す。
///
/// `candidate_arrival` は候補（生産後は行動不能なので +1 済み）が物件へ到達するまでの
/// 自分フェーズ数。`committed_best` は既存占領役の最短到達フェーズ数（未割り当てなら `None`）。
///
/// 既存より遅い・同等の候補は `None`（支配フェーズを改善しない＝機械的に買い増さない）。
pub(crate) fn capture_speed_fitness(
    candidate_arrival: u32,
    committed_best: Option<u32>,
) -> Option<f32> {
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
pub(crate) fn capture_power_from_hp(hp: u32) -> u32 {
    ((hp.saturating_add(9)) / 10) * 10
}

/// 完了フェーズ（相対オフセット）の先後判定。同じオフセットでも先手（`order` が小さい方）が
/// 先に完了する。`order` は 0 が先手、1 が後手。
pub(crate) fn completion_race_won(
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
pub(crate) fn capture_arrival_turns(
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
    let empty_occupancy = HashMap::default();
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
        let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
        assert_eq!(produced_action_offset(0, 0), 1);
        assert_eq!(produced_action_offset(0, 1), 1);
        assert_eq!(produced_action_offset(0, 2), 2);
        assert_eq!(produced_action_offset(3, 1), 4);
    }

    /// 到着ターンは格子距離ではなく実地形・実移動力から算出される。
    /// 一様な平地では、格子距離 ÷ 移動力の切り上げと一致することを配線テストで確認する。
    #[test]
    fn capture_arrival_turns_wire_through_exact_pathfinder() {
        let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
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

    /// 完了オフセットは到着＋(占領回数-1)。
    #[test]
    fn completion_offset_accounts_for_capture_actions() {
        let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
        // 都市(200)・占領力 100 → 2 回。到着 2 → 完了オフセット 3。
        assert_eq!(completion_offset(2, 200, 100), 3);
        // 首都(400)・占領力 100 → 4 回。到着 0 → 完了オフセット 3。
        assert_eq!(completion_offset(0, 400, 100), 3);
    }

    /// 占領候補は既存より早い場合だけ適合し、早いほど高得点。
    #[test]
    fn capture_speed_fitness_requires_improvement_over_committed() {
        let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
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
        let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
        assert_eq!(capture_power_from_hp(100), 100);
        assert_eq!(capture_power_from_hp(54), 60);
        assert_eq!(capture_power_from_hp(0), 0);
    }

    /// 完了フェーズの先後判定: 同着は先手が勝つ。
    #[test]
    fn completion_race_tie_goes_to_first_mover() {
        let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
        // 後手 (order 1) vs 先手 (order 0) の同着 → 後手負け。
        assert!(!completion_race_won(3, 1, 3, 0));
        // 先手 vs 後手の同着 → 先手勝ち。
        assert!(completion_race_won(3, 0, 3, 1));
        // 早い側が勝つ。
        assert!(completion_race_won(2, 1, 3, 0));
    }
}
