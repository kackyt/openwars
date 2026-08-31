//! 島作戦の残敵排除を、金額ではなく実行可能な行動列として比較する純粋計画器。
//!
//! このモジュールは盤面を直接読まず、呼び出し側が作ったsnapshotだけを受け取る。
//! 既存戦力と複数ターンの生産候補を混ぜ、悲観側ダメージで残敵を排除できる
//! パッケージだけを費用・完了ターン・損耗で比較する。

use crate::components::{GridPosition, UnitStats};
use crate::resources::master_data::MasterDataRegistry;
use crate::resources::{DamageChart, Map, Terrain, UnitType};
use crate::systems::combat::calculate_damage_formula;
use bevy_ecs::prelude::Entity;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

// 1施設手番ごとの候補を順に展開するため、同手番に複数施設を使う混成案が
// beamから落ちない幅を確保する。候補全体の直積を走査する旧方式には戻さない。
pub(crate) const SEARCH_BEAM_WIDTH: usize = 64;
pub(crate) const DEFAULT_SEARCH_TURNS: u32 = 12;

#[derive(Debug, Clone)]
pub(crate) struct FriendlyPlanUnit {
    pub stats: UnitStats,
    pub position: GridPosition,
    pub hp: u32,
    /// 0は既存unit、1以上は生産完了後に行動可能になる相対ターン。
    pub available_turn: u32,
    /// 地形連結と武装の両方を満たし、実際に交戦できる敵index。
    pub engageable_enemy_indices: Vec<usize>,
}

#[derive(Debug, Clone)]
pub(crate) struct EnemyPlanUnit {
    pub entity: Option<Entity>,
    pub stats: UnitStats,
    pub position: GridPosition,
    pub hp: u32,
    pub defense_bonus: u32,
    /// 0は現在盤面の敵。1以上は敵施設から前線へ到着する悲観scenarioの増援。
    pub available_turn: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct PlannedPurchase {
    pub facility: GridPosition,
    pub unit_type: UnitType,
    /// 0なら今手番、1以上なら将来手番の生産予定。
    pub build_turn: u32,
    pub cost: u32,
}

/// 1施設を1手番に1回だけ使えるという生産ルールを表す値オブジェクト。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct ProductionSlot {
    facility: GridPosition,
    build_turn: u32,
}

impl From<PlannedPurchase> for ProductionSlot {
    fn from(purchase: PlannedPurchase) -> Self {
        Self {
            facility: purchase.facility,
            build_turn: purchase.build_turn,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ProductionPlanOption {
    pub purchase: PlannedPurchase,
    pub stats: UnitStats,
    /// 生産地点から実際に交戦できる敵index。
    pub engageable_enemy_indices: Vec<usize>,
    /// Someなら、この候補は戦闘役ではなく指定物件の保護対象占領役として生産する。
    pub capture_target: Option<GridPosition>,
    /// 占領役候補が指定物件へ到着する実経路turn。
    pub capture_arrival_turn: Option<u32>,
    /// 占領役候補が指定物件を占領完了する実経路turn。
    pub capture_completion_turn: Option<u32>,
}

#[derive(Debug, Clone)]
pub(crate) struct RollingPlanInput {
    pub map: Arc<Map>,
    pub master_data: Arc<MasterDataRegistry>,
    pub damage_chart: Arc<DamageChart>,
    pub existing_units: Vec<FriendlyPlanUnit>,
    /// 戦闘部隊とは別に、占領完了まで生存させる必要がある実在の占領兵。
    pub protected_units: Vec<FriendlyPlanUnit>,
    pub enemies: Vec<EnemyPlanUnit>,
    pub production_options: Vec<ProductionPlanOption>,
    /// 生産候補ごと・敵ごとの、実地形と射程を通した最早攻撃turn。
    /// 空なら従来の格子距離見積りへフォールバックする（単体テスト用）。
    pub production_attack_ready_turns: Vec<Vec<Option<u32>>>,
    /// 期限付きInterdictで、初撃と残HPを判定する敵index。
    /// Noneの通常戦では全敵を従来どおり評価する。
    pub deadline_target_index: Option<usize>,
    /// 物件ごとに別Entityへ初撃を入れる必要がある `(敵index, 最終turn)` 契約。
    pub interdiction_deadlines: Vec<(usize, u32)>,
    pub current_funds: u32,
    pub income_per_turn: u32,
    /// 観測可能な盤面イベントから導出した硬い期限。なければNone。
    pub hard_deadline: Option<u32>,
    /// 実在するcampaign cargoと輸送phaseから予測した占領完了turn。未編成ならNone。
    pub capture_completion_turn: Option<u32>,
    /// 占領予定時点に必要な生存占領兵数。未所有の作戦対象施設数から導く。
    pub required_capture_survivors: usize,
    /// 占領が1ターン遅れる機会損失。実行可能案同士の比較にだけ用いる。
    pub delay_cost_per_turn: u32,
    /// 作戦規模ごとのbeam幅。局地作戦は毎手番再評価するため、首都本隊より小さくできる。
    pub search_beam_width: usize,
    /// 物件レースでは「期限内に全滅」が常態として成立しない。その fallback では
    /// 総ダメージより「早い最初の一撃」と「安い編成」を優先して快速の妨害を選ぶ。
    pub prioritize_deadline_interdiction: bool,
    /// 物件制御では今手番の全合法な占領・戦闘役組合せを枝刈りなしで比較する。
    pub exact_property_control: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PlannedCapturePurchase {
    pub purchase: PlannedPurchase,
    pub target: GridPosition,
    pub completion_turn: Option<u32>,
}

/// 戦闘シミュレーションで最初に成立した、購入unitと実在敵Entityの組合せ。
///
/// 生産後に標的を選び直すと、施設からの初撃ETAを比較した計画と別の正面へ移動してしまう。
/// 価格や兵種名ではなく、実シミュレーションで成立した最初の交戦を配備へ引き渡す。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PlannedCombatPurchase {
    pub purchase: PlannedPurchase,
    pub target: Entity,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TargetForecast {
    pub entity: Option<Entity>,
    pub unit_type: UnitType,
    pub available_turn: u32,
    pub initial_hp: u32,
    pub remaining_hp: u32,
    pub destroyed_turn: Option<u32>,
}

/// 作戦の各手番で、前線へ入る敵HPと実際に除去できるHPを比較する予測。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CampaignTurnForecast {
    pub turn: u32,
    pub enemy_arrival_hp: u32,
    pub enemy_hp_removed: u32,
    pub friendly_hp_lost: u32,
    pub attack_count: u32,
}

#[derive(Debug, Clone)]
pub(crate) struct ForcePackagePlan {
    pub purchases: Vec<PlannedPurchase>,
    pub capture_purchases: Vec<PlannedCapturePurchase>,
    pub combat_purchases: Vec<PlannedCombatPurchase>,
    pub target_forecasts: Vec<TargetForecast>,
    pub turn_forecasts: Vec<CampaignTurnForecast>,
    pub feasible: bool,
    pub first_attack_turn: Option<u32>,
    /// 観測済みの敵一体へ、その手番に行動可能な戦闘役を集中すれば撃破できる最早turn。
    /// 物件戦では総撃破数ではなく、後続占領役が通る最初の突破口を開ける時刻として使う。
    pub front_breakthrough_turn: Option<u32>,
    /// 物件制御の対象Entityへ最初に攻撃できるturn。
    pub deadline_target_first_attack_turn: Option<u32>,
    /// 敵indexごとの最初の合法攻撃turn。複数物件の同時妨害判定に用いる。
    pub target_first_attack_turns: Vec<Option<u32>>,
    pub elimination_turn: Option<u32>,
    pub occupation_turn: Option<u32>,
    pub production_cost: u32,
    pub expected_loss: u32,
    /// 敵排除時点に残る、戦闘可能な友軍のHP比例コスト。Expected増援の後に
    /// 前線を保持できるかを測るため、購入額だけでなく実損耗後の価値を使う。
    pub surviving_combat_value: u32,
    /// 仮想Expected増援の総価値から導いた、排除後に残したい最低戦闘価値。
    pub required_overmatch_value: u32,
    /// Expectedを排除した後も次波へ備える最低価値を満たすか。
    pub overmatch_ready: bool,
    /// 占領工程へ接続済みで、敵行動シミュレーションの保護対象にした兵数。
    pub protected_unit_count: usize,
    /// 占領予定時点まで生存すると予測した保護対象兵数。
    pub protected_survivor_count: usize,
    /// hard deadlineまでに目標へ到着・占領完了でき、かつ生存すると予測した新規占領役数。
    pub deadline_capture_survivor_count: usize,
    /// 未所有の作戦対象施設を占領するため、予定時点に必要な生存兵数。
    pub required_capture_survivor_count: usize,
    pub candidates_considered: usize,
    /// 同一生産slot内で相性・価格ともに支配された候補を探索前に除外した数。
    pub candidates_pruned: usize,
    pub search_truncated: bool,
}

/// 永続化した生産列を現在盤面へ載せ直せない理由。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FixedPackageError {
    /// 施設喪失・兵種制約・到達性変化により、予定していた生産slotが消えた。
    ProductionSlotUnavailable,
    /// 同じ施設・同じ手番を複数の生産へ割り当てている。
    DuplicateProductionSlot,
    /// 予定手番までの所持金と収入では購入列を実行できない。
    FundingUnavailable,
}

impl ForcePackagePlan {
    pub(crate) fn current_purchases(&self) -> impl Iterator<Item = PlannedPurchase> + '_ {
        self.purchases
            .iter()
            .copied()
            .filter(|purchase| purchase.build_turn == 0)
    }

    pub(crate) fn capture_target_for(&self, purchase: PlannedPurchase) -> Option<GridPosition> {
        self.capture_purchases
            .iter()
            .find(|assignment| assignment.purchase == purchase)
            .map(|assignment| assignment.target)
    }

    pub(crate) fn combat_target_for(&self, purchase: PlannedPurchase) -> Option<Entity> {
        self.combat_purchases
            .iter()
            .find(|assignment| assignment.purchase == purchase)
            .map(|assignment| assignment.target)
    }

    fn remaining_hp(&self) -> u32 {
        self.target_forecasts
            .iter()
            .map(|target| target.remaining_hp)
            .sum()
    }

    fn interdiction_first_attack_turn(&self) -> Option<u32> {
        self.deadline_target_first_attack_turn
            .or(self.first_attack_turn)
    }

    fn interdiction_remaining_hp(&self, target_index: Option<usize>) -> u32 {
        target_index
            .and_then(|index| self.target_forecasts.get(index))
            .map_or_else(|| self.remaining_hp(), |target| target.remaining_hp)
    }

    fn missed_interdiction_contracts(&self, deadlines: &[(usize, u32)]) -> usize {
        deadlines
            .iter()
            .filter(|(index, deadline)| {
                !self
                    .target_first_attack_turns
                    .get(*index)
                    .and_then(|turn| *turn)
                    .is_some_and(|turn| turn <= *deadline)
            })
            .count()
    }

    fn interdiction_contracts_satisfied(&self, deadlines: &[(usize, u32)]) -> bool {
        !deadlines.is_empty() && self.missed_interdiction_contracts(deadlines) == 0
    }

    fn capture_survivor_shortfall(&self, required: usize) -> usize {
        required.saturating_sub(self.protected_survivor_count)
    }

    fn property_contracts_satisfied(
        &self,
        deadlines: &[(usize, u32)],
        required_capture_survivors: usize,
    ) -> bool {
        self.completed_property_contracts(deadlines, required_capture_survivors)
            >= required_capture_survivors
    }

    fn completed_property_contracts(
        &self,
        deadlines: &[(usize, u32)],
        required_capture_survivors: usize,
    ) -> usize {
        let combat_completed = if deadlines.is_empty() {
            self.target_forecasts
                .iter()
                .filter(|target| target.destroyed_turn.is_some())
                .count()
        } else {
            deadlines
                .len()
                .saturating_sub(self.missed_interdiction_contracts(deadlines))
        };
        self.protected_survivor_count
            .min(required_capture_survivors)
            .min(combat_completed)
    }

    fn front_breakthrough_rank(&self) -> (bool, u32) {
        (
            self.front_breakthrough_turn.is_none(),
            self.front_breakthrough_turn.unwrap_or(u32::MAX),
        )
    }

    fn capture_completion_profile(&self) -> Vec<u32> {
        let mut turns = self
            .capture_purchases
            .iter()
            .map(|assignment| assignment.completion_turn.unwrap_or(u32::MAX))
            .collect::<Vec<_>>();
        turns.sort_unstable();
        turns
    }

    fn completion_for_ordering(&self) -> u32 {
        self.occupation_turn
            .or(self.elimination_turn)
            .unwrap_or(u32::MAX)
    }

    fn utility_cost(&self, delay_cost_per_turn: u32) -> u64 {
        u64::from(self.production_cost)
            + u64::from(self.expected_loss)
            + u64::from(self.completion_for_ordering())
                .saturating_mul(u64::from(delay_cost_per_turn))
    }
}

#[derive(Debug, Clone)]
struct SimFriendly {
    stats: UnitStats,
    hp: u32,
    initial_hp: u32,
    available_turn: u32,
    attacks_left: u32,
    /// 友軍から各敵へ攻撃するときの不変条件。敵indexと同じ添字で参照する。
    attack_profiles: Arc<[Option<FriendlyAttackProfile>]>,
    /// 各敵がこの友軍を攻撃するときの不変条件。敵indexと同じ添字で参照する。
    enemy_attack_profiles: Arc<[Option<EnemyAttackProfile>]>,
    /// 生産候補だけが持つ購入identity。既存戦力はNone。
    purchase: Option<PlannedPurchase>,
}

#[derive(Debug, Clone, Copy)]
struct FriendlyAttackProfile {
    base_damage: u32,
    incoming_damage: u32,
    ready_turn: u32,
}

#[derive(Debug, Clone, Copy)]
struct EnemyAttackProfile {
    base_damage: u32,
    contact_turn: u32,
}

#[derive(Debug, Clone)]
struct SimEnemy {
    source: EnemyPlanUnit,
    hp: u32,
    destroyed_turn: Option<u32>,
    attacks_left: u32,
}

#[derive(Debug, Clone, Default)]
struct SearchState {
    option_indices: Vec<usize>,
    used_slots: HashSet<ProductionSlot>,
    used_capture_targets: HashSet<GridPosition>,
    cost: u32,
}

/// 1回のRolling Plan入力で不変な交戦関係を全beam候補から共有する。
struct SimulationCatalog {
    existing_units: Vec<SimFriendly>,
    protected_units: Vec<SimFriendly>,
    production_units: Vec<SimFriendly>,
    enemies: Vec<SimEnemy>,
}

impl SimulationCatalog {
    fn new(input: &RollingPlanInput) -> Self {
        let existing_units = input
            .existing_units
            .iter()
            .map(|source| sim_friendly(input, source, true))
            .collect();
        let protected_units = input
            .protected_units
            .iter()
            .map(|source| sim_friendly(input, source, false))
            .collect();
        let production_units = input
            .production_options
            .iter()
            .enumerate()
            .map(|(option_index, option)| {
                let mut simulated = sim_friendly(
                    input,
                    &FriendlyPlanUnit {
                        stats: option.stats.clone(),
                        // 専任占領役は敵が残る物件へ先置きせず、生産地点側でscreenの
                        // 掃討を待ってから追随する。目標到着・完了時刻は別フィールドの
                        // 実経路ETAで保持し、被攻撃位置だけを敵前線へ瞬間移動させない。
                        position: option.purchase.facility,
                        hp: 100,
                        available_turn: option
                            .capture_arrival_turn
                            .unwrap_or_else(|| option.purchase.build_turn.saturating_add(1)),
                        engageable_enemy_indices: option.engageable_enemy_indices.clone(),
                    },
                    // 専任占領役はscreenを追い越して敵へ接近しない。敵側が生産地点まで
                    // 到達する時刻だけを被攻撃開始に使い、戦闘役だけが交戦距離を詰める。
                    option.capture_target.is_none(),
                );
                simulated.purchase = Some(option.purchase);
                // 生産判断側で計算した実経路ETAがあれば、格子距離による近似を
                // 上書きする。山・川・橋・最小射程を無視した快速判定を防ぐ。
                if let Some(exact_ready_turns) =
                    input.production_attack_ready_turns.get(option_index)
                {
                    let mut profiles = simulated.attack_profiles.to_vec();
                    for (enemy_index, profile) in profiles.iter_mut().enumerate() {
                        match (
                            profile.as_mut(),
                            exact_ready_turns.get(enemy_index).copied().flatten(),
                        ) {
                            (Some(profile), Some(ready_turn)) => {
                                profile.ready_turn = ready_turn;
                            }
                            (Some(_), None) => *profile = None,
                            (None, _) => {}
                        }
                    }
                    simulated.attack_profiles = profiles.into();
                }
                simulated
            })
            .collect();
        let enemies = input
            .enemies
            .iter()
            .cloned()
            .map(|source| SimEnemy {
                hp: source.hp,
                attacks_left: source
                    .stats
                    .max_ammo1
                    .saturating_add(source.stats.max_ammo2)
                    .max(1),
                source,
                destroyed_turn: None,
            })
            .collect();
        Self {
            existing_units,
            protected_units,
            production_units,
            enemies,
        }
    }
}

/// 現在観測した敵を排除できる混成パッケージを探索する。
pub(crate) fn plan_force_package(input: &RollingPlanInput) -> Option<ForcePackagePlan> {
    let catalog = SimulationCatalog::new(input);
    if input.enemies.is_empty() {
        let search_turns = input
            .hard_deadline
            .or(input.capture_completion_turn)
            .unwrap_or(1)
            .max(1);
        let mut plan =
            simulate_state_with_catalog(input, &catalog, &SearchState::default(), search_turns);
        plan.candidates_considered = 1;
        return Some(plan);
    }

    let search_turns = input.hard_deadline.unwrap_or(DEFAULT_SEARCH_TURNS).max(1);
    if input.production_options.is_empty() {
        // 空き生産枠が無い手番でも、既存編成だけで任務を継続できるかは評価する。
        // ここでNoneにすると永続Planの実行・予実監視まで途切れてしまう。
        let mut plan =
            simulate_state_with_catalog(input, &catalog, &SearchState::default(), search_turns);
        plan.candidates_considered = 1;
        return Some(plan);
    }
    if input.exact_property_control {
        return plan_current_property_control_exact(input, &catalog, search_turns);
    }
    let mut frontier = vec![SearchState::default()];
    // 全中間案の敵別forecastを保持すると長期戦でメモリと比較時間が膨張する。
    // 最終選択に必要な3案だけを逐次更新し、各候補のsimulateも1回に限定する。
    let mut best_within_deadline: Option<ForcePackagePlan> = None;
    let mut best_feasible: Option<ForcePackagePlan> = None;
    let mut best_effort: Option<ForcePackagePlan> = None;
    let mut considered = 0_usize;
    let mut truncated = false;

    // 実在するfacility-turnを1枠ずつ処理する。同じ枠の全兵種を深さごとに
    // 再展開すると、首都攻略のような長い購入列で同じ組合せを大量に作る。
    // 「作らない」または「この枠で1兵種を作る」を一度だけ分岐すれば、探索対象を
    // 減らさずにゲームルールの1施設1生産へ一致させられる。
    let mut options_by_slot: HashMap<ProductionSlot, Vec<usize>> = HashMap::new();
    for (index, option) in input.production_options.iter().enumerate() {
        options_by_slot
            .entry(option.purchase.into())
            .or_default()
            .push(index);
    }
    let mut candidates_pruned = 0_usize;
    for option_indices in options_by_slot.values_mut() {
        let original_len = option_indices.len();
        let retained = option_indices
            .iter()
            .copied()
            .filter(|candidate_index| {
                !option_indices.iter().copied().any(|other_index| {
                    other_index != *candidate_index
                        && production_option_dominates(
                            input,
                            &input.production_options[other_index],
                            &input.production_options[*candidate_index],
                        )
                })
            })
            .collect::<Vec<_>>();
        candidates_pruned = candidates_pruned.saturating_add(original_len - retained.len());
        *option_indices = retained;
    }
    let mut slots = options_by_slot.into_iter().collect::<Vec<_>>();
    slots.sort_unstable_by_key(|(slot, _)| (slot.build_turn, slot.facility.y, slot.facility.x));

    for (slot, option_indices) in slots {
        let mut next = Vec::new();
        for state in &frontier {
            // この枠を使わない案も残す。高額兵種のための現金予約や、将来の別施設を
            // 選ぶ案を、安い現在購入で強制的に上書きしないためである。
            next.push(state.clone());
            for option_index in &option_indices {
                let option = &input.production_options[*option_index];
                if option
                    .capture_target
                    .is_some_and(|target| state.used_capture_targets.contains(&target))
                {
                    continue;
                }
                let next_cost = state.cost.saturating_add(option.purchase.cost);
                let available_by_build_turn = input.current_funds.saturating_add(
                    input
                        .income_per_turn
                        .saturating_mul(option.purchase.build_turn),
                );
                if next_cost > available_by_build_turn {
                    continue;
                }
                let mut child = state.clone();
                child.option_indices.push(*option_index);
                child.used_slots.insert(slot);
                if let Some(target) = option.capture_target {
                    child.used_capture_targets.insert(target);
                }
                child.cost = next_cost;
                next.push(child);
            }
        }

        // 同じbeam層の各状態はsnapshotだけを読む独立計算である。nativeでは並列評価し、
        // 結果は入力順へ戻してから従来どおり最良案を逐次更新するため、同点時の選択は不変。
        let evaluated = crate::ai::deterministic_parallel::map_ordered(next, |state| {
            let plan = simulate_state_with_catalog(input, &catalog, &state, search_turns);
            (state, plan)
        });
        let mut evaluated_next = Vec::with_capacity(evaluated.len());
        for (state, mut plan) in evaluated {
            considered = considered.saturating_add(1);
            plan.candidates_considered = considered;
            if plan.feasible {
                update_best_feasible(&mut best_feasible, &plan, input.delay_cost_per_turn);
                if !input.prioritize_deadline_interdiction
                    && input.hard_deadline.is_some_and(|deadline| {
                        plan.elimination_turn.is_some_and(|turn| turn <= deadline)
                    })
                {
                    update_best_feasible(
                        &mut best_within_deadline,
                        &plan,
                        input.delay_cost_per_turn,
                    );
                }
            }
            // Interdictの成立条件は敵全滅ではなく、敵の占領完了より前に合法な
            // 初撃を入れること。短い期限内に全滅まで要求して成立案を捨てない。
            if input.prioritize_deadline_interdiction
                && plan.interdiction_contracts_satisfied(&input.interdiction_deadlines)
            {
                update_best_sufficient_interdiction(
                    &mut best_within_deadline,
                    &plan,
                    input.deadline_target_index,
                );
            }
            if input.prioritize_deadline_interdiction {
                update_best_effort_interdiction(
                    &mut best_effort,
                    &plan,
                    input.deadline_target_index,
                );
            } else {
                update_best_effort(&mut best_effort, &plan);
            }
            evaluated_next.push((state, plan));
        }
        let beam_width = input.search_beam_width.clamp(1, SEARCH_BEAM_WIDTH);
        if evaluated_next.len() > beam_width {
            truncated = true;
            if input.prioritize_deadline_interdiction {
                // 物件レースでは高速・安価な妨害を枝刈りで消さない。総ダメージより
                // 早い最初の一撃と安い編成を優先して beam を残す。
                evaluated_next.sort_by_key(|(_, plan)| {
                    (
                        plan.missed_interdiction_contracts(&input.interdiction_deadlines),
                        plan.interdiction_first_attack_turn().unwrap_or(u32::MAX),
                        plan.production_cost,
                        plan.expected_loss,
                        plan.interdiction_remaining_hp(input.deadline_target_index),
                    )
                });
            } else {
                evaluated_next.sort_by_key(|(_, plan)| {
                    (
                        plan.remaining_hp(),
                        plan.expected_loss,
                        plan.completion_for_ordering(),
                        plan.production_cost,
                    )
                });
            }
            evaluated_next.truncate(beam_width);
        }
        frontier = evaluated_next.into_iter().map(|(state, _)| state).collect();
        if frontier.is_empty() {
            break;
        }
    }

    let mut selected = best_within_deadline
        .or(best_feasible)
        // 期限内に全滅できる案が無くても、何も作らず停止しない。
        .or(best_effort)?;
    if !selected.feasible {
        // 実行不能な将来列のために現在の空き施設と現金を寝かせない。今出せる
        // 直接戦闘要員を前線screenとして加え、資金が競合する最も遅い予定から
        // 次revisionへ送り返す。実行可能案の高価な必須counter予約には触れない。
        selected = fill_best_effort_current_screen(input, &catalog, selected, search_turns);
    }
    // 同じ施設・同じ兵種を後の手番に置く理由がなく、資金も足りるなら最早枠へ寄せる。
    // beam探索では将来収入で複数機を買う枝が残りやすいため、編成を変えずに
    // 生産だけを前倒しして「計画はあるのに今手番の施設が遊ぶ」状態を除く。
    let shifted_purchases = left_shift_purchases(input, &selected.purchases);
    if shifted_purchases != selected.purchases
        && let Ok(mut shifted) = evaluate_fixed_package(input, &shifted_purchases)
    {
        shifted.candidates_considered = considered;
        shifted.search_truncated = truncated;
        selected = shifted;
    }
    selected.candidates_considered = considered;
    selected.candidates_pruned = candidates_pruned;
    selected.search_truncated = truncated;
    Some(selected)
}

/// 期限付き物件レースでは、今手番の全施設について合法な生産組合せを全列挙する。
///
/// 毎手番盤面を再観測するRolling Planなので、未観測の将来生産を固定幅beamへ混ぜず、
/// 現在確定している資金・施設・相性・接敵時刻だけを厳密に比較する。成立案同士は余剰火力を
/// 買わず費用と損耗を比較し、不成立案でも未達契約数を最優先するため総ダメージfallbackへ
/// 落とさない。
fn plan_current_property_control_exact(
    input: &RollingPlanInput,
    catalog: &SimulationCatalog,
    search_turns: u32,
) -> Option<ForcePackagePlan> {
    let mut options_by_facility: HashMap<GridPosition, Vec<usize>> = HashMap::new();
    for (index, option) in input.production_options.iter().enumerate() {
        if option.purchase.build_turn == 0 {
            options_by_facility
                .entry(option.purchase.facility)
                .or_default()
                .push(index);
        }
    }
    let mut facilities = options_by_facility.into_iter().collect::<Vec<_>>();
    facilities.sort_unstable_by_key(|(facility, _)| (facility.y, facility.x));

    let mut states = vec![SearchState::default()];
    for (facility, option_indices) in facilities {
        let slot = ProductionSlot {
            facility,
            build_turn: 0,
        };
        let mut next = Vec::new();
        for state in states {
            // 施設を使わない案も、他施設の高価なcounterへ資金を残す合法手として比較する。
            next.push(state.clone());
            for option_index in &option_indices {
                let option = &input.production_options[*option_index];
                if option
                    .capture_target
                    .is_some_and(|target| state.used_capture_targets.contains(&target))
                {
                    continue;
                }
                let next_cost = state.cost.saturating_add(option.purchase.cost);
                if next_cost > input.current_funds {
                    continue;
                }
                let mut child = state.clone();
                child.option_indices.push(*option_index);
                child.used_slots.insert(slot);
                if let Some(target) = option.capture_target {
                    child.used_capture_targets.insert(target);
                }
                child.cost = next_cost;
                next.push(child);
            }
        }
        states = next;
    }

    let considered = states.len();
    // T6は物件到達・妨害の締切であり、戦闘そのものをT6で打ち切る期限ではない。
    // 締切判定はinterdiction_deadlinesに残したまま、弾薬と相性を通常の作戦期間まで
    // 実シミュレーションし、「期限は守るが別兵科を攻撃不能」な安価案を弾く。
    let combat_simulation_turns = search_turns.max(DEFAULT_SEARCH_TURNS);
    let evaluated = crate::ai::deterministic_parallel::map_ordered(states, |state| {
        simulate_state_with_catalog(input, catalog, &state, combat_simulation_turns)
    });
    let mut selected: Option<ForcePackagePlan> = None;
    for candidate in evaluated {
        if selected
            .as_ref()
            .is_none_or(|current| exact_property_plan_better(input, &candidate, current))
        {
            selected = Some(candidate);
        }
    }
    let mut selected = selected?;
    selected.candidates_considered = considered;
    selected.candidates_pruned = 0;
    selected.search_truncated = false;
    Some(selected)
}

/// 物件契約の辞書順比較。固定評価点や兵種名を使わない。
fn exact_property_plan_better(
    input: &RollingPlanInput,
    candidate: &ForcePackagePlan,
    current: &ForcePackagePlan,
) -> bool {
    if input.interdiction_deadlines.is_empty() {
        // 全滅が達成可能（feasible）な案同士の比較では、占領達成と全滅完了を優先し、
        // そのうえで余分な買い足しを避けて期待損失最小化・費用最小化で経済効率を高める。
        if candidate.feasible && current.feasible {
            return (
                std::cmp::Reverse(candidate.deadline_capture_survivor_count),
                candidate.capture_completion_profile(),
                candidate.occupation_turn,
                candidate.expected_loss,
                candidate.production_cost,
                std::cmp::Reverse(candidate.surviving_combat_value),
            ) < (
                std::cmp::Reverse(current.deadline_capture_survivor_count),
                current.capture_completion_profile(),
                current.occupation_turn,
                current.expected_loss,
                current.production_cost,
                std::cmp::Reverse(current.surviving_combat_value),
            );
        }
        if candidate.feasible != current.feasible {
            return candidate.feasible;
        }

        // 全滅未達（敵が多数存在し、突破・前線維持が必要な場合）
        // 最早突破時刻、占領レーン数、敵残HP最小化、被ダメ損失最小化、生存戦力価値最大化で比較
        return (
            candidate.front_breakthrough_rank(),
            std::cmp::Reverse(candidate.deadline_capture_survivor_count),
            candidate.capture_completion_profile(),
            candidate.remaining_hp(),
            candidate.expected_loss,
            std::cmp::Reverse(candidate.surviving_combat_value),
            candidate.production_cost,
        ) < (
            current.front_breakthrough_rank(),
            std::cmp::Reverse(current.deadline_capture_survivor_count),
            current.capture_completion_profile(),
            current.remaining_hp(),
            current.expected_loss,
            std::cmp::Reverse(current.surviving_combat_value),
            current.production_cost,
        );
    }

    let candidate_satisfied = candidate.property_contracts_satisfied(
        &input.interdiction_deadlines,
        input.required_capture_survivors,
    );
    let current_satisfied = current.property_contracts_satisfied(
        &input.interdiction_deadlines,
        input.required_capture_survivors,
    );
    if candidate_satisfied != current_satisfied {
        return candidate_satisfied;
    }
    if candidate_satisfied {
        if candidate.feasible && current.feasible {
            return (
                candidate.expected_loss,
                candidate.production_cost,
                std::cmp::Reverse(candidate.surviving_combat_value),
                candidate
                    .interdiction_first_attack_turn()
                    .unwrap_or(u32::MAX),
            ) < (
                current.expected_loss,
                current.production_cost,
                std::cmp::Reverse(current.surviving_combat_value),
                current.interdiction_first_attack_turn().unwrap_or(u32::MAX),
            );
        }
        if candidate.feasible != current.feasible {
            return candidate.feasible;
        }
        // 契約を満たした案同士では、敵残存HPを削り、被ダメージを抑え、生存戦力価値（NPV）を
        // 最大化する案を優先する。これらが同等なら余剰資金温存のため費用最小を選ぶ。
        return (
            candidate.interdiction_remaining_hp(input.deadline_target_index),
            candidate.expected_loss,
            std::cmp::Reverse(candidate.surviving_combat_value),
            candidate
                .interdiction_first_attack_turn()
                .unwrap_or(u32::MAX),
            candidate.production_cost,
        ) < (
            current.interdiction_remaining_hp(input.deadline_target_index),
            current.expected_loss,
            std::cmp::Reverse(current.surviving_combat_value),
            current.interdiction_first_attack_turn().unwrap_or(u32::MAX),
            current.production_cost,
        );
    }

    let candidate_attack_turns = candidate
        .target_first_attack_turns
        .iter()
        .map(|turn| turn.unwrap_or(u32::MAX))
        .collect::<Vec<_>>();
    let current_attack_turns = current
        .target_first_attack_turns
        .iter()
        .map(|turn| turn.unwrap_or(u32::MAX))
        .collect::<Vec<_>>();
    let required = input.required_capture_survivors;
    (
        required.saturating_sub(
            candidate.completed_property_contracts(&input.interdiction_deadlines, required),
        ),
        candidate_attack_turns,
        candidate.remaining_hp(),
        candidate.capture_survivor_shortfall(required),
        candidate.expected_loss,
        std::cmp::Reverse(candidate.surviving_combat_value),
        candidate.production_cost,
    ) < (
        required.saturating_sub(
            current.completed_property_contracts(&input.interdiction_deadlines, required),
        ),
        current_attack_turns,
        current.remaining_hp(),
        current.capture_survivor_shortfall(required),
        current.expected_loss,
        std::cmp::Reverse(current.surviving_combat_value),
        current.production_cost,
    )
}

/// 同じ工場・同じ手番の候補で、価格・交戦対象・相性の全てで劣る兵種を落とす。
///
/// ある敵への打点だけが高い候補を消さないよう、右辺が届く全敵へ左辺も届き、
/// 与ダメージは同等以上かつ被ダメージは同等以下の場合だけ支配とみなす。
fn production_option_dominates(
    input: &RollingPlanInput,
    left: &ProductionPlanOption,
    right: &ProductionPlanOption,
) -> bool {
    if left.capture_target != right.capture_target
        || left.purchase.cost > right.purchase.cost
        || !right
            .engageable_enemy_indices
            .iter()
            .all(|index| left.engageable_enemy_indices.contains(index))
    {
        return false;
    }
    let matchup_not_worse = right.engageable_enemy_indices.iter().all(|index| {
        let enemy = &input.enemies[*index];
        let left_outgoing = best_damage(
            &input.damage_chart,
            left.stats.unit_type,
            enemy.stats.unit_type,
        );
        let right_outgoing = best_damage(
            &input.damage_chart,
            right.stats.unit_type,
            enemy.stats.unit_type,
        );
        let left_incoming = best_damage(
            &input.damage_chart,
            enemy.stats.unit_type,
            left.stats.unit_type,
        );
        let right_incoming = best_damage(
            &input.damage_chart,
            enemy.stats.unit_type,
            right.stats.unit_type,
        );
        left_outgoing >= right_outgoing && left_incoming <= right_incoming
    });
    let strictly_better = left.purchase.cost < right.purchase.cost
        || left.engageable_enemy_indices.len() > right.engageable_enemy_indices.len()
        || right.engageable_enemy_indices.iter().any(|index| {
            let enemy = &input.enemies[*index];
            best_damage(
                &input.damage_chart,
                left.stats.unit_type,
                enemy.stats.unit_type,
            ) > best_damage(
                &input.damage_chart,
                right.stats.unit_type,
                enemy.stats.unit_type,
            ) || best_damage(
                &input.damage_chart,
                enemy.stats.unit_type,
                left.stats.unit_type,
            ) < best_damage(
                &input.damage_chart,
                enemy.stats.unit_type,
                right.stats.unit_type,
            )
        });
    matchup_not_worse && strictly_better
}

/// best-effort案の未使用current slotを、即時投入できる直接戦闘unitで埋める。
fn fill_best_effort_current_screen(
    input: &RollingPlanInput,
    catalog: &SimulationCatalog,
    selected: ForcePackagePlan,
    search_turns: u32,
) -> ForcePackagePlan {
    let mut purchases = selected.purchases.clone();
    let mut used_current_facilities = purchases
        .iter()
        .filter(|purchase| purchase.build_turn == 0)
        .map(|purchase| purchase.facility)
        .collect::<HashSet<_>>();
    let mut current_cost = purchases
        .iter()
        .filter(|purchase| purchase.build_turn == 0)
        .map(|purchase| purchase.cost)
        .fold(0_u32, u32::saturating_add);
    let current_budget = input.current_funds;

    let mut facilities = input
        .production_options
        .iter()
        .filter(|option| option.purchase.build_turn == 0)
        .map(|option| option.purchase.facility)
        .collect::<Vec<_>>();
    facilities.sort_unstable_by_key(|facility| (facility.y, facility.x));
    facilities.dedup();

    for facility in facilities {
        if used_current_facilities.contains(&facility) {
            continue;
        }
        let affordable = current_budget.saturating_sub(current_cost);
        let candidate = input
            .production_options
            .iter()
            .filter(|option| {
                option.purchase.build_turn == 0
                    && option.purchase.facility == facility
                    && option.purchase.cost <= affordable
                    && option.stats.min_range <= 1
                    && !option.engageable_enemy_indices.is_empty()
            })
            .max_by_key(|option| {
                let outgoing = option
                    .engageable_enemy_indices
                    .iter()
                    .map(|index| {
                        best_damage(
                            &input.damage_chart,
                            option.stats.unit_type,
                            input.enemies[*index].stats.unit_type,
                        )
                    })
                    .max()
                    .unwrap_or_default();
                let incoming = input
                    .enemies
                    .iter()
                    .map(|enemy| {
                        best_damage(
                            &input.damage_chart,
                            enemy.stats.unit_type,
                            option.stats.unit_type,
                        )
                    })
                    .max()
                    .unwrap_or_default();
                let exchange = outgoing.saturating_mul(100) / incoming.saturating_add(1);
                (
                    exchange,
                    outgoing,
                    std::cmp::Reverse(incoming),
                    std::cmp::Reverse(option.purchase.cost),
                )
            });
        let Some(candidate) = candidate else {
            continue;
        };
        purchases.push(candidate.purchase);
        used_current_facilities.insert(facility);
        current_cost = current_cost.saturating_add(candidate.purchase.cost);
    }

    build_augmented_plan(input, catalog, selected, purchases, search_turns)
}

/// 即時screenを優先した結果、将来収入で払えなくなった最遠の予定だけを外し、
/// 残った購入列を実評価して返す。
fn build_augmented_plan(
    input: &RollingPlanInput,
    catalog: &SimulationCatalog,
    selected: ForcePackagePlan,
    mut purchases: Vec<PlannedPurchase>,
    search_turns: u32,
) -> ForcePackagePlan {
    while !funding_suffices(input, &purchases) {
        let Some((index, _)) = purchases
            .iter()
            .enumerate()
            .filter(|(_, purchase)| purchase.build_turn > 0)
            .max_by_key(|(_, purchase)| purchase.build_turn)
        else {
            return selected;
        };
        purchases.remove(index);
    }

    let mut indexed = Vec::with_capacity(purchases.len());
    for purchase in &purchases {
        let Some(index) = input
            .production_options
            .iter()
            .position(|option| option.purchase == *purchase)
        else {
            return selected;
        };
        indexed.push(index);
    }
    let state = SearchState {
        option_indices: indexed,
        used_slots: purchases
            .iter()
            .copied()
            .map(ProductionSlot::from)
            .collect(),
        used_capture_targets: HashSet::new(),
        cost: purchases
            .iter()
            .map(|purchase| purchase.cost)
            .fold(0_u32, u32::saturating_add),
    };
    let mut augmented = simulate_state_with_catalog(input, catalog, &state, search_turns);
    augmented.candidates_considered = selected.candidates_considered;
    augmented.search_truncated = selected.search_truncated;
    augmented
}

fn update_best_feasible(
    best: &mut Option<ForcePackagePlan>,
    candidate: &ForcePackagePlan,
    delay_cost_per_turn: u32,
) {
    let candidate_key = (
        u8::from(!candidate.overmatch_ready),
        candidate.utility_cost(delay_cost_per_turn),
        candidate.completion_for_ordering(),
        candidate.production_cost,
    );
    if best.as_ref().is_none_or(|current| {
        candidate_key
            < (
                u8::from(!current.overmatch_ready),
                current.utility_cost(delay_cost_per_turn),
                current.completion_for_ordering(),
                current.production_cost,
            )
    }) {
        *best = Some(candidate.clone());
    }
}

/// 期限付き目標の fallback 比較。
///
/// 全滅が期限内に成立しない場合でも、実際の初撃が早く、同じ初撃時刻なら敵HPを
/// 多く残さない編成を選ぶ。兵種名・移動力・施設順の固定値ではなく、シミュレーション済みの
/// 到達・射程・ダメージだけを比較する。
fn update_best_effort_interdiction(
    best: &mut Option<ForcePackagePlan>,
    candidate: &ForcePackagePlan,
    target_index: Option<usize>,
) {
    let candidate_key = (
        candidate
            .interdiction_first_attack_turn()
            .unwrap_or(u32::MAX),
        candidate.interdiction_remaining_hp(target_index),
        candidate.expected_loss,
        std::cmp::Reverse(candidate.surviving_combat_value),
        candidate.production_cost,
    );
    if best.as_ref().is_none_or(|current| {
        candidate_key
            < (
                current.interdiction_first_attack_turn().unwrap_or(u32::MAX),
                current.interdiction_remaining_hp(target_index),
                current.expected_loss,
                std::cmp::Reverse(current.surviving_combat_value),
                current.production_cost,
            )
    }) {
        *best = Some(candidate.clone());
    }
}

/// 期限前に合法な初撃を入れられる案同士の比較。
///
/// 単に最小費用を優先すると安価で脆弱な兵種ばかり選ばれ、前線の戦闘優位が失われる。
/// 初撃達成可能案同士では、残存軍事価値（NPV）の最大化、敵残HP最小化、被ダメージ損失の
/// 最小化を優先し、それらが同等の場合にのみ経済効率（最小費用）を比較する。
fn update_best_sufficient_interdiction(
    best: &mut Option<ForcePackagePlan>,
    candidate: &ForcePackagePlan,
    target_index: Option<usize>,
) {
    let candidate_key = (
        candidate
            .interdiction_first_attack_turn()
            .unwrap_or(u32::MAX),
        candidate.interdiction_remaining_hp(target_index),
        candidate.expected_loss,
        std::cmp::Reverse(candidate.surviving_combat_value),
        candidate.production_cost,
    );
    if best.as_ref().is_none_or(|current| {
        candidate_key
            < (
                current.interdiction_first_attack_turn().unwrap_or(u32::MAX),
                current.interdiction_remaining_hp(target_index),
                current.expected_loss,
                std::cmp::Reverse(current.surviving_combat_value),
                current.production_cost,
            )
    }) {
        *best = Some(candidate.clone());
    }
}

fn update_best_effort(best: &mut Option<ForcePackagePlan>, candidate: &ForcePackagePlan) {
    let candidate_key = (
        candidate.remaining_hp(),
        candidate.expected_loss,
        candidate.first_attack_turn.unwrap_or(u32::MAX),
        candidate.production_cost,
    );
    if best.as_ref().is_none_or(|current| {
        candidate_key
            < (
                current.remaining_hp(),
                current.expected_loss,
                current.first_attack_turn.unwrap_or(u32::MAX),
                current.production_cost,
            )
    }) {
        *best = Some(candidate.clone());
    }
}

/// 選ばれた編成を変えず、各購入を最も早い実行可能枠へ移す。
///
/// 候補は作戦地点へ交戦可能な施設・兵種だけに絞り込み済みなので、新たに確保した
/// 生産施設も利用する。仮置きするたび全手番の累積資金を確認し、将来収入の先食いは
/// 許さない。
fn left_shift_purchases(
    input: &RollingPlanInput,
    purchases: &[PlannedPurchase],
) -> Vec<PlannedPurchase> {
    let mut ordered = purchases.to_vec();
    ordered.sort_unstable_by_key(|purchase| {
        (
            purchase.build_turn,
            purchase.facility.y,
            purchase.facility.x,
        )
    });

    let mut shifted = Vec::with_capacity(ordered.len());
    let mut used_slots = HashSet::new();
    // まだ処理していない購入の元予定枠は先に確保する。前倒し先がその枠を奪うと、
    // 後続購入が元へ戻ったときに同一施設・同一手番の重複が発生する。
    let mut reserved_original_slots = ordered
        .iter()
        .copied()
        .map(ProductionSlot::from)
        .collect::<HashSet<_>>();
    for purchase in ordered {
        let original_slot = ProductionSlot::from(purchase);
        reserved_original_slots.remove(&original_slot);
        let mut candidates = input
            .production_options
            .iter()
            .map(|option| option.purchase)
            .filter(|candidate| {
                candidate.unit_type == purchase.unit_type
                    && candidate.build_turn <= purchase.build_turn
            })
            .collect::<Vec<_>>();
        candidates.sort_unstable_by_key(|candidate| {
            (
                candidate.build_turn,
                candidate.facility != purchase.facility,
                candidate.facility.y,
                candidate.facility.x,
            )
        });

        let replacement = candidates.into_iter().find(|candidate| {
            let slot = ProductionSlot::from(*candidate);
            if used_slots.contains(&slot) || reserved_original_slots.contains(&slot) {
                return false;
            }
            let mut tentative = shifted.clone();
            tentative.push(*candidate);
            funding_suffices(input, &tentative)
        });
        let selected = replacement.unwrap_or(purchase);
        used_slots.insert(ProductionSlot::from(selected));
        shifted.push(selected);
    }
    shifted.sort_unstable_by_key(|purchase| {
        (
            purchase.build_turn,
            purchase.facility.y,
            purchase.facility.x,
        )
    });
    shifted
}

fn funding_suffices(input: &RollingPlanInput, purchases: &[PlannedPurchase]) -> bool {
    let last_turn = purchases
        .iter()
        .map(|purchase| purchase.build_turn)
        .max()
        .unwrap_or(0);
    (0..=last_turn).all(|turn| {
        let required: u32 = purchases
            .iter()
            .filter(|purchase| purchase.build_turn <= turn)
            .map(|purchase| purchase.cost)
            .sum();
        let available = input
            .current_funds
            .saturating_add(input.income_per_turn.saturating_mul(turn));
        required <= available
    })
}

/// 前revisionの未実行購入列を、現在盤面の生産候補と資金へ載せ直して再評価する。
///
/// 新しい最適案を探す関数とは分離し、現行案を候補集合から消さずに比較できるようにする。
/// ここで失敗した計画だけが「実行不能」として撤回候補になる。
pub(crate) fn evaluate_fixed_package(
    input: &RollingPlanInput,
    purchases: &[PlannedPurchase],
) -> Result<ForcePackagePlan, FixedPackageError> {
    // 編成は固定したまま、現在利用できる新しい施設を含めて最早枠へ載せ直す。
    // 同じ兵種が同じ時点で前線へ参加できるなら、古い施設座標に計画を縛らない。
    let scheduled = left_shift_purchases(input, purchases);
    let mut indexed = scheduled
        .iter()
        .map(|purchase| {
            input
                .production_options
                .iter()
                .position(|option| option.purchase == *purchase)
                .map(|index| (purchase.build_turn, index))
                .ok_or(FixedPackageError::ProductionSlotUnavailable)
        })
        .collect::<Result<Vec<_>, _>>()?;
    indexed.sort_unstable_by_key(|(build_turn, _)| *build_turn);

    let mut state = SearchState::default();
    for (build_turn, option_index) in indexed {
        let option = &input.production_options[option_index];
        let slot = ProductionSlot {
            facility: option.purchase.facility,
            build_turn,
        };
        if !state.used_slots.insert(slot) {
            return Err(FixedPackageError::DuplicateProductionSlot);
        }
        state.cost = state.cost.saturating_add(option.purchase.cost);
        let available = input
            .current_funds
            .saturating_add(input.income_per_turn.saturating_mul(build_turn));
        if state.cost > available {
            return Err(FixedPackageError::FundingUnavailable);
        }
        state.option_indices.push(option_index);
    }

    let search_turns = input.hard_deadline.unwrap_or(DEFAULT_SEARCH_TURNS).max(1);
    let catalog = SimulationCatalog::new(input);
    Ok(simulate_state_with_catalog(
        input,
        &catalog,
        &state,
        search_turns,
    ))
}

#[cfg(test)]
fn simulate_state(
    input: &RollingPlanInput,
    state: &SearchState,
    search_turns: u32,
) -> ForcePackagePlan {
    let catalog = SimulationCatalog::new(input);
    simulate_state_with_catalog(input, &catalog, state, search_turns)
}

fn simulate_state_with_catalog(
    input: &RollingPlanInput,
    catalog: &SimulationCatalog,
    state: &SearchState,
    search_turns: u32,
) -> ForcePackagePlan {
    let purchases: Vec<_> = state
        .option_indices
        .iter()
        .map(|index| input.production_options[*index].purchase)
        .collect();
    let capture_purchases = state
        .option_indices
        .iter()
        .filter_map(|index| {
            input.production_options[*index]
                .capture_target
                .map(|target| PlannedCapturePurchase {
                    purchase: input.production_options[*index].purchase,
                    target,
                    completion_turn: input.production_options[*index].capture_completion_turn,
                })
        })
        .collect::<Vec<_>>();
    let selected_capture_completion_turns = state
        .option_indices
        .iter()
        .filter_map(|index| {
            let option = &input.production_options[*index];
            option
                .capture_target
                .map(|_| option.capture_completion_turn)
        })
        .collect::<Vec<_>>();
    let mut friendlies = catalog.existing_units.clone();
    let mut protected_units = catalog.protected_units.clone();
    for index in &state.option_indices {
        if input.production_options[*index].capture_target.is_some() {
            protected_units.push(catalog.production_units[*index].clone());
        } else {
            friendlies.push(catalog.production_units[*index].clone());
        }
    }
    let selected_capture_completion_turn = selected_capture_completion_turns
        .iter()
        .filter_map(|turn| *turn)
        .chain(input.capture_completion_turn)
        .max();
    let mut enemies = catalog.enemies.clone();
    let mut first_attack_turn = None;
    let mut front_breakthrough_turn = None;
    let mut deadline_target_first_attack_turn = None;
    let mut target_first_attack_turns = vec![None; enemies.len()];
    let mut turn_forecasts = Vec::new();
    let mut combat_purchases = Vec::new();
    let mut assigned_combat_purchases = HashSet::new();
    let mut capture_survival_at_completion = vec![None; selected_capture_completion_turns.len()];
    let mut protected_survivors_at_completion = None;

    for turn in 1..=search_turns {
        let enemy_arrival_hp = enemies
            .iter()
            .filter(|enemy| enemy.source.available_turn == turn)
            .map(|enemy| enemy.source.hp)
            .sum();
        let enemy_hp_before: u32 = enemies
            .iter()
            .filter(|enemy| enemy.source.available_turn <= turn)
            .map(|enemy| enemy.hp)
            .sum();
        let friendly_hp_before: u32 = friendlies
            .iter()
            .chain(protected_units.iter())
            .map(|friendly| friendly.hp)
            .sum();
        let mut attack_count = 0_u32;
        // 物件前線では、各攻撃役が別の敵を少しずつ削る予測では突破能力を測れない。
        // この手番に同じ実在敵へ合法に届く全火力を合算し、撃破可能ならその敵へ
        // 集中させる。兵種名・価格・座標ではなく、実相性、HP、射程、経路ETAだけで決める。
        let front_breakthrough_target = (input.exact_property_control
            && input.interdiction_deadlines.is_empty())
        .then(|| select_front_breakthrough_target(&friendlies, &enemies, turn))
        .flatten();
        if front_breakthrough_target.is_some() {
            front_breakthrough_turn.get_or_insert(turn);
        }
        for friendly in &mut friendlies {
            if friendly.hp == 0 || friendly.attacks_left == 0 || turn < friendly.available_turn {
                continue;
            }
            let concentrated_target = front_breakthrough_target.and_then(|target_index| {
                let enemy = enemies.get(target_index)?;
                let profile = friendly
                    .attack_profiles
                    .get(target_index)
                    .copied()
                    .flatten()?;
                (enemy.hp > 0 && turn >= enemy.source.available_turn && turn >= profile.ready_turn)
                    .then_some((target_index, profile))
            });
            let Some((target_index, attack_profile)) = concentrated_target.or_else(|| {
                select_target(
                    friendly,
                    &enemies,
                    turn,
                    &input.interdiction_deadlines,
                    &target_first_attack_turns,
                )
            }) else {
                continue;
            };
            let target = &mut enemies[target_index];
            let damage = calculate_damage_formula(
                attack_profile.base_damage,
                friendly.hp,
                target.source.defense_bonus,
                false,
            );
            if damage == 0 {
                continue;
            }
            first_attack_turn.get_or_insert(turn);
            if input.deadline_target_index == Some(target_index) {
                deadline_target_first_attack_turn.get_or_insert(turn);
            }
            target_first_attack_turns[target_index].get_or_insert(turn);
            if let (Some(purchase), Some(target_entity)) = (friendly.purchase, target.source.entity)
                && assigned_combat_purchases.insert(purchase)
            {
                combat_purchases.push(PlannedCombatPurchase {
                    purchase,
                    target: target_entity,
                });
            }
            attack_count = attack_count.saturating_add(1);
            target.hp = target.hp.saturating_sub(damage);
            friendly.attacks_left = friendly.attacks_left.saturating_sub(1);
            if target.hp == 0 {
                target.destroyed_turn = Some(turn);
                continue;
            }

            // 与damageと初撃ETAは従来の編成比較軸を維持し、反撃可否だけを
            // 計画交戦距離における本番武器選択へ委譲する。
            let counter_base = attack_profile.incoming_damage;
            if counter_base > 0 {
                let counter =
                    calculate_damage_formula(counter_base, target.hp, 0, true).saturating_add(10);
                friendly.hp = friendly.hp.saturating_sub(counter);
            }
        }
        // 敵砲兵・直接戦闘unitも手番ごとに最も有利な友軍を攻撃する。
        // 占領兵だけを損耗対象にすると、射程へ入った護衛砲台が無傷という予測になり、
        // 間接火力だけの脆い編成を過大評価する。
        for (enemy_index, enemy) in enemies.iter_mut().enumerate() {
            if enemy.hp == 0 || enemy.attacks_left == 0 || turn < enemy.source.available_turn {
                continue;
            }
            let combat_target = friendlies
                .iter()
                .enumerate()
                .filter(|(_, target)| target.hp > 0 && turn >= target.available_turn)
                .filter_map(|(index, target)| {
                    let profile = target.enemy_attack_profiles[enemy_index]?;
                    if turn < profile.contact_turn {
                        return None;
                    }
                    let damage = calculate_damage_formula(profile.base_damage, enemy.hp, 0, true)
                        .saturating_add(10);
                    Some((index, damage))
                })
                .max_by_key(|(index, damage)| {
                    (*damage, 100_u32.saturating_sub(friendlies[*index].hp))
                });
            let protected_target = protected_units
                .iter()
                .enumerate()
                .filter(|(_, target)| target.hp > 0 && turn >= target.available_turn)
                .filter_map(|(index, target)| {
                    let profile = target.enemy_attack_profiles[enemy_index]?;
                    if turn < profile.contact_turn {
                        return None;
                    }
                    let damage = calculate_damage_formula(profile.base_damage, enemy.hp, 0, true)
                        .saturating_add(10);
                    Some((index, damage))
                })
                .max_by_key(|(index, damage)| {
                    (*damage, 100_u32.saturating_sub(protected_units[*index].hp))
                });
            let target = match (combat_target, protected_target) {
                (Some((_index, damage)), Some((protected_index, protected_damage)))
                    if protected_damage > damage =>
                {
                    (true, protected_index, protected_damage)
                }
                (Some((index, damage)), _) => (false, index, damage),
                (None, Some((index, damage))) => (true, index, damage),
                (None, None) => {
                    continue;
                }
            };
            if target.0 {
                protected_units[target.1].hp =
                    protected_units[target.1].hp.saturating_sub(target.2);
            } else {
                friendlies[target.1].hp = friendlies[target.1].hp.saturating_sub(target.2);
            }
            if target.2 == 0 {
                continue;
            }
            enemy.attacks_left = enemy.attacks_left.saturating_sub(1);
        }
        // 占領契約の成立は戦闘シミュレーション末尾ではなく、各物件を取り切った
        // その時点で判定する。占領後に戦線へ戻って撃破されても、取得済み物件を
        // 「未達」へ巻き戻して占領役を戦闘役へ付け替えない。
        for (purchase_index, completion_turn) in
            selected_capture_completion_turns.iter().enumerate()
        {
            if *completion_turn == Some(turn) {
                capture_survival_at_completion[purchase_index] = protected_units
                    .get(input.protected_units.len().saturating_add(purchase_index))
                    .map(|unit| unit.hp > 0);
            }
        }
        if selected_capture_completion_turn == Some(turn) {
            protected_survivors_at_completion =
                Some(protected_units.iter().filter(|unit| unit.hp > 0).count());
        }
        let enemy_hp_after: u32 = enemies
            .iter()
            .filter(|enemy| enemy.source.available_turn <= turn)
            .map(|enemy| enemy.hp)
            .sum();
        let friendly_hp_after: u32 = friendlies
            .iter()
            .chain(protected_units.iter())
            .map(|friendly| friendly.hp)
            .sum();
        turn_forecasts.push(CampaignTurnForecast {
            turn,
            enemy_arrival_hp,
            enemy_hp_removed: enemy_hp_before.saturating_sub(enemy_hp_after),
            friendly_hp_lost: friendly_hp_before.saturating_sub(friendly_hp_after),
            attack_count,
        });
        if enemies.iter().all(|enemy| enemy.hp == 0)
            && selected_capture_completion_turn.is_none_or(|capture_turn| turn >= capture_turn)
        {
            break;
        }
    }

    let elimination_turn = if enemies.is_empty() {
        Some(0)
    } else {
        enemies
            .iter()
            .map(|enemy| enemy.destroyed_turn)
            .collect::<Option<Vec<_>>>()
            .and_then(|turns| turns.into_iter().max())
    };
    // 物件別の合同探索では、対象物件と到達・完了時刻を持つCapture案だけを
    // 契約成立へ数える。単にcan_captureなCombat案を数えると、敵を追う歩兵と
    // 物件へ向かう歩兵を同一視し、実行時にもう一体を重複発注してしまう。
    let protected_survivor_count = protected_survivors_at_completion
        .unwrap_or_else(|| protected_units.iter().filter(|unit| unit.hp > 0).count())
        + if input.exact_property_control {
            0
        } else {
            friendlies
                .iter()
                .filter(|unit| unit.stats.can_capture && unit.hp > 0)
                .count()
        };
    let capture_deadline = input.hard_deadline.unwrap_or(search_turns);
    let deadline_capture_survivor_count = selected_capture_completion_turns
        .iter()
        .zip(&capture_survival_at_completion)
        .filter(|(completion, survived)| {
            completion.is_some_and(|turn| turn <= capture_deadline) && **survived == Some(true)
        })
        .count();
    // 占領完了ETAがまだ無いことは、占領役の生存要求が無いことを意味しない。
    // 物件別Interdictでは撃破完了前にETAを確定できないため、要求数そのものを判定する。
    let protected_force_survives = input.required_capture_survivors == 0
        || protected_survivor_count >= input.required_capture_survivors;
    // 固定2ターンを足さず、実campaignのPickup/Transit/Drop/Capture ETAと残敵排除の
    // 遅い方を採る。輸送編成がまだ存在しない場合は占領完了を予測しない。
    let occupation_turn = elimination_turn
        .zip(selected_capture_completion_turn)
        .filter(|_| protected_force_survives)
        .map(|(elimination, capture)| elimination.max(capture));
    let expected_loss =
        friendlies
            .iter()
            .chain(protected_units.iter())
            .fold(0_u32, |total, unit| {
                let lost_hp = unit.initial_hp.saturating_sub(unit.hp);
                total.saturating_add(unit.stats.cost.saturating_mul(lost_hp) / 100)
            });
    // `entity=None` はExpected scenarioとして投入した将来増援である。撃破後に
    // その半分の戦闘価値を残せる案を優先し、相手を倒した直後に前線が空になる
    // 最小編成を避ける。ただし達成不能でもfeasibleを偽にしない。
    let required_overmatch_value = input
        .enemies
        .iter()
        .filter(|enemy| enemy.entity.is_none())
        .map(|enemy| enemy.stats.cost.saturating_mul(enemy.hp) / 100)
        .fold(0_u32, u32::saturating_add)
        / 2;
    let surviving_combat_value = friendlies
        .iter()
        // 搭載能力は非戦闘の証拠ではない。実際に観測敵へ攻撃profileを持つ
        // 生存unitだけを戦闘価値へ数え、武装装甲車を輸送役として除外しない。
        .filter(|unit| unit.hp > 0 && unit.attack_profiles.iter().any(Option::is_some))
        .map(|unit| unit.stats.cost.saturating_mul(unit.hp) / 100)
        .fold(0_u32, u32::saturating_add);
    ForcePackagePlan {
        purchases,
        capture_purchases,
        combat_purchases,
        target_forecasts: enemies
            .into_iter()
            .map(|enemy| TargetForecast {
                entity: enemy.source.entity,
                unit_type: enemy.source.stats.unit_type,
                available_turn: enemy.source.available_turn,
                initial_hp: enemy.source.hp,
                remaining_hp: enemy.hp,
                destroyed_turn: enemy.destroyed_turn,
            })
            .collect(),
        turn_forecasts,
        feasible: elimination_turn.is_some() && protected_force_survives,
        first_attack_turn,
        front_breakthrough_turn,
        deadline_target_first_attack_turn,
        target_first_attack_turns,
        elimination_turn,
        occupation_turn,
        production_cost: state.cost,
        expected_loss,
        surviving_combat_value,
        required_overmatch_value,
        overmatch_ready: surviving_combat_value >= required_overmatch_value,
        protected_unit_count: input.protected_units.len(),
        protected_survivor_count,
        deadline_capture_survivor_count,
        required_capture_survivor_count: input.required_capture_survivors,
        candidates_considered: 0,
        candidates_pruned: 0,
        search_truncated: false,
    }
}

fn sim_friendly(
    input: &RollingPlanInput,
    source: &FriendlyPlanUnit,
    can_advance_to_enemy: bool,
) -> SimFriendly {
    let total_ammo = source
        .stats
        .max_ammo1
        .saturating_add(source.stats.max_ammo2)
        .max(1);
    let fuel_turns = source
        .stats
        .max_fuel
        .checked_div(source.stats.daily_fuel_consumption)
        .unwrap_or(u32::MAX);
    // 敵・友軍位置と兵種は1回のRolling Plan探索中は不変である。同じbeam内の
    // 数千候補で距離・相性・接触turnを再計算せず、敵indexに対応する表へ固定する。
    debug_assert!(
        source
            .engageable_enemy_indices
            .windows(2)
            .all(|pair| pair[0] < pair[1]),
        "交戦可能敵indexは盤面走査順の昇順で保持する"
    );
    let mut attack_profiles = Vec::with_capacity(input.enemies.len());
    let mut enemy_attack_profiles = Vec::with_capacity(input.enemies.len());
    for (enemy_index, enemy) in input.enemies.iter().enumerate() {
        let distance = input.map.distance(
            source.position.x,
            source.position.y,
            enemy.position.x,
            enemy.position.y,
        );
        let base_damage = best_damage(
            &input.damage_chart,
            source.stats.unit_type,
            enemy.stats.unit_type,
        );
        let can_attack = source
            .engageable_enemy_indices
            .binary_search(&enemy_index)
            .is_ok()
            && base_damage > 0;
        attack_profiles.push(can_attack.then(|| {
            FriendlyAttackProfile {
                base_damage,
                incoming_damage: super::combat_profile::planned_counter_damage(
                    &input.master_data,
                    &input.damage_chart,
                    &source.stats,
                    &enemy.stats,
                    distance,
                ),
                ready_turn: source
                    .available_turn
                    .saturating_add(attack_readiness_turns(distance, &source.stats)),
            }
        }));

        let enemy_base_damage = best_damage(
            &input.damage_chart,
            enemy.stats.unit_type,
            source.stats.unit_type,
        );
        let enemy_travel = attack_readiness_turns(distance, &enemy.stats);
        let friendly_travel = distance
            .saturating_sub(enemy.stats.max_range.max(1))
            .div_ceil(source.stats.max_movement.max(1));
        let enemy_contact_turn = enemy.available_turn.saturating_add(enemy_travel);
        let contact_turn = if can_advance_to_enemy {
            enemy_contact_turn.min(source.available_turn.saturating_add(friendly_travel))
        } else {
            enemy_contact_turn
        };
        enemy_attack_profiles.push((enemy_base_damage > 0).then_some(EnemyAttackProfile {
            base_damage: enemy_base_damage,
            contact_turn,
        }));
    }
    SimFriendly {
        stats: source.stats.clone(),
        hp: source.hp,
        initial_hp: source.hp,
        available_turn: source.available_turn,
        attacks_left: total_ammo.min(fuel_turns.max(1)),
        attack_profiles: attack_profiles.into(),
        enemy_attack_profiles: enemy_attack_profiles.into(),
        purchase: None,
    }
}

/// 現在手番に行動可能な戦闘役を一体へ集中したとき、確実に除去できる前線敵を選ぶ。
///
/// 通常の戦闘シミュレーションは脅威度も考慮して標的を選ぶが、物件レースの第一波は
/// 「どの兵種を何体買えば通路を開けられるか」を先に確定する必要がある。そこで実際の
/// 攻撃可能時刻・残HP・地形防御込みのdamageだけを合算し、撃破可能な敵がある場合に限り
/// 同一標的への集中を成立させる。
fn select_front_breakthrough_target(
    friendlies: &[SimFriendly],
    enemies: &[SimEnemy],
    turn: u32,
) -> Option<usize> {
    enemies
        .iter()
        .enumerate()
        .filter(|(_, enemy)| enemy.hp > 0 && turn >= enemy.source.available_turn)
        .filter_map(|(enemy_index, enemy)| {
            let concentrated_damage = friendlies
                .iter()
                .filter(|friendly| {
                    friendly.hp > 0 && friendly.attacks_left > 0 && turn >= friendly.available_turn
                })
                .filter_map(|friendly| {
                    let profile = friendly
                        .attack_profiles
                        .get(enemy_index)
                        .copied()
                        .flatten()?;
                    (turn >= profile.ready_turn).then_some(calculate_damage_formula(
                        profile.base_damage,
                        friendly.hp,
                        enemy.source.defense_bonus,
                        false,
                    ))
                })
                .fold(0_u32, u32::saturating_add);
            (concentrated_damage >= enemy.hp).then_some((
                (
                    !enemy.source.stats.can_capture,
                    enemy.hp,
                    enemy.source.available_turn,
                    enemy_index,
                ),
                enemy_index,
            ))
        })
        .min_by_key(|(key, _)| *key)
        .map(|(_, enemy_index)| enemy_index)
}

fn select_target(
    friendly: &SimFriendly,
    enemies: &[SimEnemy],
    turn: u32,
    interdiction_deadlines: &[(usize, u32)],
    target_first_attack_turns: &[Option<u32>],
) -> Option<(usize, FriendlyAttackProfile)> {
    enemies
        .iter()
        .enumerate()
        .filter(|(_, enemy)| enemy.hp > 0)
        .filter(|(_, enemy)| turn >= enemy.source.available_turn)
        .filter_map(|(index, enemy)| {
            let profile = friendly.attack_profiles[index]?;
            if turn < profile.ready_turn {
                return None;
            }
            let strategic_rank = if enemy.source.stats.can_capture {
                0
            } else if enemy.source.stats.max_cargo > 0 {
                1
            } else {
                2
            };
            let contract_deadline = interdiction_deadlines
                .iter()
                .find_map(|(target_index, deadline)| (*target_index == index).then_some(*deadline));
            let contract_rank = match contract_deadline {
                Some(_)
                    if target_first_attack_turns
                        .get(index)
                        .is_some_and(Option::is_none) =>
                {
                    0
                }
                Some(_) => 1,
                None => 2,
            };
            // 占領能力だけで標的順を固定すると、後方の砲兵を放置したまま前衛へ
            // 損耗を重ねる。自分へ返せる最大与ダメージを先に比較し、同程度なら
            // 勝利条件へ直結する占領・輸送能力と残HPで集中撃破先を決める。
            Some((
                (
                    contract_rank,
                    contract_deadline.unwrap_or(u32::MAX),
                    std::cmp::Reverse(profile.incoming_damage),
                    strategic_rank,
                    enemy.hp,
                    index,
                ),
                profile,
            ))
        })
        .min_by_key(|(key, _)| *key)
        .map(|((_, _, _, _, _, index), profile)| (index, profile))
}

fn best_damage(chart: &DamageChart, attacker: UnitType, defender: UnitType) -> u32 {
    chart.get_base_damage(attacker, defender).unwrap_or(0).max(
        chart
            .get_base_damage_secondary(attacker, defender)
            .unwrap_or(0),
    )
}

fn attack_readiness_turns(distance: u32, stats: &UnitStats) -> u32 {
    let travel = distance
        .saturating_sub(stats.max_range.max(1))
        .div_ceil(stats.max_movement.max(1));
    travel.saturating_add(u32::from(travel > 0 && stats.min_range > 1))
}

/// 生産可能施設から、現在手番と将来2手番分の離散的な生産slotを作る。
pub(crate) fn production_options(
    current_facilities: &[(GridPosition, Terrain)],
    future_facilities: &[(GridPosition, Terrain)],
    available_types: &[(UnitType, UnitStats)],
    master_data: &crate::resources::master_data::MasterDataRegistry,
    future_turns: u32,
    mut can_reach: impl FnMut(GridPosition, &UnitStats) -> bool,
) -> Vec<ProductionPlanOption> {
    let mut options = Vec::new();
    for build_turn in 0..future_turns {
        let facilities = if build_turn == 0 {
            current_facilities
        } else {
            future_facilities
        };
        for (facility, terrain) in facilities {
            for (unit_type, stats) in available_types {
                if stats.cost == 0
                    || !master_data.can_produce_unit(terrain.as_str(), *unit_type)
                    || !can_reach(*facility, stats)
                {
                    continue;
                }
                options.push(ProductionPlanOption {
                    purchase: PlannedPurchase {
                        facility: *facility,
                        unit_type: *unit_type,
                        build_turn,
                        cost: stats.cost,
                    },
                    stats: stats.clone(),
                    engageable_enemy_indices: Vec::new(),
                    capture_target: None,
                    capture_arrival_turn: None,
                    capture_completion_turn: None,
                });
            }
        }
    }
    options.sort_by_key(|option| {
        (
            option.purchase.build_turn,
            option.purchase.facility.y,
            option.purchase.facility.x,
            option.purchase.cost,
            option.purchase.unit_type.as_str(),
        )
    });
    options
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resources::master_data::UnitName;
    use crate::resources::{GridTopology, MovementType};

    fn stats(unit_type: UnitType, cost: u32, movement: u32) -> UnitStats {
        let master = MasterDataRegistry::load().unwrap();
        let weapon_stats = master
            .create_unit_stats(&UnitName(unit_type.as_str().to_owned()))
            .unwrap();
        UnitStats {
            unit_type,
            cost,
            max_movement: movement,
            movement_type: MovementType::Air,
            max_fuel: 99,
            max_ammo1: weapon_stats.max_ammo1,
            max_ammo2: weapon_stats.max_ammo2,
            min_range: weapon_stats.min_range,
            max_range: weapon_stats.max_range,
            ..UnitStats::mock()
        }
    }

    fn input() -> RollingPlanInput {
        let map = Map::new(12, 1, Terrain::Plains, GridTopology::Square);
        let mut chart = DamageChart::new();
        chart.insert_damage(UnitType::Bcopters, UnitType::Infantry, 40);
        chart.insert_damage(UnitType::Bomber, UnitType::Infantry, 100);
        chart.insert_damage(UnitType::Infantry, UnitType::Bcopters, 10);
        RollingPlanInput {
            map: Arc::new(map),
            master_data: Arc::new(MasterDataRegistry::load().unwrap()),
            damage_chart: Arc::new(chart),
            existing_units: Vec::new(),
            protected_units: Vec::new(),
            enemies: vec![EnemyPlanUnit {
                entity: Some(Entity::from_raw(7)),
                stats: UnitStats {
                    can_capture: true,
                    ..stats(UnitType::Infantry, 1_000, 3)
                },
                position: GridPosition { x: 8, y: 0 },
                hp: 100,
                defense_bonus: 0,
                available_turn: 0,
            }],
            production_options: vec![
                ProductionPlanOption {
                    purchase: PlannedPurchase {
                        facility: GridPosition { x: 0, y: 0 },
                        unit_type: UnitType::Bcopters,
                        build_turn: 0,
                        cost: 7_500,
                    },
                    stats: stats(UnitType::Bcopters, 7_500, 6),
                    engageable_enemy_indices: vec![0],
                    capture_target: None,
                    capture_arrival_turn: None,
                    capture_completion_turn: None,
                },
                ProductionPlanOption {
                    purchase: PlannedPurchase {
                        facility: GridPosition { x: 1, y: 0 },
                        unit_type: UnitType::Bomber,
                        build_turn: 0,
                        cost: 20_000,
                    },
                    stats: stats(UnitType::Bomber, 20_000, 6),
                    engageable_enemy_indices: vec![0],
                    capture_target: None,
                    capture_arrival_turn: None,
                    capture_completion_turn: None,
                },
            ],
            production_attack_ready_turns: Vec::new(),
            deadline_target_index: None,
            interdiction_deadlines: Vec::new(),
            current_funds: 30_000,
            income_per_turn: 0,
            hard_deadline: None,
            capture_completion_turn: None,
            required_capture_survivors: 0,
            delay_cost_per_turn: 5_000,
            search_beam_width: SEARCH_BEAM_WIDTH,
            prioritize_deadline_interdiction: false,
            exact_property_control: false,
        }
    }

    #[test]
    fn fast_expensive_package_can_beat_slow_cheap_package() {
        let plan = plan_force_package(&input()).unwrap();
        assert!(plan.feasible);
        assert_eq!(plan.purchases.len(), 1);
        assert_eq!(plan.purchases[0].unit_type, UnitType::Bomber);
        assert_eq!(plan.elimination_turn, Some(2));
    }

    #[test]
    fn current_combat_purchase_is_not_blocked_by_a_hypothetical_future_capturer() {
        let mut input = input();
        input.current_funds = 8_000;
        input.income_per_turn = 10_000;
        let escort_now = PlannedPurchase {
            facility: GridPosition { x: 0, y: 0 },
            unit_type: UnitType::Bcopters,
            build_turn: 0,
            cost: 7_500,
        };
        let second_escort = PlannedPurchase {
            facility: GridPosition { x: 1, y: 0 },
            unit_type: UnitType::Bcopters,
            build_turn: 1,
            cost: 7_500,
        };

        assert!(funding_suffices(&input, &[escort_now]));
        assert!(funding_suffices(&input, &[escort_now, second_escort]));
    }

    #[test]
    fn infeasible_plan_spends_an_idle_current_slot_before_late_purchases() {
        let mut input = input();
        input.current_funds = 8_500;
        Arc::make_mut(&mut input.damage_chart).insert_damage(
            UnitType::Infantry,
            UnitType::Infantry,
            55,
        );
        Arc::make_mut(&mut input.damage_chart).insert_damage(
            UnitType::Infantry,
            UnitType::Bcopters,
            10,
        );
        input.production_options.push(ProductionPlanOption {
            purchase: PlannedPurchase {
                facility: GridPosition { x: 1, y: 0 },
                unit_type: UnitType::Infantry,
                build_turn: 0,
                cost: 1_000,
            },
            stats: UnitStats {
                unit_type: UnitType::Infantry,
                cost: 1_000,
                can_capture: true,
                max_movement: 3,
                movement_type: MovementType::Infantry,
                max_fuel: 99,
                max_ammo1: 9,
                min_range: 1,
                max_range: 1,
                ..UnitStats::mock()
            },
            engageable_enemy_indices: vec![0],
            capture_target: None,
            capture_arrival_turn: None,
            capture_completion_turn: None,
        });
        let state = SearchState {
            option_indices: vec![0],
            used_slots: HashSet::from([ProductionSlot {
                facility: GridPosition { x: 0, y: 0 },
                build_turn: 0,
            }]),
            used_capture_targets: HashSet::new(),
            cost: 7_500,
        };
        let mut selected = simulate_state(&input, &state, DEFAULT_SEARCH_TURNS);
        selected.feasible = false;

        let catalog = SimulationCatalog::new(&input);
        let augmented =
            fill_best_effort_current_screen(&input, &catalog, selected, DEFAULT_SEARCH_TURNS);

        assert!(augmented.purchases.iter().any(|purchase| {
            purchase.build_turn == 0 && purchase.unit_type == UnitType::Infantry
        }));
        assert_eq!(
            augmented
                .purchases
                .iter()
                .filter(|purchase| purchase.build_turn == 0)
                .count(),
            2
        );
    }

    #[test]
    fn best_effort_prefers_lower_loss_when_enemy_progress_is_equal() {
        let mut high_loss = plan_force_package(&input()).unwrap();
        high_loss.feasible = false;
        high_loss.expected_loss = 20_000;
        high_loss.first_attack_turn = Some(1);
        high_loss.target_forecasts[0].remaining_hp = 20;
        let mut low_loss = high_loss.clone();
        low_loss.expected_loss = 2_000;
        low_loss.first_attack_turn = Some(2);
        let mut selected = None;

        update_best_effort(&mut selected, &high_loss);
        update_best_effort(&mut selected, &low_loss);

        assert_eq!(selected.unwrap().expected_loss, 2_000);
    }

    /// 占領期限がある場合、兵種の移動力ではなくシミュレーション上の初撃時刻を優先する。
    #[test]
    fn deadline_fallback_prefers_earlier_simulated_interdiction() {
        let mut early = plan_force_package(&input()).unwrap();
        early.feasible = false;
        early.first_attack_turn = Some(1);
        early.target_forecasts[0].remaining_hp = 60;
        early.expected_loss = 4_000;
        early.production_cost = 8_000;

        let mut late = early.clone();
        late.first_attack_turn = Some(2);
        late.target_forecasts[0].remaining_hp = 0;
        late.expected_loss = 0;
        late.production_cost = 1_000;

        let mut selected = None;
        update_best_effort_interdiction(&mut selected, &late, None);
        update_best_effort_interdiction(&mut selected, &early, None);

        assert_eq!(selected.unwrap().first_attack_turn, Some(1));
    }

    /// 同じ初撃時刻なら、敵残HPをより削れる案（高い軍事成果）を優先する。
    #[test]
    fn deadline_fallback_prefers_lower_remaining_hp_after_equal_attack_turn() {
        let mut cheap = plan_force_package(&input()).unwrap();
        cheap.feasible = false;
        cheap.first_attack_turn = Some(1);
        cheap.target_forecasts[0].remaining_hp = 60;
        cheap.production_cost = 1_000;

        let mut expensive = cheap.clone();
        expensive.target_forecasts[0].remaining_hp = 20;
        expensive.production_cost = 20_000;

        let mut selected = None;
        update_best_effort_interdiction(&mut selected, &expensive, None);
        update_best_effort_interdiction(&mut selected, &cheap, None);

        assert_eq!(selected.unwrap().production_cost, 20_000);
    }

    /// 期限前の初撃が成立した案同士でも、敵残HPをより削れる案を優先する。
    #[test]
    fn sufficient_interdiction_prefers_higher_enemy_hp_reduction() {
        let mut early_expensive = plan_force_package(&input()).unwrap();
        early_expensive.feasible = false;
        early_expensive.first_attack_turn = Some(1);
        early_expensive.target_forecasts[0].remaining_hp = 20;
        early_expensive.production_cost = 20_000;

        let mut timely_cheap = early_expensive.clone();
        timely_cheap.first_attack_turn = Some(2);
        timely_cheap.target_forecasts[0].remaining_hp = 60;
        timely_cheap.production_cost = 4_000;

        let mut selected = None;
        update_best_sufficient_interdiction(&mut selected, &early_expensive, None);
        update_best_sufficient_interdiction(&mut selected, &timely_cheap, None);

        assert_eq!(selected.unwrap().production_cost, 20_000);
    }

    /// 同じ敵への一撃を複数物件の妨害として二重計上しない。
    #[test]
    fn interdiction_requires_a_legal_attack_for_every_distinct_contract() {
        let mut plan = plan_force_package(&input()).unwrap();
        plan.target_first_attack_turns = vec![Some(1), None];
        let deadlines = vec![(0, 2), (1, 3)];

        assert_eq!(plan.missed_interdiction_contracts(&deadlines), 1);
        assert!(!plan.interdiction_contracts_satisfied(&deadlines));

        plan.target_first_attack_turns[1] = Some(3);
        assert_eq!(plan.missed_interdiction_contracts(&deadlines), 0);
        assert!(plan.interdiction_contracts_satisfied(&deadlines));
    }

    #[test]
    fn feasible_selection_prefers_a_plan_that_keeps_the_expected_overmatch_margin() {
        let mut without_margin = plan_force_package(&input()).unwrap();
        without_margin.production_cost = 7_500;
        without_margin.expected_loss = 0;
        without_margin.overmatch_ready = false;
        let mut with_margin = without_margin.clone();
        with_margin.production_cost = 12_000;
        with_margin.surviving_combat_value = 6_000;
        with_margin.required_overmatch_value = 2_000;
        with_margin.overmatch_ready = true;
        let mut selected = None;

        update_best_feasible(&mut selected, &without_margin, 0);
        update_best_feasible(&mut selected, &with_margin, 0);

        assert!(selected.expect("a feasible candidate").overmatch_ready);
    }

    #[test]
    fn dominated_candidate_in_the_same_production_slot_is_pruned() {
        let mut input = input();
        input.production_options.truncate(1);
        let dominated = ProductionPlanOption {
            purchase: PlannedPurchase {
                facility: GridPosition { x: 0, y: 0 },
                unit_type: UnitType::Bcopters,
                build_turn: 0,
                cost: 8_000,
            },
            stats: input.production_options[0].stats.clone(),
            engageable_enemy_indices: vec![0],
            capture_target: None,
            capture_arrival_turn: None,
            capture_completion_turn: None,
        };
        input.production_options.push(dominated);

        let plan = plan_force_package(&input).expect("a plan");

        assert_eq!(plan.candidates_pruned, 1);
        assert!(plan.purchases.iter().all(|purchase| purchase.cost != 8_000));
    }

    #[test]
    fn production_unit_cannot_attack_before_travel_finishes() {
        let mut input = input();
        input.production_options.truncate(1);
        let plan = plan_force_package(&input).unwrap();
        assert!(plan.first_attack_turn.is_some_and(|turn| turn >= 3));
    }

    #[test]
    fn occupation_uses_live_campaign_eta_instead_of_a_fixed_delay() {
        let mut input = input();
        input.capture_completion_turn = Some(7);
        input.required_capture_survivors = 1;
        input.protected_units.push(FriendlyPlanUnit {
            stats: UnitStats {
                unit_type: UnitType::Infantry,
                can_capture: true,
                ..UnitStats::mock()
            },
            position: GridPosition { x: 8, y: 0 },
            hp: 100,
            available_turn: 0,
            engageable_enemy_indices: Vec::new(),
        });

        let plan = plan_force_package(&input).unwrap();

        assert!(plan.elimination_turn.is_some_and(|turn| turn <= 7));
        assert_eq!(plan.occupation_turn, Some(7));
    }

    #[test]
    fn combat_capturer_counts_once_as_a_surviving_occupation_unit() {
        let mut input = input();
        input.production_options.clear();
        input.existing_units = vec![FriendlyPlanUnit {
            stats: UnitStats {
                unit_type: UnitType::Infantry,
                cost: 1_000,
                can_capture: true,
                max_ammo1: 9,
                max_fuel: 99,
                max_movement: 3,
                movement_type: MovementType::Infantry,
                min_range: 1,
                max_range: 1,
                ..UnitStats::mock()
            },
            position: GridPosition { x: 8, y: 0 },
            hp: 100,
            available_turn: 0,
            engageable_enemy_indices: vec![0],
        }];
        Arc::make_mut(&mut input.damage_chart).insert_damage(
            UnitType::Infantry,
            UnitType::Infantry,
            100,
        );
        input.capture_completion_turn = Some(2);
        input.required_capture_survivors = 1;

        let plan = plan_force_package(&input).expect("歩兵だけで撃破と占領を継続できる");

        assert!(plan.feasible);
        assert_eq!(plan.protected_unit_count, 0);
        assert_eq!(plan.protected_survivor_count, 1);
        assert_eq!(plan.occupation_turn, Some(2));
    }

    #[test]
    fn losing_one_of_two_required_capturers_makes_the_plan_infeasible() {
        let mut input = input();
        input.production_options.clear();
        Arc::make_mut(&mut input.damage_chart).insert_damage(
            UnitType::Infantry,
            UnitType::Infantry,
            100,
        );
        input.existing_units.push(FriendlyPlanUnit {
            stats: stats(UnitType::Bomber, 20_000, 6),
            position: GridPosition { x: 6, y: 0 },
            hp: 100,
            // 敵を最終的には倒せるが、占領兵が先に損耗する状況を作る。
            available_turn: 2,
            engageable_enemy_indices: vec![0],
        });
        let capturer = FriendlyPlanUnit {
            stats: UnitStats {
                unit_type: UnitType::Infantry,
                can_capture: true,
                ..UnitStats::mock()
            },
            position: GridPosition { x: 7, y: 0 },
            hp: 100,
            available_turn: 0,
            engageable_enemy_indices: Vec::new(),
        };
        input.protected_units = vec![capturer.clone(), capturer];
        input.capture_completion_turn = Some(3);
        input.required_capture_survivors = 2;

        let plan = plan_force_package(&input).unwrap();

        assert_eq!(plan.elimination_turn, Some(3));
        assert_eq!(plan.protected_unit_count, 2);
        assert_eq!(plan.protected_survivor_count, 0);
        assert_eq!(plan.required_capture_survivor_count, 2);
        assert!(!plan.feasible);
        assert_eq!(plan.occupation_turn, None);
    }

    #[test]
    fn mixed_package_is_selected_when_one_type_cannot_clear_all_targets() {
        let mut input = input();
        input.enemies[0].stats.max_ammo1 = 0;
        input.enemies[0].stats.max_ammo2 = 0;
        Arc::make_mut(&mut input.damage_chart).insert_damage(
            UnitType::Infantry,
            UnitType::Bcopters,
            0,
        );
        Arc::make_mut(&mut input.damage_chart).insert_secondary_damage(
            UnitType::Infantry,
            UnitType::Bcopters,
            0,
        );
        Arc::make_mut(&mut input.damage_chart).insert_damage(
            UnitType::Bcopters,
            UnitType::Infantry,
            0,
        );
        Arc::make_mut(&mut input.damage_chart).insert_damage(
            UnitType::Bomber,
            UnitType::Fighter,
            0,
        );
        Arc::make_mut(&mut input.damage_chart).insert_secondary_damage(
            UnitType::Bcopters,
            UnitType::Fighter,
            70,
        );
        input.enemies.push(EnemyPlanUnit {
            entity: Some(Entity::from_raw(8)),
            stats: UnitStats {
                max_ammo1: 0,
                max_ammo2: 0,
                ..stats(UnitType::Fighter, 9_000, 9)
            },
            position: GridPosition { x: 8, y: 0 },
            hp: 100,
            defense_bonus: 0,
            available_turn: 0,
        });
        for option in &mut input.production_options {
            option.engageable_enemy_indices = vec![0, 1];
        }
        let plan = plan_force_package(&input).unwrap();
        assert!(plan.feasible);
        assert!(
            plan.purchases
                .iter()
                .any(|purchase| purchase.unit_type == UnitType::Bomber)
        );
        assert!(
            plan.purchases
                .iter()
                .any(|purchase| purchase.unit_type == UnitType::Bcopters)
        );
    }

    #[test]
    fn occupied_facility_is_available_only_to_future_production_slots() {
        let registry = crate::resources::master_data::MasterDataRegistry::load().unwrap();
        let airport = GridPosition { x: 2, y: 0 };
        let options = production_options(
            &[],
            &[(airport, Terrain::Airport)],
            &[(UnitType::Bcopters, stats(UnitType::Bcopters, 7_500, 6))],
            &registry,
            3,
            |_, _| true,
        );

        assert!(!options.is_empty());
        assert!(options.iter().all(|option| option.purchase.build_turn > 0));
    }

    #[test]
    fn fixed_formation_uses_a_new_free_facility_without_changing_composition() {
        let mut input = input();
        let old_facility = GridPosition { x: 0, y: 0 };
        let new_facility = GridPosition { x: 2, y: 0 };
        input.production_options = vec![
            ProductionPlanOption {
                purchase: PlannedPurchase {
                    facility: new_facility,
                    unit_type: UnitType::Bcopters,
                    build_turn: 0,
                    cost: 7_500,
                },
                stats: stats(UnitType::Bcopters, 7_500, 6),
                engageable_enemy_indices: vec![0],
                capture_target: None,
                capture_arrival_turn: None,
                capture_completion_turn: None,
            },
            ProductionPlanOption {
                purchase: PlannedPurchase {
                    facility: old_facility,
                    unit_type: UnitType::Bcopters,
                    build_turn: 1,
                    cost: 7_500,
                },
                stats: stats(UnitType::Bcopters, 7_500, 6),
                engageable_enemy_indices: vec![0],
                capture_target: None,
                capture_arrival_turn: None,
                capture_completion_turn: None,
            },
        ];

        let plan = evaluate_fixed_package(
            &input,
            &[PlannedPurchase {
                facility: old_facility,
                unit_type: UnitType::Bcopters,
                build_turn: 1,
                cost: 7_500,
            }],
        )
        .unwrap();

        assert_eq!(plan.purchases.len(), 1);
        assert_eq!(plan.purchases[0].facility, new_facility);
        assert_eq!(plan.purchases[0].build_turn, 0);
        assert_eq!(plan.purchases[0].unit_type, UnitType::Bcopters);
    }

    #[test]
    fn schedule_compaction_does_not_steal_an_unprocessed_original_slot() {
        let mut input = input();
        let facility = GridPosition { x: 0, y: 0 };
        input.production_options = vec![
            ProductionPlanOption {
                purchase: PlannedPurchase {
                    facility,
                    unit_type: UnitType::Bcopters,
                    build_turn: 1,
                    cost: 7_500,
                },
                stats: stats(UnitType::Bcopters, 7_500, 6),
                engageable_enemy_indices: vec![0],
                capture_target: None,
                capture_arrival_turn: None,
                capture_completion_turn: None,
            },
            ProductionPlanOption {
                purchase: PlannedPurchase {
                    facility,
                    unit_type: UnitType::Bomber,
                    build_turn: 1,
                    cost: 20_000,
                },
                stats: stats(UnitType::Bomber, 20_000, 6),
                engageable_enemy_indices: vec![0],
                capture_target: None,
                capture_arrival_turn: None,
                capture_completion_turn: None,
            },
        ];
        let original = vec![
            PlannedPurchase {
                facility,
                unit_type: UnitType::Bcopters,
                build_turn: 0,
                cost: 7_500,
            },
            PlannedPurchase {
                facility,
                unit_type: UnitType::Bomber,
                build_turn: 1,
                cost: 20_000,
            },
        ];

        let shifted = left_shift_purchases(&input, &original);
        let slots = shifted
            .iter()
            .map(|purchase| (purchase.facility, purchase.build_turn))
            .collect::<HashSet<_>>();

        assert_eq!(slots.len(), shifted.len());
        assert!(matches!(
            evaluate_fixed_package(&input, &original),
            Err(FixedPackageError::ProductionSlotUnavailable)
        ));
    }

    #[test]
    fn schedule_compaction_preserves_funding_for_later_original_purchases() {
        let mut input = input();
        input.current_funds = 7_500;
        input.income_per_turn = 10_000;
        let facility = GridPosition { x: 0, y: 0 };
        input.production_options = vec![
            ProductionPlanOption {
                purchase: PlannedPurchase {
                    facility,
                    unit_type: UnitType::Bcopters,
                    build_turn: 0,
                    cost: 7_500,
                },
                stats: stats(UnitType::Bcopters, 7_500, 6),
                engageable_enemy_indices: vec![0],
                capture_target: None,
                capture_arrival_turn: None,
                capture_completion_turn: None,
            },
            ProductionPlanOption {
                purchase: PlannedPurchase {
                    facility,
                    unit_type: UnitType::Bcopters,
                    build_turn: 1,
                    cost: 7_500,
                },
                stats: stats(UnitType::Bcopters, 7_500, 6),
                engageable_enemy_indices: vec![0],
                capture_target: None,
                capture_arrival_turn: None,
                capture_completion_turn: None,
            },
            ProductionPlanOption {
                purchase: PlannedPurchase {
                    facility,
                    unit_type: UnitType::Bomber,
                    build_turn: 2,
                    cost: 20_000,
                },
                stats: stats(UnitType::Bomber, 20_000, 6),
                engageable_enemy_indices: vec![0],
                capture_target: None,
                capture_arrival_turn: None,
                capture_completion_turn: None,
            },
        ];
        let original = vec![
            PlannedPurchase {
                facility,
                unit_type: UnitType::Bcopters,
                build_turn: 1,
                cost: 7_500,
            },
            PlannedPurchase {
                facility,
                unit_type: UnitType::Bomber,
                build_turn: 2,
                cost: 20_000,
            },
        ];

        let shifted = left_shift_purchases(&input, &original);

        assert_eq!(shifted[0].build_turn, 0);
        assert_eq!(shifted[1].build_turn, 2);
        assert!(funding_suffices(&input, &shifted));
        assert!(!matches!(
            evaluate_fixed_package(&input, &original),
            Err(FixedPackageError::FundingUnavailable)
        ));
    }

    #[test]
    fn future_enemy_must_be_removed_before_plan_is_feasible() {
        let mut input = input();
        input.enemies.push(EnemyPlanUnit {
            entity: None,
            stats: UnitStats {
                unit_type: UnitType::Infantry,
                cost: 1_000,
                can_capture: true,
                ..UnitStats::mock()
            },
            position: GridPosition { x: 8, y: 0 },
            hp: 100,
            defense_bonus: 0,
            available_turn: 5,
        });
        for option in &mut input.production_options {
            option.engageable_enemy_indices = vec![0, 1];
        }

        let plan = plan_force_package(&input).unwrap();

        assert!(plan.feasible);
        assert!(plan.elimination_turn.is_some_and(|turn| turn >= 5));
        assert_eq!(plan.target_forecasts.len(), 2);
        assert_eq!(
            plan.turn_forecasts
                .iter()
                .find(|forecast| forecast.turn == 5)
                .map(|forecast| forecast.enemy_arrival_hp),
            Some(100)
        );
    }

    #[test]
    fn search_depth_comes_from_facility_turns_not_a_fixed_unit_cap() {
        let mut input = input();
        input.enemies[0].stats.unit_type = UnitType::TransportHelicopter;
        input.enemies[0].stats.max_ammo1 = 0;
        input.enemies[0].stats.max_ammo2 = 0;
        Arc::make_mut(&mut input.damage_chart).insert_damage(
            UnitType::Bcopters,
            UnitType::TransportHelicopter,
            100,
        );
        input.production_options.truncate(1);
        input.production_options[0].stats.max_ammo1 = 1;
        input.production_options[0].stats.max_ammo2 = 0;
        input.production_options[0].engageable_enemy_indices = (0..6).collect();
        for build_turn in 1..6 {
            let mut option = input.production_options[0].clone();
            option.purchase.build_turn = build_turn;
            input.production_options.push(option);
        }
        for raw in 8..13 {
            let mut enemy = input.enemies[0].clone();
            enemy.entity = Some(Entity::from_raw(raw));
            input.enemies.push(enemy);
        }
        input.current_funds = 45_000;

        let plan = plan_force_package(&input).unwrap();

        assert!(plan.feasible, "{plan:#?}");
        assert_eq!(plan.purchases.len(), 6);
    }

    #[test]
    fn property_front_selects_two_unit_breakthrough_and_three_capture_lanes() {
        let mut input = input();
        input.production_options.clear();
        input.production_attack_ready_turns.clear();
        input.current_funds = 11_400;
        input.income_per_turn = 0;
        input.hard_deadline = Some(6);
        input.required_capture_survivors = 5;
        input.exact_property_control = true;
        input.prioritize_deadline_interdiction = false;
        Arc::make_mut(&mut input.damage_chart).insert_damage(
            UnitType::Recon,
            UnitType::Infantry,
            60,
        );

        for x in 0..5 {
            let facility = GridPosition { x, y: 0 };
            input.production_options.push(ProductionPlanOption {
                purchase: PlannedPurchase {
                    facility,
                    unit_type: UnitType::Recon,
                    build_turn: 0,
                    cost: 4_200,
                },
                stats: stats(UnitType::Recon, 4_200, 6),
                engageable_enemy_indices: vec![0],
                capture_target: None,
                capture_arrival_turn: None,
                capture_completion_turn: None,
            });
            input.production_attack_ready_turns.push(vec![Some(1)]);

            input.production_options.push(ProductionPlanOption {
                purchase: PlannedPurchase {
                    facility,
                    unit_type: UnitType::Infantry,
                    build_turn: 0,
                    cost: 1_000,
                },
                stats: UnitStats {
                    can_capture: true,
                    movement_type: MovementType::Infantry,
                    ..stats(UnitType::Infantry, 1_000, 3)
                },
                engageable_enemy_indices: Vec::new(),
                capture_target: Some(GridPosition { x: x + 5, y: 0 }),
                capture_arrival_turn: Some(2),
                capture_completion_turn: Some(4),
            });
            input.production_attack_ready_turns.push(vec![None]);
        }

        let plan = plan_force_package(&input).expect("突破と占領を合同で満たす生産案");
        let recon_count = plan
            .purchases
            .iter()
            .filter(|purchase| purchase.unit_type == UnitType::Recon)
            .count();

        assert_eq!(plan.front_breakthrough_turn, Some(1), "{plan:#?}");
        assert_eq!(recon_count, 2, "{plan:#?}");
        assert_eq!(plan.capture_purchases.len(), 3, "{plan:#?}");
        assert_eq!(plan.deadline_capture_survivor_count, 3, "{plan:#?}");
        assert!(
            plan.combat_purchases
                .iter()
                .all(|assignment| assignment.target == Entity::from_raw(7))
        );
    }
}
