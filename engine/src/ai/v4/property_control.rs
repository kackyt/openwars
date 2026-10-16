//! 物件単位の「行動フェーズETA」と `DirectCapture` / `Interdict` / `Recapture` の
//! 実行可能性を判定する純粋関数群。
//!
//! マップ名・兵種名・固定スコア・マップ寸法分岐を含まない。ここに現れるのは
//! 「到着フェーズ」「占領力」「耐久値」「撃破可否」といった盤面から観測できる量だけである。
//!
//! 時刻はターン数ではなく `ActionPhase`（round, player_order）で表し、
//! 同じターン数でも先手（player_order が小さい方）が先着するよう辞書順で比較する。

use super::board_scan::scanned_occupants;
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

use crate::ai::v4::*;
/// ユーザー指定の「T6までの物件到達・敵占領妨害」を、固定評価点ではなく
/// 短期厳密シミュレーションの時間境界として表す。
pub(in crate::ai::v4) const EARLY_PROPERTY_CONTROL_HORIZON: u32 = 6;

/// 物件レースに対して選ぶ、観測可能な三つの制御方法。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::ai::v4) enum PropertyControlApproach {
    /// 占領役を先着させて所有権を取る。
    DirectCapture,
    /// 敵の占領完了前に合法な初撃を入れて占領力を落とす。
    Interdict,
    /// 先着も事前妨害も不能なため、撃破後の奪還へ切り替える。
    Recapture,
}

/// 一つの物件レースについて、盤面時刻から選んだ成立可能な制御方法。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::ai::v4) struct PropertyControlObjective {
    pub(in crate::ai::v4) property: GridPosition,
    pub(in crate::ai::v4) enemy_entity: Entity,
    pub(in crate::ai::v4) enemy_arrival_turn: u32,
    pub(in crate::ai::v4) enemy_completion_turn: u32,
    pub(in crate::ai::v4) attack_ready_turn: Option<u32>,
    pub(in crate::ai::v4) approach: PropertyControlApproach,
    /// cluster内の複数物件を、別々の敵占領役が争っているため局所化が必要か。
    pub(in crate::ai::v4) disaggregated: bool,
}

#[cfg(test)]
impl PropertyControlObjective {
    /// 単一物件を事前妨害で争う、テスト用の物件制御目標を作る。
    pub(in crate::ai::v4) fn interdict_for_test(property: GridPosition) -> Self {
        Self {
            property,
            enemy_entity: Entity::from_raw(42),
            enemy_arrival_turn: 1,
            enemy_completion_turn: 3,
            attack_ready_turn: Some(2),
            approach: PropertyControlApproach::Interdict,
            disaggregated: false,
        }
    }
}

/// 同じCapture clusterで同時に進められる物件レーンと、戦闘計画の主対象。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::ai::v4) struct PropertyControlPlan {
    pub(in crate::ai::v4) primary: PropertyControlObjective,
    /// 同じ時間窓に別々の敵Entityが進める、同時成立が必要な物件制御契約。
    pub(in crate::ai::v4) controls: Vec<PropertyControlObjective>,
    pub(in crate::ai::v4) capture_lanes: Vec<GridPosition>,
}

/// 物件の占領耐久をマスターデータから取得する。
pub(in crate::ai::v4) fn property_durability(scan: &BoardScan, property: &GridPosition) -> u32 {
    scan.map
        .get_terrain(property.x, property.y)
        .map(|terrain| scan.master_data.landscape_durability(terrain.as_str()))
        .unwrap_or(0)
        .max(200)
}

/// 現在盤面または今手番の生産から、指定物件を最短で占領完了できる相対turnを返す。
pub(in crate::ai::v4) fn fastest_friendly_capture_completion(
    scan: &BoardScan,
    ctx: &mut ReachCtx,
    property: &GridPosition,
    player_id: PlayerId,
) -> Option<u32> {
    let durability = property_durability(scan, property);
    let mut best = None;
    for unit in scan.my_units.iter().filter(|unit| unit.stats.can_capture) {
        let Some(arrival) = property_control::capture_arrival_turns(
            &scan.map,
            &scan.master_data,
            (unit.pos.x, unit.pos.y),
            (property.x, property.y),
            unit.stats.movement_type,
            unit.stats.max_movement,
            unit.stats.max_fuel,
            player_id,
            &mut ctx.action_turns,
        ) else {
            continue;
        };
        let completion = property_control::completion_offset(
            arrival,
            durability,
            property_control::capture_power_from_hp(unit.hp),
        );
        best = Some(best.map_or(completion, |current: u32| current.min(completion)));
    }

    // 新規生産unitは次の自分フェーズから行動する。距離1ならそのフェーズに到着して
    // 占領を開始できるため、生産待機と移動フェーズを別々には加算しない。
    for (facility, terrain) in &scan.free_facilities {
        for (unit_type, stats) in &scan.available_types {
            if !stats.can_capture
                || stats.cost > scan.funds
                || !scan.can_produce(*terrain, *unit_type)
            {
                continue;
            }
            let Some(arrival) = property_control::capture_arrival_turns(
                &scan.map,
                &scan.master_data,
                (facility.x, facility.y),
                (property.x, property.y),
                stats.movement_type,
                stats.max_movement,
                stats.max_fuel,
                player_id,
                &mut ctx.action_turns,
            ) else {
                continue;
            };
            let completion = property_control::completion_offset(
                property_control::produced_action_offset(0, arrival),
                durability,
                property_control::capture_power_from_hp(100),
            );
            best = Some(best.map_or(completion, |current: u32| current.min(completion)));
        }
    }
    best
}

/// 今手番に生産できる戦闘unitのうち、敵が物件へ来た後に最も早く初撃できる時刻を返す。
pub(in crate::ai::v4) fn fastest_produced_attack_turn(
    scan: &BoardScan,
    ctx: &mut ReachCtx,
    property: &GridPosition,
    enemy: &UnitSnapshot,
    enemy_arrival_turn: u32,
    player_id: PlayerId,
) -> Option<u32> {
    let mut best: Option<(u32, u32, std::cmp::Reverse<u32>, usize, usize)> = None;
    for (facility, terrain) in &scan.free_facilities {
        for (unit_type, stats) in &scan.available_types {
            let damage = best_damage(&scan.damage_chart, stats.unit_type, enemy.stats.unit_type);
            if damage == 0 || stats.cost > scan.funds || !scan.can_produce(*terrain, *unit_type) {
                continue;
            }
            let Some(projection) = produced_attack_turn_along_capture_route(
                scan,
                ctx,
                facility,
                stats,
                enemy,
                property,
                enemy_arrival_turn,
                player_id,
            ) else {
                continue;
            };
            let key = (
                projection.ready_turn,
                stats.cost,
                std::cmp::Reverse(damage),
                facility.y,
                facility.x,
            );
            if best.is_none_or(|current| key < current) {
                best = Some(key);
            }
        }
    }
    best.map(|candidate| candidate.0)
}

/// 生産した攻撃役が、敵占領役の最短物件進入経路上で合法な初撃を行える最早turn。
///
/// 現在位置へ敵を固定せず、また物件へ瞬間移動させない。敵の始点→経路セル→物件が
/// 実行動ターン距離上の最短経路になる全セルを列挙し、攻撃役も地形・燃料・射程・
/// 移動後間接攻撃制約を通した到達時刻で比較する。
#[allow(clippy::too_many_arguments)]
pub(in crate::ai::v4) fn produced_attack_turn_along_capture_route(
    scan: &BoardScan,
    ctx: &mut ReachCtx,
    facility: &GridPosition,
    stats: &UnitStats,
    enemy: &UnitSnapshot,
    property: &GridPosition,
    enemy_arrival_turn: u32,
    player_id: PlayerId,
) -> Option<ProductionAttackProjection> {
    let actual_occupancy = scanned_occupants(scan, player_id);
    let attacker_fuel = if stats.max_fuel == 0 {
        u32::MAX
    } else {
        stats.max_fuel
    };
    let my_order = player_order(player_id);
    let enemy_order = player_order(player_id.opposite());
    let mut best: Option<(u32, u32, u32, usize, usize, ProductionAttackProjection)> = None;

    for y in 0..scan.map.height {
        for x in 0..scan.map.width {
            let cell = GridPosition { x, y };
            let Some(enemy_to_cell) = property_control::capture_arrival_turns(
                &scan.map,
                &scan.master_data,
                (enemy.pos.x, enemy.pos.y),
                (cell.x, cell.y),
                enemy.stats.movement_type,
                enemy.stats.max_movement,
                enemy.stats.max_fuel,
                player_id.opposite(),
                &mut ctx.action_turns,
            ) else {
                continue;
            };
            let Some(cell_to_property) = property_control::capture_arrival_turns(
                &scan.map,
                &scan.master_data,
                (cell.x, cell.y),
                (property.x, property.y),
                enemy.stats.movement_type,
                enemy.stats.max_movement,
                enemy.stats.max_fuel,
                player_id.opposite(),
                &mut ctx.action_turns,
            ) else {
                continue;
            };
            if enemy_to_cell.saturating_add(cell_to_property) > enemy_arrival_turn {
                continue;
            }
            let Some(attacker_distance) = calculate_action_distance_to_range(
                &scan.map,
                &scan.master_data,
                &actual_occupancy,
                (facility.x, facility.y),
                (cell.x, cell.y),
                stats.movement_type,
                stats.max_movement,
                attacker_fuel,
                stats.min_range,
                stats.max_range,
                player_id,
                &mut ctx.action_turns,
            ) else {
                continue;
            };
            let attacker_ready =
                property_control::produced_action_offset(0, attacker_distance.turns);
            // 後手は敵がセルへ入った同roundに攻撃できる。先手は敵の移動後、次roundの
            // 自分手番で攻撃する。現在セル(到達0)は生産直後には迎撃できない。
            let encounter_turn = if my_order > enemy_order {
                enemy_to_cell
            } else {
                enemy_to_cell.saturating_add(1)
            };
            if enemy_to_cell == 0 || attacker_ready > encounter_turn {
                continue;
            }
            let projection = ProductionAttackProjection {
                ready_turn: encounter_turn,
                firing_position: attacker_distance.firing_position,
                requires_movement: attacker_distance.requires_movement,
            };
            let key = (
                encounter_turn,
                attacker_distance.used_fuel,
                attacker_distance.used_mp,
                projection.firing_position.y,
                projection.firing_position.x,
            );
            if best
                .as_ref()
                .is_none_or(|current| key < (current.0, current.1, current.2, current.3, current.4))
            {
                best = Some((key.0, key.1, key.2, key.3, key.4, projection));
            }
        }
    }
    best.map(|(_, _, _, _, _, projection)| projection)
}

/// Capture clusterを一つの中心点へ畳まず、物件ごとの直取り・妨害・奪還を列挙する。
pub(in crate::ai::v4) fn select_property_control_plan(
    scan: &BoardScan,
    ctx: &mut ReachCtx,
    cluster: &[GridPosition],
    latest_enemy_completion: u32,
    player_id: PlayerId,
) -> Option<PropertyControlPlan> {
    let my_order = player_order(player_id);
    let enemy_order = player_order(player_id.opposite());
    let mut objectives = Vec::new();

    // 一体の敵占領役を複数物件へ同時投入したことにしない。全「敵×物件」の実ETAを
    // 早い順に並べ、敵Entityと物件をそれぞれ一度だけ割り当てる。これにより独立した
    // 物件レース数は、実在する敵占領役数と物件数の双方を超えない。
    let mut projections = Vec::new();
    let reference_infantry = scan
        .available_types
        .iter()
        .find(|(unit_type, stats)| *unit_type == UnitType::Infantry && stats.can_capture)
        .map(|(_, stats)| stats.clone());
    let fallback_enemy_units: Vec<UnitSnapshot> = if scan.enemy_units.is_empty()
        && let Some(infantry_stats) = reference_infantry
    {
        scan.enemy_facilities
            .iter()
            .enumerate()
            .map(|(index, facility)| UnitSnapshot {
                entity: Some(Entity::from_raw(900_000 + index as u32)),
                pos: facility.pos,
                stats: infantry_stats.clone(),
                hp: 100,
                free_cargo: 0,
            })
            .collect()
    } else {
        Vec::new()
    };
    let enemy_units: &[UnitSnapshot] = if !scan.enemy_units.is_empty() {
        &scan.enemy_units
    } else {
        &fallback_enemy_units
    };

    for property in cluster {
        let durability = property_durability(scan, property);
        let friendly_completion =
            fastest_friendly_capture_completion(scan, ctx, property, player_id);
        // 作戦時間幅までに自軍の占領完了が物理的に間に合わない物件は、今手番の
        // 物件契約へ入れない。後方物件は次のRoadmap区間で盤面を再観測して扱う。
        if friendly_completion.is_none_or(|completion| completion > latest_enemy_completion) {
            continue;
        }
        let neutral = scan.neutral_properties.contains(property);
        for enemy in enemy_units.iter().filter(|enemy| enemy.stats.can_capture) {
            let Some(entity) = enemy.entity else {
                continue;
            };
            let Some(arrival) = property_control::capture_arrival_turns(
                &scan.map,
                &scan.master_data,
                (enemy.pos.x, enemy.pos.y),
                (property.x, property.y),
                enemy.stats.movement_type,
                enemy.stats.max_movement,
                enemy.stats.max_fuel,
                player_id.opposite(),
                &mut ctx.action_turns,
            ) else {
                continue;
            };
            let completion = property_control::completion_offset(
                arrival,
                durability,
                property_control::capture_power_from_hp(enemy.hp),
            );
            if neutral && completion > latest_enemy_completion {
                continue;
            }
            // 中立は敵の占領完了が早いレースから、敵所有物件は自軍が奪還完了できる
            // 前線から割り当てる。後者で敵後方駐留地を先に選ばない。
            let property_deadline = if neutral {
                completion
            } else {
                friendly_completion.expect("到達可能物件だけを残している")
            };
            projections.push((
                property_deadline,
                arrival,
                entity.to_bits(),
                property.y,
                property.x,
                entity,
                *property,
                neutral,
                completion,
                friendly_completion,
            ));
        }
    }
    projections.sort_unstable_by_key(|projection| {
        (
            projection.0,
            projection.1,
            projection.2,
            projection.3,
            projection.4,
        )
    });
    let mut assigned_enemies = HashSet::default();
    let mut assigned_properties = HashSet::default();
    for (
        _,
        enemy_arrival,
        _,
        _,
        _,
        enemy_entity,
        property,
        neutral,
        enemy_completion,
        friendly_completion,
    ) in projections
    {
        if assigned_enemies.contains(&enemy_entity) || assigned_properties.contains(&property) {
            continue;
        }
        assigned_enemies.insert(enemy_entity);
        assigned_properties.insert(property);
        let Some(enemy) = enemy_units
            .iter()
            .find(|enemy| enemy.entity == Some(enemy_entity))
        else {
            continue;
        };

        let attack_ready =
            fastest_produced_attack_turn(scan, ctx, &property, enemy, enemy_arrival, player_id);
        let approach = if !neutral {
            PropertyControlApproach::Recapture
        } else if friendly_completion.is_some_and(|mine| {
            property_control::completion_race_won(mine, my_order, enemy_completion, enemy_order)
        }) {
            PropertyControlApproach::DirectCapture
        } else if attack_ready.is_some_and(|attack| {
            property_control::interdict_feasible(
                property_control::ActionPhase::new(attack, my_order),
                property_control::ActionPhase::new(enemy_completion, enemy_order),
            )
        }) {
            PropertyControlApproach::Interdict
        } else {
            PropertyControlApproach::Recapture
        };
        objectives.push((
            PropertyControlObjective {
                property,
                enemy_entity,
                enemy_arrival_turn: enemy_arrival,
                enemy_completion_turn: enemy_completion,
                attack_ready_turn: attack_ready,
                approach,
                disaggregated: false,
            },
            friendly_completion,
        ));
    }

    let distinct_enemy_count = objectives
        .iter()
        .map(|(objective, _)| objective.enemy_entity)
        .collect::<HashSet<_>>()
        .len();
    objectives.sort_by_key(|(objective, friendly_completion)| {
        let approach_rank = match objective.approach {
            PropertyControlApproach::DirectCapture => 0,
            PropertyControlApproach::Interdict => 1,
            PropertyControlApproach::Recapture => 2,
        };
        let action_turn = match objective.approach {
            PropertyControlApproach::DirectCapture => friendly_completion.unwrap_or(u32::MAX),
            PropertyControlApproach::Interdict | PropertyControlApproach::Recapture => {
                objective.attack_ready_turn.unwrap_or(u32::MAX)
            }
        };
        (
            approach_rank,
            action_turn,
            objective.enemy_completion_turn,
            objective.property.y,
            objective.property.x,
        )
    });
    for (objective, _) in &mut objectives {
        objective.disaggregated = distinct_enemy_count > 1;
    }
    let primary = objectives.first().map(|(objective, _)| *objective)?;
    let capture_lanes = objectives
        .iter()
        // 直取り・妨害に失敗するレーンも、撃破後の奪還には占領役が必要である。
        // 到達可能なレーンだけを残し、Recaptureを理由に占領生産そのものを消さない。
        .filter(|(_, friendly_completion)| friendly_completion.is_some())
        .map(|(objective, _)| objective.property)
        .collect::<Vec<_>>();
    let capture_lanes = (!capture_lanes.is_empty()).then_some(capture_lanes)?;
    Some(PropertyControlPlan {
        primary,
        controls: objectives.iter().map(|(objective, _)| *objective).collect(),
        capture_lanes,
    })
}

/// 期限付き物件レーンの占領役を、実経路の占領完了時刻で辞書順選択する。
///
/// 価格で割った適合度は、到達1手の差を安価な歩兵で上書きしてしまう。ここではまず
/// 完了時刻、次に観測敵から受ける最大実ダメージ、最後に費用を比較する。同じ期限を
/// 満たす候補間でだけ費用が効くため、兵種名や固定評価点による軽歩兵優遇はない。
pub(in crate::ai::v4) fn select_property_capture_candidate(
    scan: &BoardScan,
    ctx: &mut ReachCtx,
    op: &Operation,
    used_facilities: &HashSet<GridPosition>,
    remaining_funds: u32,
    player_id: PlayerId,
) -> Option<SlotCandidate> {
    // 占領完了時刻・敵反撃・費用・施設位置を混同しない比較キー。
    type CaptureCandidateKey = (u32, u32, u32, u32, usize, usize);
    let target = next_capture_target(op);
    let durability = property_durability(scan, &target);
    let mut best: Option<(CaptureCandidateKey, SlotCandidate)> = None;

    for (facility, terrain) in &scan.free_facilities {
        if used_facilities.contains(facility) {
            continue;
        }
        for (unit_type, stats) in &scan.available_types {
            if !stats.can_capture
                || stats.cost == 0
                || stats.cost > remaining_funds
                || !scan.can_produce(*terrain, *unit_type)
                || !can_join_operation(
                    scan,
                    ctx,
                    &target,
                    op.facts.requires_transport,
                    facility,
                    stats,
                )
            {
                continue;
            }
            let Some(arrival) = property_control::capture_arrival_turns(
                &scan.map,
                &scan.master_data,
                (facility.x, facility.y),
                (target.x, target.y),
                stats.movement_type,
                stats.max_movement,
                stats.max_fuel,
                player_id,
                &mut ctx.action_turns,
            )
            .map(|turns| property_control::produced_action_offset(0, turns)) else {
                continue;
            };
            // `capture_lane_targets` は既存担当数を除いた未充足レーンである。別レーンの
            // 既存歩兵がこの物件へ近いというだけで候補を落とすと、1体が複数物件を
            // 同時占領できる誤った前提になり、担当撃破後の有限な再契約まで消える。
            let completion = property_control::completion_offset(
                arrival,
                durability,
                property_control::capture_power_from_hp(100),
            );
            let maximum_incoming = op
                .reachable_threats
                .iter()
                .map(|threat| {
                    best_damage(&scan.damage_chart, threat.stats.unit_type, stats.unit_type)
                })
                .max()
                .unwrap_or(0);
            let candidate = SlotCandidate {
                unit_type: *unit_type,
                cost: stats.cost,
                facility: *facility,
                // トレース互換用。実際の比較は上の整数tupleだけで行う。
                fitness: 1.0,
            };
            let key = (
                completion,
                arrival,
                stats.cost,
                maximum_incoming,
                facility.y,
                facility.x,
            );
            if best.as_ref().is_none_or(|(current, _)| key < *current) {
                best = Some((key, candidate));
            }
        }
    }
    best.map(|(_, candidate)| candidate)
}

/// 敵が今この手番に未所有（中立・敵）拠点の上にいて占領を進めているか。
///
/// 占領中かどうかはユニット名や固定表ではなく、「占領能力を持ち、かつ未所有拠点の
/// 座標に立っている」という盤面の事実だけで判定する。この敵は占領完了を目前にした
/// `Interdict` の直接対象であり、単に「占領可能な敵」より優先して撃破・妨害を向ける。
pub(in crate::ai::v4) fn enemy_is_capturing(scan: &BoardScan, threat: &ThreatTarget) -> bool {
    threat.stats.can_capture
        && scan
            .open_properties
            .iter()
            .any(|property| property.x == threat.position.x && property.y == threat.position.y)
}

/// 既に盤面にいて、この作戦の物件へ到達できる自軍占領役の最短到達フェーズ数。
///
/// 生産候補（`slot_fitness` の Capture 枝）は、この最短到達より速い場合だけ直取り目的で
/// 価値を持つ。既存ユニットはこの手番に動けるので +1 しない。到達できる占領役が
/// いなければ `None` を返す（＝候補は初動として必ず価値を持つ）。
pub(in crate::ai::v4) fn next_capture_target(op: &Operation) -> GridPosition {
    op.capture_lane_targets
        .get(op.filled.capture_units as usize)
        .copied()
        .unwrap_or(op.anchor)
}

pub(in crate::ai::v4) fn committed_capture_arrival(
    scan: &BoardScan,
    ctx: &mut ReachCtx,
    target: GridPosition,
    player_id: PlayerId,
) -> Option<u32> {
    scan.my_units
        .iter()
        .filter(|unit| unit.stats.can_capture)
        .filter_map(|unit| {
            property_control::capture_arrival_turns(
                &scan.map,
                &scan.master_data,
                (unit.pos.x, unit.pos.y),
                (target.x, target.y),
                unit.stats.movement_type,
                unit.stats.max_movement,
                unit.stats.max_fuel,
                player_id,
                &mut ctx.action_turns,
            )
        })
        .min()
}

/// 敵の占領可能ユニットが `anchor` を占領完了するまでの相対フェーズ数（オフセット）を返す。
///
/// 到着フェーズは `capture_arrival_turns` で実測し、`completion_offset` で占領完了まで
/// 加算する。対象物件の耐久（durability）は、`anchor` にある未所有拠点の
/// `max_capture_points` を採用する。到達できる敵占領役がいなければ `None`。
pub(in crate::ai::v4) fn enemy_capture_completion_offset(
    scan: &BoardScan,
    ctx: &mut ReachCtx,
    anchor: &GridPosition,
    player_id: PlayerId,
) -> Option<u32> {
    // 物件の耐久（占領ポイント最大値）は地形から引く。都市・工場=200、空港・港=300、
    // 首都=400。地形が不明な場合は標準の都市耐久 200 へ丸める。
    let durability = scan
        .map
        .get_terrain(anchor.x, anchor.y)
        .map(|terrain| scan.master_data.landscape_durability(terrain.as_str()))
        .unwrap_or(0)
        .max(200);

    let opposing = player_id.opposite();
    scan.enemy_units
        .iter()
        .filter(|unit| unit.stats.can_capture)
        .filter_map(|unit| {
            property_control::capture_arrival_turns(
                &scan.map,
                &scan.master_data,
                (unit.pos.x, unit.pos.y),
                (anchor.x, anchor.y),
                unit.stats.movement_type,
                unit.stats.max_movement,
                unit.stats.max_fuel,
                opposing,
                &mut ctx.action_turns,
            )
            .map(|arrival| {
                let power = property_control::capture_power_from_hp(unit.hp);
                property_control::completion_offset(arrival, durability, power)
            })
        })
        .min()
}

/// 手番順（0 が先手、1 が後手）。PlayerId(1) が先手、PlayerId(2) が後手。
pub(in crate::ai::v4) fn player_order(player_id: PlayerId) -> u32 {
    player_id.0.saturating_sub(1)
}

/// 直接占領競争で後着かを判定する。
///
/// `deploy_lead_time` は基準占領ユニットの展開リードタイム。占領完了はそこに
/// `CAPTURE_COMPLETION_TURNS` を加えたもの。敵の占領完了（`enemy_completion`）と同じか
/// それより後なら後着（`true`）で、占領枠を広げず妨害・奪還へ予算を残すべき。
/// 敵の完了が不明（`None`）なら後着と扱わない。
pub(in crate::ai::v4) fn capture_race_behind(
    deploy_lead_time: u32,
    enemy_completion: Option<u32>,
    player_id: PlayerId,
) -> bool {
    let Some(enemy) = enemy_completion else {
        return false;
    };
    let mine = deploy_lead_time.saturating_add(CAPTURE_COMPLETION_TURNS);
    !property_control::completion_race_won(
        mine,
        player_order(player_id),
        enemy,
        player_order(player_id.opposite()),
    )
}

/// この生産判断内で確定した占領役。戦闘計画へ実兵種・実施設・実任務先を渡す。
#[derive(Debug, Clone)]
pub(in crate::ai::v4) struct PlannedCaptureUnit {
    pub(in crate::ai::v4) stats: UnitStats,
    pub(in crate::ai::v4) facility: GridPosition,
    pub(in crate::ai::v4) target: GridPosition,
}
