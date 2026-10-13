//! 島作戦の残敵排除を、金額ではなく実行可能な行動列として比較する純粋計画器。
//!
//! このモジュールは盤面を直接読まず、呼び出し側が作ったsnapshotだけを受け取る。
//! 既存戦力と複数ターンの生産候補を混ぜ、悲観側ダメージで残敵を排除できる
//! パッケージだけを費用・完了ターン・損耗で比較する。

use crate::collections::{HashMap, HashSet};
use crate::components::{GridPosition, UnitStats};
use crate::resources::master_data::MasterDataRegistry;
use crate::resources::{DamageChart, Map, MovementType, Terrain, UnitType};
use crate::systems::combat::calculate_damage_formula;
use bevy_ecs::prelude::Entity;
use std::collections::BTreeSet;
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
    /// 既に実Entityとなった占領担当を生存させる期限。同じEntityを別の
    /// `protected_units`へ複製せず、この一体の攻撃・HP・生存を共有する。
    pub protection_completion_turn: Option<u32>,
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
    /// Someなら、指定物件の戦闘・占領を一体の時系列契約として引き受ける。
    /// 生産時点で戦闘役と占領専任役へ分断せず、物件を塞ぐ敵が残る間は攻撃し、
    /// 排除後に占領行動へ移る。
    pub capture_target: Option<GridPosition>,
    /// 占領役候補が指定物件へ到着する実経路turn。
    pub capture_arrival_turn: Option<u32>,
    /// 占領役候補が指定物件を占領完了する実経路turn。
    pub capture_completion_turn: Option<u32>,
    /// 対象物件の現在耐久値。戦闘でHPが減った場合の占領回数も実値から再計算する。
    pub capture_durability: Option<u32>,
    /// 対象物件への進入を妨げる観測済み敵。排除前は同じunitが攻撃役を兼ねる。
    pub capture_blocking_enemy_index: Option<usize>,
}

#[derive(Debug, Clone)]
pub(crate) struct RollingPlanInput {
    pub map: Arc<Map>,
    pub master_data: Arc<MasterDataRegistry>,
    pub damage_chart: Arc<DamageChart>,
    /// 初手専用の役割下限を後続手番へ漏らさないための、実際のゲーム手番。
    pub current_turn: u32,
    /// ターン入口で固定した戦略パイプラインから渡す初手方針。
    pub opening_policy: OpeningProductionPolicy,
    pub existing_units: Vec<FriendlyPlanUnit>,
    /// 戦闘部隊とは別に、占領完了まで生存させる必要がある実在の占領兵。
    pub protected_units: Vec<FriendlyPlanUnit>,
    pub enemies: Vec<EnemyPlanUnit>,
    pub production_options: Vec<ProductionPlanOption>,
    /// 生産候補ごと・敵ごとの、実地形と射程を通した最早攻撃turn。
    /// 空なら従来の格子距離見積りへフォールバックする（単体テスト用）。
    pub production_attack_projections: Vec<Vec<Option<ProductionAttackProjection>>>,
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
    /// 次の自軍手番に生産施設上から攻撃して、その施設を空けない購入数。
    /// 未成立作戦では、火力だけでなく失う次回生産slotも比較する。
    pub blocked_next_turn_production_slots: usize,
    /// 今手番の直接部隊が実シミュレーションで担当した、敵別のユニークな射点数。
    /// 同じ敵・同じ射点へ向かう複数unitは一つと数え、渋滞の比較に使う。
    pub direct_fire_lane_count: usize,
    /// 生存している敵直接部隊へ、相性と接敵時刻を満たす直接担当を一体ずつ
    /// 割り当てられなかった数。砲撃で直接戦線を置換しないために使う。
    #[allow(dead_code)]
    pub direct_front_shortfall: usize,
    /// 敵直接部隊ごとに、直接部隊だけで担当割当てしても削り切れないHP。
    /// 添字は `target_forecasts` と一致し、間接火力で直接戦線の穴を隠さない比較に使う。
    #[allow(dead_code)]
    pub direct_enemy_remaining_hp: Vec<u32>,
    /// 直接部隊が各敵へ与えた実ダメージ。敵種を問わず、砲撃の標的重複判定に使う。
    pub direct_damage_by_target: Vec<u32>,
    /// 既存の間接部隊が各敵へ与えた実ダメージ。今回の砲の標的重複を判定する。
    pub existing_indirect_damage_by_target: Vec<u32>,
    /// 今回生産した間接部隊が各敵へ与えた実ダメージ。上の既存火力との差分で限界効用を測る。
    pub produced_indirect_damage_by_target: Vec<u32>,
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

    fn remaining_hp(&self) -> u32 {
        self.target_forecasts
            .iter()
            .map(|target| target.remaining_hp)
            .sum()
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
    /// 攻撃できる敵indexを、標的選択の静的な優先順（被反撃damageの大きい順、
    /// 占領・輸送・その他の順、index順）に並べたもの。
    target_order: Arc<[usize]>,
    /// 生産候補だけが持つ購入identity。既存戦力はNone。
    purchase: Option<PlannedPurchase>,
    /// 生産時に割り当てた物件契約。戦闘と占領を別unitとして二重計上しない。
    capture_contract: Option<SimCaptureContract>,
    protection_completion_turn: Option<u32>,
    survived_protection_completion: Option<bool>,
}

#[derive(Debug, Clone, Copy)]
struct FriendlyAttackProfile {
    base_damage: u32,
    incoming_damage: u32,
    ready_turn: u32,
    firing_position: Option<GridPosition>,
    requires_movement: bool,
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

impl SearchState {
    /// 同じ生産枠で購入できる子状態を作る。占領対象の適格条件や登録はモデル側が
    /// 判断し、この共通処理は累積費用・生産turnまでの資金と購入列だけを扱う。
    fn with_affordable_purchase(
        &self,
        input: &RollingPlanInput,
        option_index: usize,
        slot: ProductionSlot,
    ) -> Option<Self> {
        let option = &input.production_options[option_index];
        let next_cost = self.cost.saturating_add(option.purchase.cost);
        let available_by_build_turn = input.current_funds.saturating_add(
            input
                .income_per_turn
                .saturating_mul(option.purchase.build_turn),
        );
        if next_cost > available_by_build_turn {
            return None;
        }
        let mut child = self.clone();
        child.option_indices.push(option_index);
        child.used_slots.insert(slot);
        child.cost = next_cost;
        Some(child)
    }
}

/// 施設・生産turnごとに候補をまとめ、同じ枠の兵種indexは入力順で保持する。
/// 支配関係による候補除外と枠の処理順の確定は呼出し側に残す。
fn group_production_options_by_slot(
    input: &RollingPlanInput,
) -> HashMap<ProductionSlot, Vec<usize>> {
    let mut options_by_slot: HashMap<ProductionSlot, Vec<usize>> = HashMap::default();
    for (index, option) in input.production_options.iter().enumerate() {
        options_by_slot
            .entry(option.purchase.into())
            .or_default()
            .push(index);
    }
    options_by_slot
}

/// 1回のRolling Plan入力で不変な交戦関係を全beam候補から共有する。
struct SimulationCatalog {
    existing_units: Vec<SimFriendly>,
    protected_units: Vec<SimFriendly>,
    production_units: Vec<SimFriendly>,
    enemies: Vec<SimEnemy>,
    /// 敵indexごとに、その敵が攻撃できる既存unitの添字を基礎damageの大きい順に並べたもの。
    existing_by_enemy_damage: Vec<Vec<usize>>,
}

impl SimulationCatalog {
    fn new(input: &RollingPlanInput) -> Self {
        // 小規模マップのV4は extended 側の実装を使う。
        if crate::ai::code_path::is_extended_at("v4/rolling_plan.rs::new") {
            return Self::new_extended(input);
        }
        // 同じ施設・兵種の生産案は生産turnだけが違う。交戦条件の表は1回だけ作り、
        // 行動可能turnだけを生産案ごとに当てはめる。
        let mut templates: HashMap<(GridPosition, UnitType), EngagementTemplate> =
            HashMap::default();
        Self::new_with_production_initializer(input, |_, option| {
            let key = (option.purchase.facility, option.purchase.unit_type);
            let available_turn = option.purchase.build_turn.saturating_add(1);
            match templates.get(&key) {
                Some(template) if template.matches(option) => {
                    template.instantiate(available_turn, 100, true, None)
                }
                _ => {
                    let template = EngagementTemplate::build(
                        input,
                        &option.stats,
                        option.purchase.facility,
                        &option.engageable_enemy_indices,
                    );
                    let simulated = template.instantiate(available_turn, 100, true, None);
                    templates.insert(key, template);
                    simulated
                }
            }
        })
    }

    /// 通常モデルを土台に、既存・保護・敵の初期化と入力順を共有する。
    /// 生産unitにだけモデル固有の初期化を適用し、購入契約や実経路projectionは
    /// この共通構築器へ持ち込まない。indexはprojection行との対応に使う。
    fn new_with_production_initializer(
        input: &RollingPlanInput,
        mut initialize_production: impl FnMut(usize, &ProductionPlanOption) -> SimFriendly,
    ) -> Self {
        let existing_units = input
            .existing_units
            .iter()
            .map(|source| sim_friendly(input, source, true))
            .collect::<Vec<_>>();
        let protected_units = input
            .protected_units
            .iter()
            .map(|source| sim_friendly(input, source, false))
            .collect();
        let production_units = input
            .production_options
            .iter()
            .enumerate()
            .map(|(index, option)| initialize_production(index, option))
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
            .collect::<Vec<_>>();
        let existing_by_enemy_damage = existing_units_by_enemy_damage(&existing_units, &enemies);
        Self {
            existing_units,
            protected_units,
            production_units,
            enemies,
            existing_by_enemy_damage,
        }
    }
}

fn existing_units_by_enemy_damage(
    existing_units: &[SimFriendly],
    enemies: &[SimEnemy],
) -> Vec<Vec<usize>> {
    (0..enemies.len())
        .map(|enemy_index| {
            let mut order = existing_units
                .iter()
                .enumerate()
                .filter_map(|(index, unit)| {
                    unit.enemy_attack_profiles[enemy_index]
                        .map(|profile| (std::cmp::Reverse(profile.base_damage), index))
                })
                .collect::<Vec<_>>();
            order.sort_unstable();
            order.into_iter().map(|(_, index)| index).collect()
        })
        .collect()
}

/// 現在観測した敵を排除できる混成パッケージを探索する。
pub(crate) fn plan_force_package(input: &RollingPlanInput) -> Option<ForcePackagePlan> {
    // 小規模マップのV4は extended 側の実装を使う。
    if crate::ai::code_path::is_extended_at("v4/rolling_plan.rs::plan_force_package") {
        return extended::plan_force_package(input);
    }
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
    let root = SearchState::default();
    let mut frontier = vec![(root.clone(), CombatRun::start(&catalog, &root))];
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
    let options_by_slot = group_production_options_by_slot(input);
    let mut slots = options_by_slot.into_iter().collect::<Vec<_>>();
    slots.sort_unstable_by_key(|(slot, _)| (slot.build_turn, slot.facility.y, slot.facility.x));

    for (slot, option_indices) in slots {
        // この枠の生産unitは生産turnの翌turnから交戦する。それまでの交戦は親案と同じなので、
        // 親案を生産turnまで進めた途中状態から子案を再開する。
        let resume_turn = slot.build_turn.min(search_turns);
        let mut next = Vec::new();
        for (state, run) in &mut frontier {
            run.advance_to(input, resume_turn);
            // この枠を使わない案も残す。高額兵種のための現金予約や、将来の別施設を
            // 選ぶ案を、安い現在購入で強制的に上書きしないためである。
            next.push((state.clone(), run.clone()));
            for option_index in &option_indices {
                if let Some(child) = state.with_affordable_purchase(input, *option_index, slot) {
                    next.push((child, run.with_purchase(*option_index)));
                }
            }
        }

        // 同じbeam層の各状態はsnapshotだけを読む独立計算である。nativeでは並列評価し、
        // 結果は入力順へ戻してから従来どおり最良案を逐次更新するため、同点時の選択は不変。
        let evaluated = crate::ai::deterministic_parallel::map_ordered(next, |(state, run)| {
            let mut finished = run.clone();
            finished.advance_to(input, search_turns);
            let plan = finished.into_plan(input, &state);
            (state, run, plan)
        });
        let mut evaluated_next = Vec::with_capacity(evaluated.len());
        for (state, run, mut plan) in evaluated {
            considered = considered.saturating_add(1);
            plan.candidates_considered = considered;
            if plan.feasible {
                update_best_feasible(&mut best_feasible, &plan, input.delay_cost_per_turn);
                if input.hard_deadline.is_some_and(|deadline| {
                    plan.elimination_turn.is_some_and(|turn| turn <= deadline)
                }) {
                    update_best_feasible(
                        &mut best_within_deadline,
                        &plan,
                        input.delay_cost_per_turn,
                    );
                }
            }
            update_best_effort(&mut best_effort, &plan);
            evaluated_next.push((state, run, plan));
        }
        let beam_width = input.search_beam_width.clamp(1, SEARCH_BEAM_WIDTH);
        if evaluated_next.len() > beam_width {
            truncated = true;
            evaluated_next.sort_by_key(|(_, _, plan)| {
                (
                    plan.remaining_hp(),
                    plan.expected_loss,
                    plan.completion_for_ordering(),
                    plan.production_cost,
                )
            });
            evaluated_next.truncate(beam_width);
        }
        frontier = evaluated_next
            .into_iter()
            .map(|(state, run, _)| (state, run))
            .collect();
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
    selected.search_truncated = truncated;
    Some(selected)
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

    // 即時screenを優先した結果、将来収入で払えなくなった最遠の予定だけを外す。
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
        used_capture_targets: HashSet::default(),
        option_indices: indexed,
        used_slots: purchases
            .iter()
            .copied()
            .map(ProductionSlot::from)
            .collect(),
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
        candidate.utility_cost(delay_cost_per_turn),
        candidate.completion_for_ordering(),
        candidate.production_cost,
    );
    if best.as_ref().is_none_or(|current| {
        candidate_key
            < (
                current.utility_cost(delay_cost_per_turn),
                current.completion_for_ordering(),
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
    let mut used_slots = HashSet::default();
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
    // 小規模マップのV4は extended 側の実装を使う。
    if crate::ai::code_path::is_extended_at("v4/rolling_plan.rs::simulate_state_with_catalog") {
        return extended::simulate_state_with_catalog(input, catalog, state, search_turns);
    }
    let mut run = CombatRun::start(catalog, state);
    run.advance_to(input, search_turns);
    run.into_plan(input, state)
}

/// 1購入案の交戦シミュレーションの途中状態。
///
/// 交戦条件（兵種・射程・接触turn）は探索中不変なので `SimulationCatalog` を参照し、
/// 1候補ごとに変わるHP・残弾・撃破turnと経過turnだけを持つ。生産unitは生産turnの
/// 翌turnまで交戦しないため、親案を生産turnまで進めた状態から子案を再開できる。
#[derive(Clone)]
struct CombatRun<'c> {
    catalog: &'c SimulationCatalog,
    /// 戦闘に加わる生産案（`catalog.production_units` の添字、購入列の順）。
    production: Vec<usize>,
    /// 既存unit、生産unitの順に並ぶ友軍の状態。
    friendly_states: Vec<UnitCombatState>,
    protected_states: Vec<UnitCombatState>,
    enemy_states: Vec<EnemyCombatState>,
    first_attack_turn: Option<u32>,
    turn_forecasts: Vec<CampaignTurnForecast>,
    completed_turns: u32,
    /// 全敵を排除して占領期限も過ぎたため、以後のturnを打ち切った。
    finished: bool,
}

impl<'c> CombatRun<'c> {
    fn start(catalog: &'c SimulationCatalog, state: &SearchState) -> Self {
        let mut run = Self {
            catalog,
            production: Vec::with_capacity(state.option_indices.len()),
            friendly_states: catalog
                .existing_units
                .iter()
                .map(UnitCombatState::of)
                .collect(),
            protected_states: catalog
                .protected_units
                .iter()
                .map(UnitCombatState::of)
                .collect(),
            enemy_states: catalog.enemies.iter().map(EnemyCombatState::of).collect(),
            first_attack_turn: None,
            turn_forecasts: Vec::new(),
            completed_turns: 0,
            finished: false,
        };
        for option_index in &state.option_indices {
            run.add_production(*option_index);
        }
        run
    }

    /// 生産unitを友軍の末尾へ加えた子案。生産turnまでの経過は親と同じである。
    fn with_purchase(&self, option_index: usize) -> Self {
        let mut child = self.clone();
        child.add_production(option_index);
        child
    }

    fn add_production(&mut self, option_index: usize) {
        let unit = &self.catalog.production_units[option_index];
        debug_assert!(
            unit.available_turn > self.completed_turns,
            "経過済みturnに交戦できる生産unitを後から加えない"
        );
        self.production.push(option_index);
        self.friendly_states.push(UnitCombatState::of(unit));
    }

    fn friendly(&self, index: usize) -> &'c SimFriendly {
        let existing = &self.catalog.existing_units;
        if index < existing.len() {
            &existing[index]
        } else {
            &self.catalog.production_units[self.production[index - existing.len()]]
        }
    }

    /// `last_turn` まで交戦を進める（打ち切り済みなら何もしない）。
    fn advance_to(&mut self, input: &RollingPlanInput, last_turn: u32) {
        while !self.finished && self.completed_turns < last_turn {
            let turn = self.completed_turns + 1;
            self.simulate_turn(input, turn);
            self.completed_turns = turn;
        }
    }

    fn simulate_turn(&mut self, input: &RollingPlanInput, turn: u32) {
        let catalog = self.catalog;
        let enemy_arrival_hp = catalog
            .enemies
            .iter()
            .filter(|enemy| enemy.source.available_turn == turn)
            .map(|enemy| enemy.source.hp)
            .sum();
        let enemy_hp_before = arrived_enemy_hp(&catalog.enemies, &self.enemy_states, turn);
        let friendly_hp_before = total_hp(&self.friendly_states) + total_hp(&self.protected_states);
        let attack_count = self.friendly_attacks(turn);
        self.enemy_attacks(turn);
        let enemy_hp_after = arrived_enemy_hp(&catalog.enemies, &self.enemy_states, turn);
        let friendly_hp_after = total_hp(&self.friendly_states) + total_hp(&self.protected_states);
        self.turn_forecasts.push(CampaignTurnForecast {
            turn,
            enemy_arrival_hp,
            enemy_hp_removed: enemy_hp_before.saturating_sub(enemy_hp_after),
            friendly_hp_lost: friendly_hp_before.saturating_sub(friendly_hp_after),
            attack_count,
        });
        if self.enemy_states.iter().all(|enemy| enemy.hp == 0)
            && input
                .capture_completion_turn
                .is_none_or(|capture_turn| turn >= capture_turn)
        {
            self.finished = true;
        }
    }

    /// 友軍が並び順に1回ずつ攻撃し、生存した標的から反撃を受ける。攻撃回数を返す。
    fn friendly_attacks(&mut self, turn: u32) -> u32 {
        let catalog = self.catalog;
        let mut attack_count = 0_u32;
        for index in 0..self.friendly_states.len() {
            let friendly = self.friendly(index);
            let friendly_state = &mut self.friendly_states[index];
            if friendly_state.hp == 0
                || friendly_state.attacks_left == 0
                || turn < friendly.available_turn
            {
                continue;
            }
            let Some((target_index, attack_profile)) =
                select_target(friendly, &catalog.enemies, &self.enemy_states, turn)
            else {
                continue;
            };
            let target = &mut self.enemy_states[target_index];
            let damage = calculate_damage_formula(
                attack_profile.base_damage,
                friendly_state.hp,
                catalog.enemies[target_index].source.defense_bonus,
                false,
            );
            if damage == 0 {
                continue;
            }
            self.first_attack_turn.get_or_insert(turn);
            attack_count = attack_count.saturating_add(1);
            target.hp = target.hp.saturating_sub(damage);
            friendly_state.attacks_left = friendly_state.attacks_left.saturating_sub(1);
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
                friendly_state.hp = friendly_state.hp.saturating_sub(counter);
            }
        }
        attack_count
    }

    /// 敵砲兵・直接戦闘unitも手番ごとに最も有利な友軍を攻撃する。
    /// 占領兵だけを損耗対象にすると、射程へ入った護衛砲台が無傷という予測になり、
    /// 間接火力だけの脆い編成を過大評価する。
    fn enemy_attacks(&mut self, turn: u32) {
        let catalog = self.catalog;
        for enemy_index in 0..catalog.enemies.len() {
            let enemy = &catalog.enemies[enemy_index];
            let enemy_hp = self.enemy_states[enemy_index].hp;
            if enemy_hp == 0
                || self.enemy_states[enemy_index].attacks_left == 0
                || turn < enemy.source.available_turn
            {
                continue;
            }
            let existing_count = catalog.existing_units.len();
            let existing_target = strongest_attack_in_damage_order(
                &catalog.existing_by_enemy_damage[enemy_index],
                &catalog.existing_units,
                &self.friendly_states[..existing_count],
                enemy_index,
                enemy_hp,
                turn,
            );
            let production_target = strongest_enemy_attack(
                self.production
                    .iter()
                    .map(|option_index| &catalog.production_units[*option_index]),
                &self.friendly_states[existing_count..],
                enemy_index,
                enemy_hp,
                turn,
            )
            .map(|(index, damage)| (existing_count + index, damage));
            // 既存unitの後に生産unitが並ぶ。同じdamage・HPなら後方の要素を選ぶ従来の順序を保つ。
            let combat_target = match (existing_target, production_target) {
                (Some(existing), Some(produced))
                    if (existing.1, attack_priority(&self.friendly_states, existing.0))
                        > (produced.1, attack_priority(&self.friendly_states, produced.0)) =>
                {
                    Some(existing)
                }
                (existing, None) => existing,
                (_, produced) => produced,
            };
            let protected_target = strongest_enemy_attack(
                catalog.protected_units.iter(),
                &self.protected_states,
                enemy_index,
                enemy_hp,
                turn,
            );
            let (target_states, target_index, damage) = match (combat_target, protected_target) {
                (Some((_index, damage)), Some((protected_index, protected_damage)))
                    if protected_damage > damage =>
                {
                    (
                        &mut self.protected_states,
                        protected_index,
                        protected_damage,
                    )
                }
                (Some((index, damage)), _) => (&mut self.friendly_states, index, damage),
                (None, Some((index, damage))) => (&mut self.protected_states, index, damage),
                (None, None) => {
                    continue;
                }
            };
            target_states[target_index].hp = target_states[target_index].hp.saturating_sub(damage);
            if damage == 0 {
                continue;
            }
            let enemy_state = &mut self.enemy_states[enemy_index];
            enemy_state.attacks_left = enemy_state.attacks_left.saturating_sub(1);
        }
    }

    fn into_plan(self, input: &RollingPlanInput, state: &SearchState) -> ForcePackagePlan {
        let catalog = self.catalog;
        let purchases: Vec<_> = state
            .option_indices
            .iter()
            .map(|index| input.production_options[*index].purchase)
            .collect();
        let enemy_states = &self.enemy_states;
        let elimination_turn = if enemy_states.is_empty() {
            Some(0)
        } else {
            enemy_states
                .iter()
                .map(|enemy| enemy.destroyed_turn)
                .collect::<Option<Vec<_>>>()
                .and_then(|turns| turns.into_iter().max())
        };
        // Combat枠の歩兵・重歩兵も、生き残ればそのまま前線施設を占領できる。
        // 専任Captureとして同数を二重要求せず、作戦パッケージ全体の占領能力を数える。
        let protected_survivor_count = self
            .protected_states
            .iter()
            .filter(|unit| unit.hp > 0)
            .count()
            + (0..self.friendly_states.len())
                .filter(|index| {
                    self.friendly(*index).stats.can_capture && self.friendly_states[*index].hp > 0
                })
                .count();
        let protected_force_survives = input.capture_completion_turn.is_none()
            || protected_survivor_count >= input.required_capture_survivors.max(1);
        // 固定2ターンを足さず、実campaignのPickup/Transit/Drop/Capture ETAと残敵排除の
        // 遅い方を採る。輸送編成がまだ存在しない場合は占領完了を予測しない。
        let occupation_turn = elimination_turn
            .zip(input.capture_completion_turn)
            .filter(|_| protected_force_survives)
            .map(|(elimination, capture)| elimination.max(capture));
        let expected_loss = (0..self.friendly_states.len())
            .map(|index| (self.friendly(index), &self.friendly_states[index]))
            .chain(catalog.protected_units.iter().zip(&self.protected_states))
            .fold(0_u32, |total, (unit, unit_state)| {
                let lost_hp = unit.initial_hp.saturating_sub(unit_state.hp);
                total.saturating_add(unit.stats.cost.saturating_mul(lost_hp) / 100)
            });
        let first_attack_turn = self.first_attack_turn;
        let turn_forecasts = self.turn_forecasts;
        ForcePackagePlan {
            capture_purchases: Vec::new(),
            combat_purchases: Vec::new(),
            front_breakthrough_turn: None,
            deadline_target_first_attack_turn: None,
            target_first_attack_turns: Vec::new(),
            blocked_next_turn_production_slots: 0,
            direct_fire_lane_count: 0,
            direct_front_shortfall: 0,
            direct_enemy_remaining_hp: Vec::new(),
            direct_damage_by_target: Vec::new(),
            existing_indirect_damage_by_target: Vec::new(),
            produced_indirect_damage_by_target: Vec::new(),
            surviving_combat_value: 0,
            required_overmatch_value: 0,
            overmatch_ready: false,
            deadline_capture_survivor_count: 0,
            candidates_pruned: 0,
            purchases,
            target_forecasts: catalog
                .enemies
                .iter()
                .zip(enemy_states)
                .map(|(enemy, enemy_state)| TargetForecast {
                    entity: enemy.source.entity,
                    unit_type: enemy.source.stats.unit_type,
                    available_turn: enemy.source.available_turn,
                    initial_hp: enemy.source.hp,
                    remaining_hp: enemy_state.hp,
                    destroyed_turn: enemy_state.destroyed_turn,
                })
                .collect(),
            turn_forecasts,
            feasible: elimination_turn.is_some() && protected_force_survives,
            first_attack_turn,
            elimination_turn,
            occupation_turn,
            production_cost: state.cost,
            expected_loss,
            protected_unit_count: input.protected_units.len(),
            protected_survivor_count,
            required_capture_survivor_count: input.required_capture_survivors,
            candidates_considered: 0,
            search_truncated: false,
        }
    }
}

/// 両モデルで同じ購入unitの基礎状態を作る。生産当日は行動せず、次手番からだけ
/// 行動できる。拡張の占領役も到着turnまでは待たせず、経路上での交戦を表現する。
fn sim_production_friendly(
    input: &RollingPlanInput,
    option: &ProductionPlanOption,
    can_advance_to_enemy: bool,
) -> SimFriendly {
    sim_friendly(
        input,
        &FriendlyPlanUnit {
            protection_completion_turn: None,
            stats: option.stats.clone(),
            position: option.purchase.facility,
            hp: 100,
            available_turn: option.purchase.build_turn.saturating_add(1),
            engageable_enemy_indices: option.engageable_enemy_indices.clone(),
        },
        can_advance_to_enemy,
    )
}

fn sim_friendly(
    input: &RollingPlanInput,
    source: &FriendlyPlanUnit,
    can_advance_to_enemy: bool,
) -> SimFriendly {
    // 保護完了時刻は、保護契約の生存を予測する拡張経路のモデルだけが使う。
    // 通常経路は従来どおり保護期限を持たない戦力として初期化する。
    let protection_completion_turn =
        if crate::ai::code_path::is_extended_at("v4/rolling_plan.rs::sim_friendly") {
            source.protection_completion_turn
        } else {
            None
        };
    EngagementTemplate::build(
        input,
        &source.stats,
        source.position,
        &source.engageable_enemy_indices,
    )
    .instantiate(
        source.available_turn,
        source.hp,
        can_advance_to_enemy,
        protection_completion_turn,
    )
}

/// 友軍1体と各敵の交戦条件のうち、行動可能turnに依存しない部分。
///
/// 敵・友軍位置と兵種は1回のRolling Plan探索中は不変である。同じbeam内の
/// 数千候補で距離・相性・接触turnを再計算せず、敵indexに対応する表へ固定する。
struct EngagementTemplate {
    stats: UnitStats,
    engageable_enemy_indices: Vec<usize>,
    attacks_left: u32,
    rows: Vec<EngagementRow>,
    target_order: Arc<[usize]>,
}

/// 敵1体との交戦条件。turnは友軍の行動可能turnからの相対値で持つ。
#[derive(Debug, Clone, Copy)]
struct EngagementRow {
    /// 友軍から攻撃できる場合の (基礎damage, 被反撃damage, 射撃位置までのturn)。
    attack: Option<(u32, u32, u32)>,
    /// 敵からこの友軍への基礎damage（0なら攻撃されない）。
    enemy_base_damage: u32,
    /// 敵が自力で射程へ入るturn（絶対値）。
    enemy_contact_turn: u32,
    /// 友軍が敵の射程へ入るまでの移動turn。
    friendly_travel: u32,
}

impl EngagementTemplate {
    fn build(
        input: &RollingPlanInput,
        stats: &UnitStats,
        position: GridPosition,
        engageable_enemy_indices: &[usize],
    ) -> Self {
        let total_ammo = stats.max_ammo1.saturating_add(stats.max_ammo2).max(1);
        let fuel_turns = stats
            .max_fuel
            .checked_div(stats.daily_fuel_consumption)
            .unwrap_or(u32::MAX);
        debug_assert!(
            engageable_enemy_indices
                .windows(2)
                .all(|pair| pair[0] < pair[1]),
            "交戦可能敵indexは盤面走査順の昇順で保持する"
        );
        let rows = input
            .enemies
            .iter()
            .enumerate()
            .map(|(enemy_index, enemy)| {
                let distance =
                    input
                        .map
                        .distance(position.x, position.y, enemy.position.x, enemy.position.y);
                let base_damage =
                    best_damage(&input.damage_chart, stats.unit_type, enemy.stats.unit_type);
                let can_attack = engageable_enemy_indices
                    .binary_search(&enemy_index)
                    .is_ok()
                    && base_damage > 0;
                let attack = can_attack.then(|| {
                    (
                        base_damage,
                        super::combat_profile::planned_counter_damage(
                            &input.master_data,
                            &input.damage_chart,
                            stats,
                            &enemy.stats,
                            distance,
                        ),
                        attack_readiness_turns(distance, stats),
                    )
                });
                let enemy_travel = attack_readiness_turns(distance, &enemy.stats);
                EngagementRow {
                    attack,
                    enemy_base_damage: best_damage(
                        &input.damage_chart,
                        enemy.stats.unit_type,
                        stats.unit_type,
                    ),
                    enemy_contact_turn: enemy.available_turn.saturating_add(enemy_travel),
                    friendly_travel: distance
                        .saturating_sub(enemy.stats.max_range.max(1))
                        .div_ceil(stats.max_movement.max(1)),
                }
            })
            .collect::<Vec<EngagementRow>>();
        let mut target_order = rows
            .iter()
            .enumerate()
            .filter_map(|(index, row)| {
                row.attack.map(|(_, incoming_damage, _)| {
                    (
                        std::cmp::Reverse(incoming_damage),
                        strategic_rank(&input.enemies[index].stats),
                        index,
                    )
                })
            })
            .collect::<Vec<_>>();
        target_order.sort_unstable();
        Self {
            stats: stats.clone(),
            engageable_enemy_indices: engageable_enemy_indices.to_vec(),
            attacks_left: total_ammo.min(fuel_turns.max(1)),
            rows,
            target_order: target_order
                .into_iter()
                .map(|(_, _, index)| index)
                .collect(),
        }
    }

    /// 同じ表を使える生産案か（兵種性能と交戦可能な敵が同じ）。
    fn matches(&self, option: &ProductionPlanOption) -> bool {
        self.stats == option.stats
            && self.engageable_enemy_indices == option.engageable_enemy_indices
    }

    fn instantiate(
        &self,
        available_turn: u32,
        hp: u32,
        can_advance_to_enemy: bool,
        protection_completion_turn: Option<u32>,
    ) -> SimFriendly {
        let attack_profiles = self
            .rows
            .iter()
            .map(|row| {
                row.attack.map(|(base_damage, incoming_damage, readiness)| {
                    FriendlyAttackProfile {
                        firing_position: None,
                        requires_movement: false,
                        base_damage,
                        incoming_damage,
                        ready_turn: available_turn.saturating_add(readiness),
                    }
                })
            })
            .collect::<Vec<_>>();
        let enemy_attack_profiles = self
            .rows
            .iter()
            .map(|row| {
                let contact_turn = if can_advance_to_enemy {
                    row.enemy_contact_turn
                        .min(available_turn.saturating_add(row.friendly_travel))
                } else {
                    row.enemy_contact_turn
                };
                (row.enemy_base_damage > 0).then_some(EnemyAttackProfile {
                    base_damage: row.enemy_base_damage,
                    contact_turn,
                })
            })
            .collect::<Vec<_>>();
        SimFriendly {
            purchase: None,
            capture_contract: None,
            protection_completion_turn,
            survived_protection_completion: None,
            stats: self.stats.clone(),
            hp,
            initial_hp: hp,
            available_turn,
            attacks_left: self.attacks_left,
            attack_profiles: attack_profiles.into(),
            enemy_attack_profiles: enemy_attack_profiles.into(),
            target_order: Arc::clone(&self.target_order),
        }
    }
}

/// 友軍1体の標的。比較順は（被反撃damageの大きい順、占領・輸送・その他の順、残HPの少ない順、index順）。
///
/// 占領能力だけで標的順を固定すると、後方の砲兵を放置したまま前衛へ損耗を重ねる。
/// 自分へ返せる最大与ダメージを先に比較し、同程度なら勝利条件へ直結する占領・
/// 輸送能力と残HPで集中撃破先を決める。前2項は探索中不変なので `target_order` の順に
/// 調べ、最初に攻撃できる敵と同じ前2項の範囲だけで残HPを比べる。
fn select_target(
    friendly: &SimFriendly,
    enemies: &[SimEnemy],
    enemy_states: &[EnemyCombatState],
    turn: u32,
) -> Option<(usize, FriendlyAttackProfile)> {
    let mut leading_group = None;
    let mut best: Option<((u32, usize), FriendlyAttackProfile)> = None;
    for &index in friendly.target_order.iter() {
        let Some(profile) = friendly.attack_profiles[index] else {
            continue;
        };
        let group = (
            profile.incoming_damage,
            strategic_rank(&enemies[index].source.stats),
        );
        if leading_group.is_some_and(|leading| leading != group) {
            break;
        }
        let enemy_hp = enemy_states[index].hp;
        if enemy_hp == 0
            || turn < enemies[index].source.available_turn
            || turn < profile.ready_turn
        {
            continue;
        }
        leading_group = Some(group);
        let key = (enemy_hp, index);
        if best.is_none_or(|(best_key, _)| key < best_key) {
            best = Some((key, profile));
        }
    }
    best.map(|((_, index), profile)| (index, profile))
}

/// 標的選択で占領能力・輸送能力を持つ敵を先に狙う順位。
fn strategic_rank(stats: &UnitStats) -> u8 {
    if stats.can_capture {
        0
    } else if stats.max_cargo > 0 {
        1
    } else {
        2
    }
}

/// 敵1体が今手番に与えられる最大damageの友軍（同点ならHPの低い方、さらに同点なら後方の要素）。
fn strongest_enemy_attack<'a>(
    units: impl Iterator<Item = &'a SimFriendly>,
    unit_states: &[UnitCombatState],
    enemy_index: usize,
    enemy_hp: u32,
    turn: u32,
) -> Option<(usize, u32)> {
    units
        .zip(unit_states)
        .enumerate()
        .filter(|(_, (unit, unit_state))| unit_state.hp > 0 && turn >= unit.available_turn)
        .filter_map(|(index, (unit, _))| {
            let profile = unit.enemy_attack_profiles[enemy_index]?;
            if turn < profile.contact_turn {
                return None;
            }
            let damage =
                calculate_damage_formula(profile.base_damage, enemy_hp, 0, true).saturating_add(10);
            Some((index, damage))
        })
        .max_by_key(|(index, damage)| (*damage, 100_u32.saturating_sub(unit_states[*index].hp)))
}

/// 同じdamageの友軍では、HPの低い方を敵が優先する。
fn attack_priority(states: &[UnitCombatState], index: usize) -> u32 {
    100_u32.saturating_sub(states[index].hp)
}

/// `strongest_enemy_attack` と同じ選択を、基礎damageの大きい順に並べた添字で行う。
/// damageは基礎damageについて単調なので、最良候補よりdamageが小さくなった時点で打ち切る。
fn strongest_attack_in_damage_order(
    order: &[usize],
    units: &[SimFriendly],
    unit_states: &[UnitCombatState],
    enemy_index: usize,
    enemy_hp: u32,
    turn: u32,
) -> Option<(usize, u32)> {
    let mut best: Option<(u32, u32, usize)> = None;
    for &index in order {
        let unit = &units[index];
        let Some(profile) = unit.enemy_attack_profiles[enemy_index] else {
            continue;
        };
        let damage =
            calculate_damage_formula(profile.base_damage, enemy_hp, 0, true).saturating_add(10);
        if best.is_some_and(|(best_damage, _, _)| damage < best_damage) {
            break;
        }
        if unit_states[index].hp == 0
            || turn < unit.available_turn
            || turn < profile.contact_turn
        {
            continue;
        }
        let key = (damage, attack_priority(unit_states, index), index);
        if best.is_none_or(|best_key| key > best_key) {
            best = Some(key);
        }
    }
    best.map(|(damage, _, index)| (index, damage))
}

/// 1候補のシミュレーション中に変わる友軍の状態。交戦条件はcatalogの `SimFriendly` が持つ。
#[derive(Debug, Clone, Copy)]
struct UnitCombatState {
    hp: u32,
    attacks_left: u32,
}

impl UnitCombatState {
    fn of(unit: &SimFriendly) -> Self {
        Self {
            hp: unit.hp,
            attacks_left: unit.attacks_left,
        }
    }
}

/// 1候補のシミュレーション中に変わる敵の状態。
#[derive(Debug, Clone, Copy)]
struct EnemyCombatState {
    hp: u32,
    attacks_left: u32,
    destroyed_turn: Option<u32>,
}

impl EnemyCombatState {
    fn of(enemy: &SimEnemy) -> Self {
        Self {
            hp: enemy.hp,
            attacks_left: enemy.attacks_left,
            destroyed_turn: enemy.destroyed_turn,
        }
    }
}

fn total_hp(states: &[UnitCombatState]) -> u32 {
    states.iter().map(|state| state.hp).sum()
}

/// `turn` までに戦場へ到着している敵の残HP合計。
fn arrived_enemy_hp(enemies: &[SimEnemy], states: &[EnemyCombatState], turn: u32) -> u32 {
    enemies
        .iter()
        .zip(states)
        .filter(|(enemy, _)| enemy.source.available_turn <= turn)
        .map(|(_, state)| state.hp)
        .sum()
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
    let allow_cargo =
        crate::ai::code_path::is_extended_at("v4/rolling_plan.rs::production_options");
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
                    || (!allow_cargo && stats.max_cargo > 0)
                    || !master_data.can_produce_unit(terrain.as_str(), *unit_type)
                    || !can_reach(*facility, stats)
                {
                    continue;
                }
                options.push(ProductionPlanOption {
                    capture_target: None,
                    capture_arrival_turn: None,
                    capture_completion_turn: None,
                    capture_durability: None,
                    capture_blocking_enemy_index: None,
                    purchase: PlannedPurchase {
                        facility: *facility,
                        unit_type: *unit_type,
                        build_turn,
                        cost: stats.cost,
                    },
                    stats: stats.clone(),
                    engageable_enemy_indices: Vec::new(),
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
            current_turn: 0,
            opening_policy: OpeningProductionPolicy::Standard,
            production_attack_projections: Vec::new(),
            deadline_target_index: None,
            interdiction_deadlines: Vec::new(),
            prioritize_deadline_interdiction: false,
            exact_property_control: false,
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
                    capture_target: None,
                    capture_arrival_turn: None,
                    capture_completion_turn: None,
                    capture_durability: None,
                    capture_blocking_enemy_index: None,
                    purchase: PlannedPurchase {
                        facility: GridPosition { x: 0, y: 0 },
                        unit_type: UnitType::Bcopters,
                        build_turn: 0,
                        cost: 7_500,
                    },
                    stats: stats(UnitType::Bcopters, 7_500, 6),
                    engageable_enemy_indices: vec![0],
                },
                ProductionPlanOption {
                    capture_target: None,
                    capture_arrival_turn: None,
                    capture_completion_turn: None,
                    capture_durability: None,
                    capture_blocking_enemy_index: None,
                    purchase: PlannedPurchase {
                        facility: GridPosition { x: 1, y: 0 },
                        unit_type: UnitType::Bomber,
                        build_turn: 0,
                        cost: 20_000,
                    },
                    stats: stats(UnitType::Bomber, 20_000, 6),
                    engageable_enemy_indices: vec![0],
                },
            ],
            current_funds: 30_000,
            income_per_turn: 0,
            hard_deadline: None,
            capture_completion_turn: None,
            required_capture_survivors: 0,
            delay_cost_per_turn: 5_000,
            search_beam_width: SEARCH_BEAM_WIDTH,
        }
    }

    /// F14の共通初期化は入力順、HP、行動可能turn、弾数を保持する。
    /// 購入identity・占領契約・保護期限は拡張モデルだけが持つ。
    #[test]
    fn simulation_catalog_preserves_input_order_and_model_specific_contracts() {
        let mut input = input();
        let friendly = |hp, available_turn| FriendlyPlanUnit {
            stats: stats(UnitType::Bcopters, 7_500, 6),
            position: GridPosition { x: 0, y: 0 },
            hp,
            available_turn,
            engageable_enemy_indices: vec![0],
            protection_completion_turn: Some(5),
        };
        input.existing_units = vec![friendly(37, 2), friendly(81, 3)];
        input.protected_units = vec![friendly(62, 4)];
        // 友軍は弾数の和を燃料で制限する。弾数0・燃料0でも従来の最低1を保持。
        input.existing_units[0].stats.max_ammo1 = u32::MAX;
        input.existing_units[0].stats.max_ammo2 = 1;
        input.existing_units[0].stats.max_fuel = 4;
        input.existing_units[0].stats.daily_fuel_consumption = 2;
        input.existing_units[1].stats.max_ammo1 = 0;
        input.existing_units[1].stats.max_ammo2 = 0;
        input.existing_units[1].stats.max_fuel = 0;
        input.existing_units[1].stats.daily_fuel_consumption = 1;
        // 占領契約のunitは自分から敵へ前進しないため、遅い敵の接触turnが変わる。
        input.enemies[0].stats.max_movement = 1;
        input.enemies[0].stats.max_range = 1;
        input.production_options[1].purchase.build_turn = u32::MAX;
        input.production_options[0].capture_target = Some(GridPosition { x: 8, y: 0 });
        input.production_options[0].capture_arrival_turn = Some(3);
        input.production_options[0].capture_completion_turn = Some(5);
        input.production_options[0].capture_durability = Some(20);
        input.production_options[0].capture_blocking_enemy_index = Some(0);
        // 敵の弾数は0でも最低1、和が溢れる場合は飽和する（燃料で制限しない）。
        input.enemies[0].stats.max_ammo1 = 0;
        input.enemies[0].stats.max_ammo2 = 0;
        let mut second_enemy = input.enemies[0].clone();
        second_enemy.hp = 53;
        second_enemy.entity = Some(Entity::from_raw(8));
        second_enemy.stats.max_ammo1 = u32::MAX;
        second_enemy.stats.max_ammo2 = 1;
        input.enemies.push(second_enemy);

        for (path, has_contracts) in [
            (crate::ai::code_path::CodePath::MainCompatible, false),
            (crate::ai::code_path::CodePath::Extended, true),
        ] {
            let _guard = crate::ai::code_path::enter(path);
            let catalog = SimulationCatalog::new(&input);
            assert_eq!(
                catalog
                    .existing_units
                    .iter()
                    .map(|unit| unit.hp)
                    .collect::<Vec<_>>(),
                vec![37, 81]
            );
            assert_eq!(catalog.protected_units[0].hp, 62);
            assert_eq!(catalog.existing_units[0].available_turn, 2);
            assert_eq!(catalog.existing_units[0].attacks_left, 2);
            assert_eq!(catalog.existing_units[1].attacks_left, 1);
            assert_eq!(
                catalog.production_units[0].enemy_attack_profiles[0]
                    .expect("敵歩兵からの被攻撃予測")
                    .contact_turn,
                if has_contracts { 7 } else { 3 },
                "{path:?}"
            );
            assert_eq!(catalog.production_units.len(), 2);
            assert_eq!(
                catalog.production_units[0].stats.unit_type,
                UnitType::Bcopters
            );
            assert_eq!(
                catalog.production_units[1].stats.unit_type,
                UnitType::Bomber
            );
            assert_eq!(catalog.production_units[0].hp, 100);
            assert_eq!(catalog.production_units[0].available_turn, 1);
            assert_eq!(catalog.production_units[1].available_turn, u32::MAX);
            assert_eq!(
                catalog.production_units[0].purchase.is_some(),
                has_contracts
            );
            assert_eq!(
                catalog.production_units[0].capture_contract.is_some(),
                has_contracts
            );
            assert!(catalog.production_units[1].capture_contract.is_none());
            assert_eq!(
                catalog.protected_units[0].protection_completion_turn,
                has_contracts.then_some(5)
            );
            assert_eq!(
                catalog
                    .enemies
                    .iter()
                    .map(|enemy| enemy.hp)
                    .collect::<Vec<_>>(),
                vec![100, 53]
            );
            assert_eq!(catalog.enemies[0].attacks_left, 1);
            assert_eq!(catalog.enemies[1].attacks_left, u32::MAX);
            assert_eq!(catalog.enemies[0].source.entity, Some(Entity::from_raw(7)));
            assert!(
                catalog
                    .enemies
                    .iter()
                    .all(|enemy| enemy.destroyed_turn.is_none())
            );
        }
    }

    /// projectionの行なしは近似を保持し、要素なしは攻撃不能としてprofileを除去する。
    /// 元から攻撃不能な敵のprofileは、projectionがあっても復活させない。
    #[test]
    fn simulation_catalog_applies_exact_profiles_only_on_extended_path() {
        let mut input = input();
        let mut second_enemy = input.enemies[0].clone();
        second_enemy.entity = Some(Entity::from_raw(8));
        input.enemies.push(second_enemy);
        input.production_options[0].engageable_enemy_indices = vec![0, 1];
        let projection = ProductionAttackProjection {
            ready_turn: 9,
            firing_position: GridPosition { x: 7, y: 0 },
            requires_movement: true,
        };
        // 1番目候補は両敵と交戦可。2番目候補は敵0だけと交戦可。
        input.production_attack_projections =
            vec![vec![Some(projection)], vec![None, Some(projection)]];
        for path in [
            crate::ai::code_path::CodePath::MainCompatible,
            crate::ai::code_path::CodePath::Extended,
        ] {
            let _guard = crate::ai::code_path::enter(path);
            let catalog = SimulationCatalog::new(&input);
            let first = catalog.production_units[0].attack_profiles[0].expect("交戦可能な敵0");
            if path == crate::ai::code_path::CodePath::Extended {
                assert_eq!(first.ready_turn, 9);
                assert_eq!(first.firing_position, Some(projection.firing_position));
                assert!(first.requires_movement);
                assert!(
                    catalog.production_units[0].attack_profiles[1].is_none(),
                    "短い行の欠落要素は除去"
                );
                assert!(
                    catalog.production_units[1].attack_profiles[0].is_none(),
                    "明示Noneは除去"
                );
            } else {
                assert!(first.ready_turn < 9);
                assert_eq!(first.firing_position, None);
                assert!(!first.requires_movement);
                assert!(catalog.production_units[0].attack_profiles[1].is_some());
                assert!(catalog.production_units[1].attack_profiles[0].is_some());
            }
            assert!(
                catalog.production_units[1].attack_profiles[1].is_none(),
                "攻撃不能profileは復活しない"
            );
        }
        input.production_attack_projections.truncate(1);
        let _guard = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
        let catalog = SimulationCatalog::new(&input);
        let profile =
            catalog.production_units[1].attack_profiles[0].expect("行自体がなければ近似を保持");
        assert_eq!(profile.firing_position, None);
        assert!(!profile.requires_movement);
    }

    /// F15のグループ化は同じ施設・turnの兵種を入力順で保持し、別turnを混ぜない。
    #[test]
    fn production_slot_groups_keep_candidate_order_and_separate_turns() {
        let mut input = input();
        let mut later = input.production_options[0].clone();
        later.purchase.build_turn = 2;
        let mut same_slot = input.production_options[1].clone();
        same_slot.purchase.facility = input.production_options[0].purchase.facility;
        input.production_options.extend([later, same_slot]);
        let groups = group_production_options_by_slot(&input);
        assert_eq!(groups.len(), 3);
        assert_eq!(
            groups[&input.production_options[0].purchase.into()],
            vec![0, 3]
        );
        assert_eq!(
            groups[&input.production_options[1].purchase.into()],
            vec![1]
        );
        assert_eq!(
            groups[&input.production_options[2].purchase.into()],
            vec![2]
        );
    }

    /// 累積購入費が生産turnまでの資金と等しければ許可し、1だけ超えれば除外する。
    /// 子状態の生成は親状態を変更せず、候補順・既存slot・既存占領対象を保持する。
    #[test]
    fn search_child_respects_cumulative_budget_and_preserves_parent_state() {
        let mut input = input();
        input.current_funds = 7;
        input.income_per_turn = 4;
        input.production_options[1].purchase.build_turn = 2;
        input.production_options[1].purchase.cost = 10;
        let slot = input.production_options[1].purchase.into();
        let previous_slot = input.production_options[0].purchase.into();
        let target = GridPosition { x: 8, y: 0 };
        let state = SearchState {
            option_indices: vec![0],
            used_slots: HashSet::from_iter([previous_slot]),
            used_capture_targets: HashSet::from_iter([target]),
            cost: 5,
        };
        let child = state
            .with_affordable_purchase(&input, 1, slot)
            .expect("5+10は7+4*2の資金と等しい");
        assert_eq!(child.option_indices, vec![0, 1]);
        assert_eq!(child.used_slots, HashSet::from_iter([previous_slot, slot]));
        assert_eq!(child.used_capture_targets, HashSet::from_iter([target]));
        assert_eq!(child.cost, 15);
        assert_eq!(state.option_indices, vec![0]);
        assert_eq!(state.used_slots, HashSet::from_iter([previous_slot]));
        assert_eq!(state.cost, 5);

        input.production_options[1].purchase.cost = 11;
        assert!(state.with_affordable_purchase(&input, 1, slot).is_none());
        // 将来収入を現在turnの購入に流用しない。
        input.production_options[1].purchase.build_turn = 0;
        input.production_options[1].purchase.cost = 3;
        assert!(
            state
                .with_affordable_purchase(&input, 1, input.production_options[1].purchase.into())
                .is_none()
        );
    }

    /// 購入費・収入の加算と乗算は、従来の探索と同じ飽和演算で扱う。
    #[test]
    fn search_child_preserves_saturating_budget_arithmetic() {
        let mut input = input();
        input.current_funds = u32::MAX - 1;
        input.income_per_turn = u32::MAX;
        input.production_options[0].purchase.build_turn = 2;
        input.production_options[0].purchase.cost = 10;
        let slot = input.production_options[0].purchase.into();
        let state = SearchState {
            cost: u32::MAX - 5,
            ..SearchState::default()
        };
        let child = state
            .with_affordable_purchase(&input, 0, slot)
            .expect("双方ともMAXに飽和");
        assert_eq!(child.cost, u32::MAX);
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
            capture_target: None,
            capture_arrival_turn: None,
            capture_completion_turn: None,
            capture_durability: None,
            capture_blocking_enemy_index: None,
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
        });
        let state = SearchState {
            used_capture_targets: HashSet::default(),
            option_indices: vec![0],
            used_slots: HashSet::from_iter([ProductionSlot {
                facility: GridPosition { x: 0, y: 0 },
                build_turn: 0,
            }]),
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

    #[test]
    fn production_unit_cannot_attack_before_travel_finishes() {
        let mut input = input();
        input.production_options.truncate(1);
        let plan = plan_force_package(&input).unwrap();
        assert!(plan.first_attack_turn.is_some_and(|turn| turn >= 3));
    }

    #[test]
    fn sim_friendly_keeps_protection_deadline_only_on_extended_path() {
        // 保護完了時刻は、保護契約を予測する拡張経路だけがsimulation unitへ渡す
        // （F17の経路差を固定する）。
        let input = input();
        let source = FriendlyPlanUnit {
            protection_completion_turn: Some(4),
            stats: UnitStats {
                unit_type: UnitType::Infantry,
                can_capture: true,
                ..UnitStats::mock()
            },
            position: GridPosition { x: 8, y: 0 },
            hp: 100,
            available_turn: 0,
            engageable_enemy_indices: Vec::new(),
        };
        for (path, expected) in [
            (crate::ai::code_path::CodePath::MainCompatible, None),
            (crate::ai::code_path::CodePath::Extended, Some(4)),
        ] {
            let _guard = crate::ai::code_path::enter(path);
            let simulated = sim_friendly(&input, &source, false);
            assert_eq!(simulated.protection_completion_turn, expected, "{path:?}");
        }
    }

    #[test]
    fn occupation_uses_live_campaign_eta_instead_of_a_fixed_delay() {
        let mut input = input();
        input.capture_completion_turn = Some(7);
        input.required_capture_survivors = 1;
        input.protected_units.push(FriendlyPlanUnit {
            protection_completion_turn: None,
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
            protection_completion_turn: None,
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
            protection_completion_turn: None,
            stats: stats(UnitType::Bomber, 20_000, 6),
            position: GridPosition { x: 6, y: 0 },
            hp: 100,
            // 敵を最終的には倒せるが、占領兵が先に損耗する状況を作る。
            available_turn: 2,
            engageable_enemy_indices: vec![0],
        });
        let capturer = FriendlyPlanUnit {
            protection_completion_turn: None,
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
                capture_target: None,
                capture_arrival_turn: None,
                capture_completion_turn: None,
                capture_durability: None,
                capture_blocking_enemy_index: None,
                purchase: PlannedPurchase {
                    facility: new_facility,
                    unit_type: UnitType::Bcopters,
                    build_turn: 0,
                    cost: 7_500,
                },
                stats: stats(UnitType::Bcopters, 7_500, 6),
                engageable_enemy_indices: vec![0],
            },
            ProductionPlanOption {
                capture_target: None,
                capture_arrival_turn: None,
                capture_completion_turn: None,
                capture_durability: None,
                capture_blocking_enemy_index: None,
                purchase: PlannedPurchase {
                    facility: old_facility,
                    unit_type: UnitType::Bcopters,
                    build_turn: 1,
                    cost: 7_500,
                },
                stats: stats(UnitType::Bcopters, 7_500, 6),
                engageable_enemy_indices: vec![0],
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
                capture_target: None,
                capture_arrival_turn: None,
                capture_completion_turn: None,
                capture_durability: None,
                capture_blocking_enemy_index: None,
                purchase: PlannedPurchase {
                    facility,
                    unit_type: UnitType::Bcopters,
                    build_turn: 1,
                    cost: 7_500,
                },
                stats: stats(UnitType::Bcopters, 7_500, 6),
                engageable_enemy_indices: vec![0],
            },
            ProductionPlanOption {
                capture_target: None,
                capture_arrival_turn: None,
                capture_completion_turn: None,
                capture_durability: None,
                capture_blocking_enemy_index: None,
                purchase: PlannedPurchase {
                    facility,
                    unit_type: UnitType::Bomber,
                    build_turn: 1,
                    cost: 20_000,
                },
                stats: stats(UnitType::Bomber, 20_000, 6),
                engageable_enemy_indices: vec![0],
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
                capture_target: None,
                capture_arrival_turn: None,
                capture_completion_turn: None,
                capture_durability: None,
                capture_blocking_enemy_index: None,
                purchase: PlannedPurchase {
                    facility,
                    unit_type: UnitType::Bcopters,
                    build_turn: 0,
                    cost: 7_500,
                },
                stats: stats(UnitType::Bcopters, 7_500, 6),
                engageable_enemy_indices: vec![0],
            },
            ProductionPlanOption {
                capture_target: None,
                capture_arrival_turn: None,
                capture_completion_turn: None,
                capture_durability: None,
                capture_blocking_enemy_index: None,
                purchase: PlannedPurchase {
                    facility,
                    unit_type: UnitType::Bcopters,
                    build_turn: 1,
                    cost: 7_500,
                },
                stats: stats(UnitType::Bcopters, 7_500, 6),
                engageable_enemy_indices: vec![0],
            },
            ProductionPlanOption {
                capture_target: None,
                capture_arrival_turn: None,
                capture_completion_turn: None,
                capture_durability: None,
                capture_blocking_enemy_index: None,
                purchase: PlannedPurchase {
                    facility,
                    unit_type: UnitType::Bomber,
                    build_turn: 2,
                    cost: 20_000,
                },
                stats: stats(UnitType::Bomber, 20_000, 6),
                engageable_enemy_indices: vec![0],
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
}

// 小規模マップ戦略など拡張経路の実装。main互換経路と本文が異なる関数と拡張専用の項目を置く。
mod extended;
#[allow(unused_imports)]
pub use self::extended::*;
