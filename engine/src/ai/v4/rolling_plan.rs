//! 島作戦の残敵排除を、金額ではなく実行可能な行動列として比較する純粋計画器。
//!
//! このモジュールは盤面を直接読まず、呼び出し側が作ったsnapshotだけを受け取る。
//! 既存戦力と複数ターンの生産候補を混ぜ、悲観側ダメージで残敵を排除できる
//! パッケージだけを費用・完了ターン・損耗で比較する。

use crate::components::{GridPosition, UnitStats};
use crate::resources::master_data::MasterDataRegistry;
use crate::resources::{DamageChart, Map, MovementType, Terrain, UnitType};
use crate::systems::combat::calculate_damage_formula;
use bevy_ecs::prelude::Entity;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;

// 1施設手番ごとの候補を順に展開するため、同手番に複数施設を使う混成案が
// beamから落ちない幅を確保する。候補全体の直積を走査する旧方式には戻さない。
pub(crate) const SEARCH_BEAM_WIDTH: usize = 64;
pub(crate) const DEFAULT_SEARCH_TURNS: u32 = 12;

/// 多工場の現手番物件制御で、実シミュレーションへ渡せる候補数上限。
///
/// 工場数と各工場の到達可能物件がともに多い盤面では、現在手番だけの選択でも
/// 直積が指数的に増える。これは戦術上の評価値ではなく、実行時間を有限に保つための
/// 候補数上限である。上限に達するまでは従来どおり全候補を厳密にシミュレーションする。
const MANY_FACTORY_PROPERTY_STATE_LIMIT: usize = 256;

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

/// 生産unitが観測敵へ初撃する際の、地形・手番順・射点を含む実行可能edge。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProductionAttackProjection {
    pub ready_turn: u32,
    pub firing_position: GridPosition,
    pub requires_movement: bool,
}

/// 初手の候補集合に適用する戦略パイプライン固有の制約。
///
/// 標準マップの探索へ小規模マップの役割下限や全額投入を混ぜないため、候補列挙の
/// 分岐ではなく入力時に一度だけ決定する。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OpeningProductionPolicy {
    Standard,
    SmallMapExpansion,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PlannedCapturePurchase {
    pub purchase: PlannedPurchase,
    pub target: GridPosition,
    pub completion_turn: Option<u32>,
    /// この物件への進入を妨げる敵index。別レーンの容易な敵を倒しても、この契約を
    /// 成立扱いにしないため、物件と実在敵の対応を最後まで保持する。
    pub blocking_enemy_index: Option<usize>,
    /// 占領完了時点にこの購入unitが生存していたか。
    pub survived_at_completion: bool,
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

    /// 生き残った敵HPを、マスターデータ上の再調達価値で比例換算する。
    /// 同じ100HPでも歩兵と戦車を同価値にせず、固定の兵種点ではなく盤面に存在する
    /// unit自身の価格と残HPだけを使う。
    fn remaining_enemy_value(&self, input: &RollingPlanInput) -> u64 {
        self.target_forecasts
            .iter()
            .zip(&input.enemies)
            .map(|(forecast, enemy)| {
                u64::from(enemy.stats.cost).saturating_mul(u64::from(forecast.remaining_hp)) / 100
            })
            .sum()
    }

    fn has_indirect_purchase(&self, input: &RollingPlanInput) -> bool {
        self.purchases.iter().any(|purchase| {
            input
                .production_options
                .iter()
                .find(|option| option.purchase == *purchase)
                .is_some_and(|option| option.stats.min_range > 1)
        })
    }

    /// 間接部隊が、同じ局地目標を担当する直接部隊の外側から射点を増やせるか。
    ///
    /// 戦力額の総和ではなく、各候補の実際の射点・到達turn・担当敵だけを見る。
    /// したがって、歩兵・装甲車・戦車のどれがscreenを担うかは相性シミュレーションに
    /// 任せ、砲撃役だけが直接部隊の任務を置き換える案を避けられる。
    fn indirect_fire_has_local_support(&self, input: &RollingPlanInput) -> bool {
        self.purchases.iter().all(|purchase| {
            let Some(option_index) = input
                .production_options
                .iter()
                .position(|option| option.purchase == *purchase)
            else {
                return true;
            };
            let option = &input.production_options[option_index];
            if option.stats.min_range <= 1 {
                return true;
            }
            input
                .production_attack_projections
                .get(option_index)
                .into_iter()
                .flatten()
                .enumerate()
                .filter_map(|(enemy_index, projection)| {
                    (*projection).map(|edge| (enemy_index, edge))
                })
                // 工場上から撃ち続ける砲は、次手番の生産枠を失うため前線の火力枠にはしない。
                .filter(|(_, edge)| edge.firing_position != option.purchase.facility)
                .any(|(enemy_index, edge)| {
                    direct_fire_saturates_target(input, self, enemy_index, edge.ready_turn)
                })
        })
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

    /// 期限が早い契約から見た累積未達数。
    ///
    /// 敵index順の初撃turnをそのまま辞書順比較すると、盤面走査で先に現れた無関係な敵へ
    /// 一発入れる案が有利になる。同じ期限の契約は同価値として束ね、明示された期限だけから
    /// 「この時点までに何件止め損ねたか」を作る。
    fn interdiction_deadline_shortfall_profile(&self, deadlines: &[(usize, u32)]) -> Vec<usize> {
        let mut checkpoints = deadlines
            .iter()
            .map(|(_, deadline)| *deadline)
            .collect::<Vec<_>>();
        checkpoints.sort_unstable();
        checkpoints.dedup();
        checkpoints
            .into_iter()
            .map(|checkpoint| {
                deadlines
                    .iter()
                    .filter(|(_, deadline)| *deadline <= checkpoint)
                    .filter(|(index, deadline)| {
                        !self
                            .target_first_attack_turns
                            .get(*index)
                            .and_then(|turn| *turn)
                            .is_some_and(|turn| turn <= *deadline)
                    })
                    .count()
            })
            .collect()
    }

    /// 明示された妨害対象だけの初撃時刻。非契約敵への早い小ダメージは比較へ混ぜない。
    fn interdiction_attack_turn_profile(&self, deadlines: &[(usize, u32)]) -> Vec<u32> {
        let mut turns = deadlines
            .iter()
            .map(|(index, _)| {
                self.target_first_attack_turns
                    .get(*index)
                    .and_then(|turn| *turn)
                    .unwrap_or(u32::MAX)
            })
            .collect::<Vec<_>>();
        turns.sort_unstable();
        turns
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

    /// 物件ごとに対応付けた妨害敵を排除し、同じ占領役が生存して完了したレーン数。
    ///
    /// 撃破時刻と占領時刻を並べ替えて対応させると、左翼の容易な敵を倒しただけで
    /// 中央レーンまで開通したことになる。生産入力で確定した物件―敵のidentityを使う。
    fn cleared_capture_lane_count(&self) -> usize {
        self.capture_purchases
            .iter()
            .filter(|assignment| {
                let Some(completion_turn) = assignment.completion_turn else {
                    return false;
                };
                if !assignment.survived_at_completion {
                    return false;
                }
                assignment.blocking_enemy_index.is_none_or(|enemy_index| {
                    self.target_forecasts
                        .get(enemy_index)
                        .and_then(|target| target.destroyed_turn)
                        .is_some_and(|destroyed_turn| destroyed_turn <= completion_turn)
                })
            })
            .count()
            .min(self.required_capture_survivor_count)
    }

    /// 一体目だけでなく、観測した前線敵を何手番で順次掃討できるかを比較する。
    fn target_destruction_profile(&self) -> Vec<u32> {
        let mut turns = self
            .target_forecasts
            .iter()
            .map(|target| target.destroyed_turn.unwrap_or(u32::MAX))
            .collect::<Vec<_>>();
        turns.sort_unstable();
        turns
    }

    /// 対応する敵の排除と占領完了が両方そろう時刻を、レーンごとに比較する。
    fn property_contract_completion_profile(&self) -> Vec<u32> {
        let mut turns = self
            .capture_purchases
            .iter()
            .map(|assignment| {
                if !assignment.survived_at_completion {
                    return u32::MAX;
                }
                let capture = assignment.completion_turn.unwrap_or(u32::MAX);
                let destruction = assignment.blocking_enemy_index.map_or(0, |enemy_index| {
                    self.target_forecasts
                        .get(enemy_index)
                        .and_then(|target| target.destroyed_turn)
                        .unwrap_or(u32::MAX)
                });
                capture.max(destruction)
            })
            .collect::<Vec<_>>();
        // `[] < [turn]` によって契約を一件も引き受けない案が有利にならないよう、
        // 不足契約は未達時刻で埋める。同じ入力ではrequired数が共通なので比較可能。
        if turns.len() < self.required_capture_survivor_count {
            turns.resize(self.required_capture_survivor_count, u32::MAX);
        }
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
    /// 生産時に割り当てた物件契約。戦闘と占領を別unitとして二重計上しない。
    capture_contract: Option<SimCaptureContract>,
    protection_completion_turn: Option<u32>,
    survived_protection_completion: Option<bool>,
}

#[derive(Debug, Clone)]
struct SimCaptureContract {
    target: GridPosition,
    arrival_turn: u32,
    remaining_durability: u32,
    blocking_enemy_index: Option<usize>,
    completed_turn: Option<u32>,
    survived_at_completion: bool,
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
                        position: option.purchase.facility,
                        hp: 100,
                        // 生産unitは兵種・任務にかかわらず次の自軍手番からだけ行動できる。
                        // 物件到着時刻まで戦闘不能にすると、経路上の迎撃を表現できない。
                        available_turn: option.purchase.build_turn.saturating_add(1),
                        engageable_enemy_indices: option.engageable_enemy_indices.clone(),
                        protection_completion_turn: None,
                    },
                    option.capture_target.is_none(),
                );
                simulated.purchase = Some(option.purchase);
                debug_assert!(option.capture_completion_turn.is_none_or(|completion| {
                    option
                        .capture_arrival_turn
                        .is_some_and(|arrival| completion >= arrival)
                }));
                simulated.capture_contract = option.capture_target.and_then(|target| {
                    Some(SimCaptureContract {
                        target,
                        arrival_turn: option.capture_arrival_turn?,
                        remaining_durability: option.capture_durability?,
                        blocking_enemy_index: option.capture_blocking_enemy_index,
                        completed_turn: None,
                        survived_at_completion: false,
                    })
                });
                // 生産判断側で計算した実経路ETAがあれば、格子距離による近似を
                // 上書きする。山・川・橋・最小射程を無視した快速判定を防ぐ。
                if let Some(exact_projections) =
                    input.production_attack_projections.get(option_index)
                {
                    let mut profiles = simulated.attack_profiles.to_vec();
                    for (enemy_index, profile) in profiles.iter_mut().enumerate() {
                        match (
                            profile.as_mut(),
                            exact_projections.get(enemy_index).copied().flatten(),
                        ) {
                            (Some(profile), Some(projection)) => {
                                profile.ready_turn = projection.ready_turn;
                                profile.firing_position = Some(projection.firing_position);
                                profile.requires_movement = projection.requires_movement;
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
                update_best_feasible(input, &mut best_feasible, &plan, input.delay_cost_per_turn);
                if !input.prioritize_deadline_interdiction
                    && input.hard_deadline.is_some_and(|deadline| {
                        plan.elimination_turn.is_some_and(|turn| turn <= deadline)
                    })
                {
                    update_best_feasible(
                        input,
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
            } else if uses_small_map_tactical_rules(input) {
                // 小規模マップでは、安価な歩兵で後から囲む案より、敵が前線へ侵入する前に
                // 先制攻撃できる案を枝刈りで残す。
                evaluated_next.sort_by_key(|(_, plan)| {
                    (
                        plan.first_attack_turn.unwrap_or(u32::MAX),
                        plan.remaining_hp(),
                        plan.expected_loss,
                        plan.completion_for_ordering(),
                        plan.production_cost,
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

/// 購入済み候補を、敵一体への最初の一発だけを割り当てて評価する。
/// 同じunitは同じ手番に二つの敵へ撃てないが、別手番なら別の敵を止められる。
#[allow(dead_code)]
fn evaluate_opening_package_static(
    input: &RollingPlanInput,
    state: &SearchState,
) -> ForcePackagePlan {
    let mut purchases = state
        .option_indices
        .iter()
        .map(|index| input.production_options[*index].purchase)
        .collect::<Vec<_>>();
    purchases.sort_unstable_by_key(|purchase| {
        (
            purchase.build_turn,
            purchase.facility.y,
            purchase.facility.x,
        )
    });

    let capture_purchases = state
        .option_indices
        .iter()
        .filter_map(|index| {
            let option = &input.production_options[*index];
            Some(PlannedCapturePurchase {
                purchase: option.purchase,
                target: option.capture_target?,
                completion_turn: option.capture_completion_turn,
                blocking_enemy_index: option.capture_blocking_enemy_index,
                // 初動にはまだ敵の反撃手番が無い。生存判定は次手番に実盤面で行う。
                survived_at_completion: true,
            })
        })
        .collect::<Vec<_>>();

    // 明示的な妨害契約に加え、占領レーンを塞ぐ敵には占領完了時刻までの初撃を要求する。
    let mut contract_deadlines = HashMap::<usize, u32>::new();
    for (enemy_index, deadline) in &input.interdiction_deadlines {
        contract_deadlines
            .entry(*enemy_index)
            .and_modify(|current| *current = (*current).min(*deadline))
            .or_insert(*deadline);
    }
    for capture in &capture_purchases {
        if let (Some(enemy_index), Some(deadline)) =
            (capture.blocking_enemy_index, capture.completion_turn)
        {
            contract_deadlines
                .entry(enemy_index)
                .and_modify(|current| *current = (*current).min(deadline))
                .or_insert(deadline);
        }
    }
    let mut contracts = contract_deadlines.into_iter().collect::<Vec<_>>();
    contracts.sort_unstable_by_key(|(enemy_index, deadline)| (*deadline, *enemy_index));

    let mut used_unit_turns = HashSet::new();
    let mut target_first_attack_turns = vec![None; input.enemies.len()];
    let mut target_damage = vec![0_u32; input.enemies.len()];
    let mut combat_purchases = Vec::new();
    let mut assigned_units = HashSet::new();
    for (enemy_index, deadline) in contracts {
        let mut best: Option<(u32, std::cmp::Reverse<u32>, u32, usize, u32)> = None;
        for option_index in &state.option_indices {
            let option = &input.production_options[*option_index];
            if option.capture_target.is_some() {
                continue;
            }
            let Some(projection) = input
                .production_attack_projections
                .get(*option_index)
                .and_then(|projections| projections.get(enemy_index))
                .and_then(|projection| *projection)
            else {
                continue;
            };
            let damage = best_damage(
                &input.damage_chart,
                option.stats.unit_type,
                input.enemies[enemy_index].stats.unit_type,
            );
            if damage == 0 || projection.ready_turn > deadline {
                continue;
            }
            for turn in projection.ready_turn..=deadline {
                if used_unit_turns.contains(&(*option_index, turn)) {
                    continue;
                }
                let candidate = (
                    turn,
                    std::cmp::Reverse(damage),
                    option.purchase.cost,
                    *option_index,
                    damage,
                );
                if best.as_ref().is_none_or(|current| candidate < *current) {
                    best = Some(candidate);
                }
                break;
            }
        }
        if let Some((turn, _, _, option_index, damage)) = best {
            used_unit_turns.insert((option_index, turn));
            target_first_attack_turns[enemy_index] = Some(turn);
            target_damage[enemy_index] = target_damage[enemy_index].saturating_add(damage);
            if assigned_units.insert(option_index)
                && let Some(entity) = input.enemies[enemy_index].entity
            {
                combat_purchases.push(PlannedCombatPurchase {
                    purchase: input.production_options[option_index].purchase,
                    target: entity,
                });
            }
        }
    }

    let target_forecasts = input
        .enemies
        .iter()
        .enumerate()
        .map(|(index, enemy)| {
            let remaining_hp = enemy.hp.saturating_sub(target_damage[index]);
            TargetForecast {
                entity: enemy.entity,
                unit_type: enemy.stats.unit_type,
                available_turn: enemy.available_turn,
                initial_hp: enemy.hp,
                remaining_hp,
                destroyed_turn: (remaining_hp == 0)
                    .then_some(target_first_attack_turns[index].unwrap_or(u32::MAX)),
            }
        })
        .collect::<Vec<_>>();
    let first_attack_turn = target_first_attack_turns.iter().flatten().copied().min();
    let elimination_turn = target_forecasts
        .iter()
        .all(|target| target.destroyed_turn.is_some())
        .then(|| {
            target_forecasts
                .iter()
                .filter_map(|target| target.destroyed_turn)
                .max()
                .unwrap_or_default()
        });
    let occupation_turn = capture_purchases
        .iter()
        .map(|capture| capture.completion_turn)
        .collect::<Option<Vec<_>>>()
        .filter(|_| capture_purchases.len() >= input.required_capture_survivors)
        .map(|turns| turns.into_iter().max().unwrap_or_default());
    let deadline_capture_survivor_count = capture_purchases
        .iter()
        .filter(|capture| {
            capture
                .completion_turn
                .is_some_and(|turn| input.hard_deadline.is_none_or(|deadline| turn <= deadline))
        })
        .count()
        .min(input.required_capture_survivors);
    let surviving_combat_value = purchases.iter().map(|purchase| purchase.cost).sum();
    let protected_unit_count = capture_purchases.len();

    ForcePackagePlan {
        purchases,
        capture_purchases,
        combat_purchases,
        target_forecasts,
        turn_forecasts: Vec::new(),
        feasible: false,
        first_attack_turn,
        front_breakthrough_turn: elimination_turn,
        deadline_target_first_attack_turn: input
            .deadline_target_index
            .and_then(|index| target_first_attack_turns.get(index).and_then(|turn| *turn)),
        target_first_attack_turns,
        elimination_turn,
        occupation_turn,
        production_cost: state.cost,
        expected_loss: 0,
        blocked_next_turn_production_slots: 0,
        direct_fire_lane_count: 0,
        direct_front_shortfall: 0,
        direct_enemy_remaining_hp: Vec::new(),
        direct_damage_by_target: Vec::new(),
        existing_indirect_damage_by_target: Vec::new(),
        produced_indirect_damage_by_target: Vec::new(),
        surviving_combat_value,
        required_overmatch_value: 0,
        overmatch_ready: false,
        protected_unit_count,
        protected_survivor_count: deadline_capture_survivor_count,
        deadline_capture_survivor_count,
        required_capture_survivor_count: input.required_capture_survivors,
        candidates_considered: 0,
        candidates_pruned: 0,
        search_truncated: false,
    }
}

/// 物件レースにおける抽象生産要求（役割）。
/// 施設ごとの順列展開を排除し、マルチセット（何を何体作るか）として組合せを表現する。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum ProductionRole {
    Capture {
        target: GridPosition,
        unit_type: UnitType,
        cost: u32,
    },
    Combat {
        unit_type: UnitType,
        cost: u32,
    },
}

impl ProductionRole {
    fn cost(&self) -> u32 {
        match *self {
            ProductionRole::Capture { cost, .. } | ProductionRole::Combat { cost, .. } => cost,
        }
    }

    fn capture_target(&self) -> Option<GridPosition> {
        match *self {
            ProductionRole::Capture { target, .. } => Some(target),
            ProductionRole::Combat { .. } => None,
        }
    }

    fn unit_type(&self) -> UnitType {
        match *self {
            ProductionRole::Capture { unit_type, .. }
            | ProductionRole::Combat { unit_type, .. } => unit_type,
        }
    }
}

/// 多工場の物件レースにおいて各サイズ段階で保持するマルチセット候補のビーム幅。
/// 通常マップ（施設数5以下）では各段階の候補数がこの上限に達しないため、全候補が保持される。
/// 施設数が多い盤面での順列・組合せ爆発を防ぎつつ、最も有望な編成を保持する。
const MULTISET_BEAM_WIDTH: usize = 384;

/// 同一の戦闘ユニット役割を同一手番に重複生産する上限。
/// 前線物件制御において同種戦闘ユニット（戦車・対空等）の極端な偏重を抑え、諸兵科連合を促す。
const MAX_COMBAT_ROLE_COPIES: usize = 3;

#[derive(Debug, Clone)]
struct RoleMetadata {
    cost: u32,
    capture_target: Option<GridPosition>,
    capacity: usize,
    min_capture_completion: u32,
    is_combat_non_infantry: bool,
    max_movement: u32,
    damage_by_enemy: Vec<u32>,
}

#[derive(Clone)]
struct MultisetCandidate {
    role_indices: Vec<usize>,
    /// 占領対象数は高々数個〜10個程度（MAX_CAPTURE_SLOTS=8）に収まるため、
    /// HashSetのヒープ確保・ハッシュ計算オーバーヘッドを避け、連続メモリのVecによる線形探索（L1キャッシュ効率優先）を採用。
    used_targets: Vec<GridPosition>,
    current_role_count: usize,
    remaining_funds: u32,
    last_role_idx: usize,
}

/// 物件制御における候補（マルチセットおよびSearchState）の順序付けキー。
/// 評価軸の一貫性を保ち、multiset探索段階と最終フロンティア段階での評価乖離を防ぐ。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct PropertyFrontierKey {
    /// 占領必要数に対する不足数（0が最良）
    capture_shortfall: usize,
    /// 小規模戦術ルールにおける占領完了予定ターン（昇順）
    small_map_capture_completion: Vec<u32>,
    /// 小規模戦術ルールにおける非歩兵戦闘ユニット数（降順）
    combat_unit_count_desc: std::cmp::Reverse<usize>,
    /// 全ユニットの合計移動力（降順）
    total_movement_desc: std::cmp::Reverse<u32>,
    /// 締切対象・関連敵の残HP合計（昇順）
    remaining_relevant_hp: u32,
    /// 全敵の残HP合計（昇順）
    remaining_all_hp: u32,
    /// 全占領予定の完了ターン（昇順、required_capturersでresize済み）
    capture_completion_turns: Vec<u32>,
    /// 占領必要数を超える余剰占領ユニットのペナルティ（昇順）
    excess_capturers: usize,
    /// 生産コスト合計（昇順）
    cost: u32,
    /// 同点時の決定論的タイブレーク用インデックス列
    identity_indices: Vec<usize>,
}

/// 観測量から `PropertyFrontierKey` を共通ロジックで構築する。
#[allow(clippy::too_many_arguments)]
fn build_property_frontier_key(
    input: &RollingPlanInput,
    capture_count: usize,
    mut capture_completion_turns: Vec<u32>,
    combat_unit_count: usize,
    total_movement: u32,
    damage_by_enemy: &[u32],
    total_cost: u32,
    identity_indices: Vec<usize>,
) -> PropertyFrontierKey {
    let opening_capture_floor = if uses_small_map_opening_rules(input) {
        2
    } else {
        0
    };
    let required_capturers = input.required_capture_survivors.max(opening_capture_floor);

    let capture_shortfall = required_capturers.saturating_sub(capture_count);
    capture_completion_turns.sort_unstable();
    capture_completion_turns.truncate(required_capturers.max(1));
    capture_completion_turns.resize(required_capturers.max(1), u32::MAX);

    let small_map_capture_completion = if uses_small_map_tactical_rules(input) {
        capture_completion_turns.clone()
    } else {
        Vec::new()
    };

    let (combat_unit_count_desc, total_movement_desc) = if uses_small_map_tactical_rules(input) {
        (
            std::cmp::Reverse(combat_unit_count),
            std::cmp::Reverse(total_movement),
        )
    } else {
        (std::cmp::Reverse(0), std::cmp::Reverse(0))
    };

    let remaining_by_enemy: Vec<u32> = input
        .enemies
        .iter()
        .enumerate()
        .map(|(index, enemy)| enemy.hp.saturating_sub(damage_by_enemy[index]))
        .collect();

    let relevant_enemy_indices: Vec<usize> = if input.interdiction_deadlines.is_empty() {
        (0..input.enemies.len()).collect()
    } else {
        input
            .interdiction_deadlines
            .iter()
            .map(|(index, _)| *index)
            .collect()
    };

    let remaining_relevant_hp: u32 = relevant_enemy_indices
        .into_iter()
        .filter_map(|index| remaining_by_enemy.get(index))
        .copied()
        .sum();
    let remaining_all_hp: u32 = remaining_by_enemy.into_iter().sum();

    PropertyFrontierKey {
        capture_shortfall,
        small_map_capture_completion,
        combat_unit_count_desc,
        total_movement_desc,
        remaining_relevant_hp,
        remaining_all_hp,
        capture_completion_turns,
        excess_capturers: capture_count.saturating_sub(required_capturers),
        cost: total_cost,
        identity_indices,
    }
}

fn multiset_frontier_key(
    input: &RollingPlanInput,
    role_indices: &[usize],
    role_metas: &[RoleMetadata],
) -> PropertyFrontierKey {
    let mut capture_count = 0_usize;
    let mut capture_completion_turns = Vec::new();
    let mut combat_unit_count = 0_usize;
    let mut total_movement = 0_u32;
    let mut total_cost = 0_u32;
    let mut damage_by_enemy = vec![0_u32; input.enemies.len()];

    for &idx in role_indices {
        let meta = &role_metas[idx];
        total_cost = total_cost.saturating_add(meta.cost);
        total_movement = total_movement.saturating_add(meta.max_movement);

        if meta.capture_target.is_some() {
            capture_count += 1;
            if meta.min_capture_completion < u32::MAX {
                capture_completion_turns.push(meta.min_capture_completion);
            }
        } else {
            if meta.is_combat_non_infantry {
                combat_unit_count += 1;
            }
            for (enemy_idx, &dmg) in meta.damage_by_enemy.iter().enumerate() {
                damage_by_enemy[enemy_idx] = damage_by_enemy[enemy_idx].saturating_add(dmg);
            }
        }
    }

    build_property_frontier_key(
        input,
        capture_count,
        capture_completion_turns,
        combat_unit_count,
        total_movement,
        &damage_by_enemy,
        total_cost,
        role_indices.to_vec(),
    )
}

/// 役割のマルチセット（重複組合せ）を資金上限およびスロット上限の範囲で生成する。
/// 同一占領対象への二重割当は除外され、戦闘ユニットは重複が許容される。
/// 施設数が多い盤面では、サイズ段階ごとにビーム幅で有望な候補を維持し、組合せ爆発を防ぐ。
fn generate_role_multisets(
    roles: &[ProductionRole],
    input: &RollingPlanInput,
    facilities: &[(GridPosition, Vec<usize>)],
    facility_role_options: &HashMap<(GridPosition, ProductionRole), usize>,
    current_funds: u32,
    max_slots: usize,
) -> (Vec<Vec<ProductionRole>>, bool) {
    if roles.is_empty() || max_slots == 0 {
        return (Vec::new(), false);
    }

    let role_metas: Vec<RoleMetadata> = roles
        .iter()
        .map(|role| {
            let matching_facs: Vec<(GridPosition, usize)> = facilities
                .iter()
                .filter_map(|(fac, _)| {
                    facility_role_options
                        .get(&(*fac, *role))
                        .copied()
                        .map(|opt_idx| (*fac, opt_idx))
                })
                .collect();
            let capacity = matching_facs.len();

            let first_opt = matching_facs
                .first()
                .map(|&(_, opt_idx)| &input.production_options[opt_idx]);
            let is_combat_non_infantry = role.capture_target().is_none()
                && first_opt.is_some_and(|opt| opt.stats.movement_type != MovementType::Infantry);
            let max_movement = first_opt.map_or(0, |opt| opt.stats.max_movement);

            let min_capture_completion = if role.capture_target().is_some() {
                matching_facs
                    .iter()
                    .filter_map(|&(_, opt_idx)| {
                        input.production_options[opt_idx].capture_completion_turn
                    })
                    .min()
                    .unwrap_or(u32::MAX)
            } else {
                u32::MAX
            };

            let damage_by_enemy: Vec<u32> = input
                .enemies
                .iter()
                .enumerate()
                .map(|(enemy_idx, enemy)| {
                    if role.capture_target().is_some() {
                        return 0;
                    }
                    let can_reach = matching_facs.iter().any(|&(_, opt_idx)| {
                        input
                            .production_attack_projections
                            .get(opt_idx)
                            .map_or_else(
                                || {
                                    input.production_options[opt_idx]
                                        .engageable_enemy_indices
                                        .contains(&enemy_idx)
                                },
                                |projections| {
                                    projections.get(enemy_idx).is_some_and(Option::is_some)
                                },
                            )
                    });
                    if can_reach {
                        best_damage(&input.damage_chart, role.unit_type(), enemy.stats.unit_type)
                    } else {
                        0
                    }
                })
                .collect();

            RoleMetadata {
                cost: role.cost(),
                capture_target: role.capture_target(),
                capacity,
                min_capture_completion,
                is_combat_non_infantry,
                max_movement,
                damage_by_enemy,
            }
        })
        .collect();

    let initial = MultisetCandidate {
        role_indices: Vec::new(),
        used_targets: Vec::new(),
        current_role_count: 0,
        remaining_funds: current_funds,
        last_role_idx: 0,
    };

    let mut current_level = vec![initial];
    let mut all_multisets = Vec::new();
    let mut truncated = false;

    for _size in 1..=max_slots {
        let mut next_level = Vec::new();
        for parent in &current_level {
            for (i, meta) in role_metas.iter().enumerate().skip(parent.last_role_idx) {
                if meta.cost > parent.remaining_funds {
                    continue;
                }

                let next_role_count = if i == parent.last_role_idx {
                    parent.current_role_count.saturating_add(1)
                } else {
                    1
                };

                if let Some(target) = meta.capture_target {
                    if parent.used_targets.contains(&target) {
                        continue;
                    }
                    let mut used_targets = parent.used_targets.clone();
                    used_targets.push(target);
                    let mut new_role_indices = parent.role_indices.clone();
                    new_role_indices.push(i);
                    next_level.push(MultisetCandidate {
                        role_indices: new_role_indices,
                        used_targets,
                        current_role_count: next_role_count,
                        remaining_funds: parent.remaining_funds.saturating_sub(meta.cost),
                        last_role_idx: i,
                    });
                } else if next_role_count <= meta.capacity
                    && next_role_count <= MAX_COMBAT_ROLE_COPIES
                {
                    let mut new_role_indices = parent.role_indices.clone();
                    new_role_indices.push(i);
                    next_level.push(MultisetCandidate {
                        role_indices: new_role_indices,
                        used_targets: parent.used_targets.clone(),
                        current_role_count: next_role_count,
                        remaining_funds: parent.remaining_funds.saturating_sub(meta.cost),
                        last_role_idx: i,
                    });
                }
            }
        }

        if next_level.len() > MULTISET_BEAM_WIDTH {
            truncated = true;
            next_level.sort_unstable_by_key(|cand| {
                multiset_frontier_key(input, &cand.role_indices, &role_metas)
            });
            next_level.truncate(MULTISET_BEAM_WIDTH);
        }

        for cand in &next_level {
            all_multisets.push(cand.role_indices.iter().map(|&idx| roles[idx]).collect());
        }

        current_level = next_level;
        if current_level.is_empty() {
            break;
        }
    }

    (all_multisets, truncated)
}

/// ある施設に特定の役割を割り当てた際のコスト（所要ターン数・距離）。
/// 小さいほど優秀な割当。鈍足ユニット（歩兵等）を前線工場に、俊足ユニットを後方工場に
/// 自然に誘導する。
fn calculate_assignment_cost(
    input: &RollingPlanInput,
    opt: &ProductionPlanOption,
    opt_idx: usize,
    fac_pos: GridPosition,
) -> u64 {
    if let Some(target) = opt.capture_target {
        let arrival = opt.capture_arrival_turn.unwrap_or(99) as u64;
        let completion = opt.capture_completion_turn.unwrap_or(99) as u64;
        let dist = input.map.distance(fac_pos.x, fac_pos.y, target.x, target.y) as u64;
        arrival * 10_000 + completion * 1_000 + dist
    } else {
        let speed = opt.stats.max_movement.max(1) as u64;
        let proj_earliest = input
            .production_attack_projections
            .get(opt_idx)
            .and_then(|projs| projs.iter().flatten().map(|p| p.ready_turn).min());

        let target_pos = input
            .enemies
            .iter()
            .min_by_key(|e| {
                input
                    .map
                    .distance(fac_pos.x, fac_pos.y, e.position.x, e.position.y)
            })
            .map(|e| e.position)
            .or_else(|| {
                input
                    .production_options
                    .iter()
                    .filter_map(|o| o.capture_target)
                    .min_by_key(|t| input.map.distance(fac_pos.x, fac_pos.y, t.x, t.y))
            });
        let dist = target_pos
            .map(|t| input.map.distance(fac_pos.x, fac_pos.y, t.x, t.y) as u64)
            .unwrap_or(0);

        if let Some(ready_turn) = proj_earliest {
            (ready_turn as u64) * 10_000 + dist
        } else {
            let eta = dist.div_ceil(speed);
            eta * 10_000 + dist
        }
    }
}

/// 役割列と施設列の最小費用二部マッチング（Branch and Bound）。
#[allow(clippy::too_many_arguments)]
fn solve_min_cost_assignment(
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
        solve_min_cost_assignment(
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

/// 単一の部隊編成（マルチセット）に対し、各役割をどの生産施設に配置するのが
/// 最適（ETAおよび移動距離の総和が最小）かを解き、SearchStateを構築する。
fn optimize_multiset_facility_assignment(
    input: &RollingPlanInput,
    multiset: &[ProductionRole],
    facilities: &[(GridPosition, Vec<usize>)],
    facility_role_options: &HashMap<(GridPosition, ProductionRole), usize>,
) -> Option<SearchState> {
    let m = multiset.len();
    let n = facilities.len();
    if m > n {
        return None;
    }

    let mut cost_matrix = Vec::with_capacity(m);
    for role in multiset {
        let mut row = Vec::with_capacity(n);
        for (fac_pos, _) in facilities {
            if let Some(&opt_idx) = facility_role_options.get(&(*fac_pos, *role)) {
                let opt = &input.production_options[opt_idx];
                let cost = calculate_assignment_cost(input, opt, opt_idx, *fac_pos);
                row.push(Some(cost));
            } else {
                row.push(None);
            }
        }
        if row.iter().all(|c| c.is_none()) {
            return None;
        }
        cost_matrix.push(row);
    }

    let mut best_cost = u64::MAX;
    let mut best_assignment = Vec::new();
    let mut current_assignment = Vec::with_capacity(m);

    solve_min_cost_assignment(
        0,
        0,
        0,
        &mut current_assignment,
        &mut best_cost,
        &mut best_assignment,
        &cost_matrix,
        n,
    );

    if best_cost == u64::MAX {
        return None;
    }

    let mut state = SearchState::default();
    for (role_i, &fac_idx) in best_assignment.iter().enumerate() {
        let fac_pos = facilities[fac_idx].0;
        let opt_idx = facility_role_options[&(fac_pos, multiset[role_i])];
        let option = &input.production_options[opt_idx];
        state.option_indices.push(opt_idx);
        state.used_slots.insert(ProductionSlot {
            facility: fac_pos,
            build_turn: 0,
        });
        if let Some(target) = option.capture_target {
            state.used_capture_targets.insert(target);
        }
        state.cost = state.cost.saturating_add(option.purchase.cost);
    }
    Some(state)
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
    // 厳密探索でも、同一施設・同一手番で全交戦対象・価格・被弾期待が劣る候補を
    // 列挙する必要はない。兵種名や固定点ではなく、入力盤面の相性表だけで支配関係を
    // 判定するため、残した候補の最適解は変わらない。
    let mut candidates_pruned = 0_usize;
    for option_indices in options_by_facility.values_mut() {
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
    let mut facilities = options_by_facility.into_iter().collect::<Vec<_>>();
    facilities.sort_unstable_by_key(|(facility, _)| (facility.y, facility.x));
    let state_limit = property_control_state_limit(facilities.len());

    if std::env::var_os("OPENWARS_ROLLING_AUDIT").is_some() {
        let capture_option_count = input
            .production_options
            .iter()
            .filter(|option| option.purchase.build_turn == 0 && option.capture_target.is_some())
            .count();
        let combat_option_count = input
            .production_options
            .iter()
            .filter(|option| option.purchase.build_turn == 0 && option.capture_target.is_none())
            .count();
        let capture_target_count = input
            .production_options
            .iter()
            .filter(|option| option.purchase.build_turn == 0)
            .filter_map(|option| option.capture_target)
            .collect::<HashSet<_>>()
            .len();
        eprintln!(
            "ROLLING_AUDIT exact_input facilities={} combat_options={} capture_options={} capture_targets={} funds={} enemies={}",
            facilities.len(),
            combat_option_count,
            capture_option_count,
            capture_target_count,
            input.current_funds,
            input.enemies.len(),
        );
    }

    let mut facility_role_options: HashMap<(GridPosition, ProductionRole), usize> = HashMap::new();
    let mut distinct_roles_set: HashSet<ProductionRole> = HashSet::new();

    for (facility, option_indices) in &facilities {
        for option_index in option_indices {
            let option = &input.production_options[*option_index];
            if option.stats.can_capture && option.capture_target.is_none() {
                // 物件レースにおいて占領可能ユニット（歩兵・工兵等）は必ず占領対象物件と紐付けて評価する。
                // 目標なき歩兵の乱造は、戦闘部隊（装甲車・戦車等）の配備枠を奪うため除外する。
                continue;
            }
            let role = if let Some(target) = option.capture_target {
                ProductionRole::Capture {
                    target,
                    unit_type: option.purchase.unit_type,
                    cost: option.purchase.cost,
                }
            } else {
                ProductionRole::Combat {
                    unit_type: option.purchase.unit_type,
                    cost: option.purchase.cost,
                }
            };
            facility_role_options.insert((*facility, role), *option_index);
            distinct_roles_set.insert(role);
        }
    }

    let mut distinct_roles: Vec<ProductionRole> = distinct_roles_set.into_iter().collect();
    distinct_roles.sort_by_key(|role| {
        (
            role.capture_target().is_none(),
            role.cost(),
            role.unit_type().as_str(),
            role.capture_target().map(|target| (target.y, target.x)),
        )
    });

    let (all_multisets, multiset_truncated) = generate_role_multisets(
        &distinct_roles,
        input,
        &facilities,
        &facility_role_options,
        input.current_funds,
        facilities.len(),
    );

    let mut states = vec![SearchState::default()];
    for multiset in all_multisets {
        if let Some(state) = optimize_multiset_facility_assignment(
            input,
            &multiset,
            &facilities,
            &facility_role_options,
        ) {
            states.push(state);
        }
    }

    let mut frontier_truncated = multiset_truncated;
    if states.len() > state_limit {
        retain_property_control_frontier(input, &mut states, state_limit);
        frontier_truncated = true;
    }

    // 通常の物件マップでは、少なくとも二つの異なる物件へ向かう占領役を作戦の
    // 下限にする。到達可能な物件または生産枠が二つ未満ならこの下限を課さないため、
    // 占領不能・到達不能の特殊マップをマップ名で例外扱いしない。
    let capture_targets = states
        .iter()
        .flat_map(|state| state.used_capture_targets.iter().copied())
        .collect::<HashSet<_>>();
    let available_slots = states
        .iter()
        .map(|state| state.used_slots.len())
        .max()
        .unwrap_or_default();
    let role_floor = (uses_small_map_opening_rules(input)
        && capture_targets.len() >= 2
        && available_slots >= 2
        && states
            .iter()
            .any(|state| state.used_capture_targets.len() >= 2))
    .then_some(2_usize);
    if let Some(required) = role_floor {
        states.retain(|state| state.used_capture_targets.len() >= required);
    }
    // 初手では、空いている工場で残額以内の合法なunitを購入できるなら、資金を翌手番へ
    // 繰り越す案を比較しない。途中状態の空き枠は高額案の組合せに必要なので、最終候補だけ
    // を除外する。資金不足または占領先の重複で購入不能な枠はそのまま残す。
    if uses_small_map_opening_rules(input) {
        states.retain(|state| opening_package_spends_all_usable_funds(input, state, &facilities));
    }

    let considered = states.len();
    // T6は物件到達・妨害の締切であり、戦闘そのものをT6で打ち切る期限ではない。
    // 締切判定はinterdiction_deadlinesに残したまま、弾薬と相性を通常の作戦期間まで
    // 実シミュレーションし、「期限は守るが別兵科を攻撃不能」な安価案を弾く。
    let combat_simulation_turns = search_turns.max(DEFAULT_SEARCH_TURNS);
    let evaluated = crate::ai::deterministic_parallel::map_ordered(states, |state| {
        simulate_state_with_catalog(input, catalog, &state, combat_simulation_turns)
    });
    if std::env::var_os("OPENWARS_ROLLING_AUDIT").is_some() {
        // 選択結果だけでは誤った比較軸を特定できないため、明示的な監査実行時だけ
        // 同じ辞書順で上位候補を表示する。通常対戦の探索量・選択結果には影響しない。
        let mut audit = evaluated.clone();
        audit.sort_by(|left, right| {
            if exact_property_plan_better(input, left, right) {
                std::cmp::Ordering::Less
            } else if exact_property_plan_better(input, right, left) {
                std::cmp::Ordering::Greater
            } else {
                std::cmp::Ordering::Equal
            }
        });
        eprintln!(
            "ROLLING_AUDIT context policy={:?} funds={} enemies={:?} protected={} options={} capture_options={}",
            input.opening_policy,
            input.current_funds,
            input
                .enemies
                .iter()
                .map(|enemy| (enemy.stats.unit_type, enemy.hp))
                .collect::<Vec<_>>(),
            input
                .existing_units
                .iter()
                .filter(|unit| unit.protection_completion_turn.is_some())
                .count()
                + input.protected_units.len(),
            input.production_options.len(),
            input
                .production_options
                .iter()
                .filter(|option| option.capture_target.is_some())
                .count(),
        );
        for (rank, plan) in audit.iter().take(10).enumerate() {
            eprintln!(
                "ROLLING_AUDIT rank={} purchases={:?} lanes={} joint={:?} breakthrough={:?} blocked={} destruction={:?} survivors={}/{} remaining_hp={} loss={} surviving_value={} cost={}",
                rank + 1,
                plan.purchases
                    .iter()
                    .map(|purchase| (
                        purchase.unit_type,
                        purchase.cost,
                        purchase.facility,
                        plan.capture_target_for(*purchase)
                    ))
                    .collect::<Vec<_>>(),
                plan.cleared_capture_lane_count(),
                plan.property_contract_completion_profile(),
                plan.front_breakthrough_rank(),
                plan.blocked_next_turn_production_slots,
                plan.target_destruction_profile(),
                plan.deadline_capture_survivor_count,
                plan.protected_unit_count,
                plan.remaining_hp(),
                plan.expected_loss,
                plan.surviving_combat_value,
                plan.production_cost,
            );
        }
        let mut seen_unit_types = HashSet::new();
        let unit_types = input
            .production_options
            .iter()
            .map(|option| option.stats.unit_type)
            .filter(|unit_type| seen_unit_types.insert(*unit_type))
            .collect::<Vec<_>>();
        for unit_type in unit_types {
            if let Some((rank, plan)) = audit.iter().enumerate().find(|(_, plan)| {
                plan.purchases
                    .iter()
                    .any(|purchase| purchase.unit_type == unit_type)
            }) {
                eprintln!(
                    "ROLLING_AUDIT type_best={:?} rank={} purchases={:?} lanes={} joint={:?} breakthrough={:?} targets={:?} survivors={}/{} remaining_hp={} loss={} surviving_value={} cost={}",
                    unit_type,
                    rank + 1,
                    plan.purchases
                        .iter()
                        .map(|purchase| purchase.unit_type)
                        .collect::<Vec<_>>(),
                    plan.cleared_capture_lane_count(),
                    plan.property_contract_completion_profile(),
                    plan.front_breakthrough_rank(),
                    plan.target_forecasts
                        .iter()
                        .map(|target| (
                            target.unit_type,
                            target.initial_hp,
                            target.remaining_hp,
                            target.destroyed_turn
                        ))
                        .collect::<Vec<_>>(),
                    plan.deadline_capture_survivor_count,
                    plan.protected_unit_count,
                    plan.remaining_hp(),
                    plan.expected_loss,
                    plan.surviving_combat_value,
                    plan.production_cost,
                );
            }
        }
        if let Some(plan) = audit.iter().find(|plan| {
            plan.purchases.len() == 4
                && plan
                    .purchases
                    .iter()
                    .filter(|purchase| purchase.unit_type == UnitType::Tank)
                    .count()
                    == 1
                && plan
                    .purchases
                    .iter()
                    .filter(|purchase| purchase.unit_type == UnitType::Recon)
                    .count()
                    == 1
                && plan
                    .purchases
                    .iter()
                    .filter(|purchase| purchase.unit_type == UnitType::Infantry)
                    .count()
                    == 2
        }) {
            eprintln!(
                "ROLLING_AUDIT ideal_shape purchases={:?} blocked={} destruction={:?} targets={:?} remaining_hp={} loss={} surviving_value={} cost={}",
                plan.purchases
                    .iter()
                    .map(|purchase| purchase.unit_type)
                    .collect::<Vec<_>>(),
                plan.blocked_next_turn_production_slots,
                plan.target_destruction_profile(),
                plan.target_forecasts
                    .iter()
                    .map(|target| (
                        target.unit_type,
                        target.initial_hp,
                        target.remaining_hp,
                        target.destroyed_turn,
                    ))
                    .collect::<Vec<_>>(),
                plan.remaining_hp(),
                plan.expected_loss,
                plan.surviving_combat_value,
                plan.production_cost,
            );
        }
        for (label, wanted) in [
            (
                "tank_recon_infantry",
                [UnitType::Tank, UnitType::Recon, UnitType::Infantry],
            ),
            (
                "tank_recon_mech",
                [UnitType::Tank, UnitType::Recon, UnitType::Mech],
            ),
        ] {
            if let Some(plan) = audit.iter().find(|plan| {
                plan.purchases.len() == wanted.len()
                    && wanted.iter().all(|unit_type| {
                        plan.purchases
                            .iter()
                            .filter(|purchase| purchase.unit_type == *unit_type)
                            .count()
                            == wanted
                                .iter()
                                .filter(|wanted_type| *wanted_type == unit_type)
                                .count()
                    })
            }) {
                eprintln!(
                    "ROLLING_AUDIT shape={label} purchases={:?} lanes={} joint={:?} breakthrough={:?} blocked={} destruction={:?} remaining_value={} remaining_hp={} loss={} surviving_value={} cost={}",
                    plan.purchases
                        .iter()
                        .map(|purchase| purchase.unit_type)
                        .collect::<Vec<_>>(),
                    plan.cleared_capture_lane_count(),
                    plan.property_contract_completion_profile(),
                    plan.front_breakthrough_rank(),
                    plan.blocked_next_turn_production_slots,
                    plan.target_destruction_profile(),
                    plan.remaining_enemy_value(input),
                    plan.remaining_hp(),
                    plan.expected_loss,
                    plan.surviving_combat_value,
                    plan.production_cost,
                );
            }
        }
    }
    // まず直接部隊だけで、物件・突破・生産口を守る最良案をシミュレーションから選ぶ。
    // 間接部隊はこの基準案を悪化させず、実際に残敵を減らせる時だけ後から置き換える。
    // これにより密度や総額を入口条件にして、砲撃だけで直接部隊の任務を上書きしない。
    let mut selected = select_best_property_plan(
        input,
        evaluated
            .iter()
            .filter(|plan| !plan.has_indirect_purchase(input)),
    )
    .or_else(|| select_best_property_plan(input, evaluated.iter()))?;
    if let Some(indirect_upgrade) = select_best_indirect_upgrade(
        input,
        evaluated.iter().filter(|candidate| {
            candidate.has_indirect_purchase(input)
                && indirect_upgrade_preserves_direct_plan(input, candidate, &selected)
        }),
    ) {
        selected = indirect_upgrade;
    }
    selected.candidates_considered = considered;
    selected.candidates_pruned = candidates_pruned;
    selected.search_truncated = frontier_truncated;
    Some(selected)
}

/// 全列挙できない物件制御だけで使う、現在盤面に基づく候補前線。
///
/// この段階は最終評価を代替しない。占領数の不足、各敵へ届く実ダメージ、占領完了ETA
/// が劣る中間案から先に落とし、残った候補だけを従来どおり完全な戦闘シミュレーションで
/// 比較する。従って小規模で候補数が上限未満の盤面（map_1を含む）は一切変わらない。
fn retain_property_control_frontier(
    input: &RollingPlanInput,
    states: &mut Vec<SearchState>,
    state_limit: usize,
) {
    states.sort_unstable_by_key(|state| property_control_frontier_key(input, state));
    if states.len() <= state_limit {
        return;
    }

    // 上位256件をそのまま切ると、安価な直接火力だけが残り、物件到着前に敵歩兵を
    // 止められる快速screenのような別作戦が消える。まず盤面から得た到達profileごとに
    // 最良の一案を残し、その後だけ同profile内の順位で予算を埋める。
    let tactical_profiles = states
        .iter()
        .map(|state| property_control_tactical_profile(input, state))
        .collect::<BTreeSet<_>>();
    if tactical_profiles.len() > state_limit {
        // profile自体が予算を超える極端な盤面では、全profileを実simできない。
        // この場合だけ従来の決定順を使う。通常の多工場初手ではprofile数は十分小さい。
        states.truncate(state_limit);
        return;
    }

    let mut retained_profiles = BTreeSet::new();
    let mut retained_states = BTreeSet::new();
    let mut retained = Vec::with_capacity(state_limit);
    for state in states.iter() {
        let profile = property_control_tactical_profile(input, state);
        if retained_profiles.insert(profile) {
            retained_states.insert(state.option_indices.clone());
            retained.push(state.clone());
        }
    }
    for state in states.iter() {
        if retained.len() == state_limit {
            break;
        }
        let profile = property_control_tactical_profile(input, state);
        // 同profileの追加案も最終simに渡すが、代表案を先に確保した後に限る。
        if retained_profiles.contains(&profile)
            && retained_states.insert(state.option_indices.clone())
        {
            retained.push(state.clone());
        }
    }
    *states = retained;
}

/// 物件到達と敵歩兵への初撃を、候補削減前に失わないための戦術profile。
///
/// 固定の兵種評価ではなく、各候補が各観測敵へ何turn目に何ダメージを届けられるかを
/// 用いる。同じprofile内では従来の物件・損耗比較が代表を選ぶので、profileは最終評価を
/// 置き換えない。
fn property_control_tactical_profile(
    input: &RollingPlanInput,
    state: &SearchState,
) -> (Vec<u32>, usize, Vec<(u32, u32)>) {
    let mut capture_completion_turns = state
        .option_indices
        .iter()
        .filter_map(|index| input.production_options[*index].capture_completion_turn)
        .collect::<Vec<_>>();
    capture_completion_turns.sort_unstable();
    let mut enemy_profiles = Vec::with_capacity(input.enemies.len());
    for (enemy_index, enemy) in input.enemies.iter().enumerate() {
        let mut earliest_turn = u32::MAX;
        let mut damage_at_earliest = 0_u32;
        for option_index in &state.option_indices {
            let option = &input.production_options[*option_index];
            let ready_turn = input
                .production_attack_projections
                .get(*option_index)
                .and_then(|projections| projections.get(enemy_index))
                .and_then(|projection| *projection)
                .map_or_else(
                    || {
                        option
                            .engageable_enemy_indices
                            .contains(&enemy_index)
                            .then_some(0)
                    },
                    |projection| Some(projection.ready_turn),
                );
            let Some(ready_turn) = ready_turn else {
                continue;
            };
            let damage = best_damage(
                &input.damage_chart,
                option.stats.unit_type,
                enemy.stats.unit_type,
            );
            if ready_turn < earliest_turn {
                earliest_turn = ready_turn;
                damage_at_earliest = damage;
            } else if ready_turn == earliest_turn {
                damage_at_earliest = damage_at_earliest.saturating_add(damage);
            }
        }
        enemy_profiles.push((earliest_turn, damage_at_earliest));
    }
    (
        capture_completion_turns,
        state.used_capture_targets.len(),
        enemy_profiles,
    )
}

/// 多数工場のマルチセット組合せが極端に膨張した場合だけ、候補前線へ縮約する安全弁。
///
/// 施設の順列爆発を排除したため通常の盤面では上限に達しない。特定mapや施設数への依存を排除。
fn property_control_state_limit(_facility_count: usize) -> usize {
    MANY_FACTORY_PROPERTY_STATE_LIMIT
}

/// 現在手番の中間生産列を、物件契約と敵編成に対する到達性だけで順序付ける。
///
/// まだ戦闘順・反撃・渋滞を確定しないため、その評価は最終シミュレーションに残す。
/// ここで見るダメージは「どの敵へ何も届かない候補か」を判別する下限であり、
/// 高火力unitを固定的に優遇する評価値ではない。
fn property_control_frontier_key(
    input: &RollingPlanInput,
    state: &SearchState,
) -> PropertyFrontierKey {
    let capture_count = state.used_capture_targets.len();
    let capture_completion_turns = state
        .option_indices
        .iter()
        .filter_map(|index| input.production_options[*index].capture_completion_turn)
        .collect::<Vec<_>>();

    let mut damage_by_enemy = vec![0_u32; input.enemies.len()];
    let mut combat_unit_count = 0_usize;
    let mut total_movement = 0_u32;

    for &option_index in &state.option_indices {
        let option = &input.production_options[option_index];
        total_movement = total_movement.saturating_add(option.stats.max_movement);
        if option.capture_target.is_none() && option.stats.movement_type != MovementType::Infantry {
            combat_unit_count += 1;
        }

        for (enemy_index, enemy) in input.enemies.iter().enumerate() {
            let reaches_enemy = input
                .production_attack_projections
                .get(option_index)
                .map_or_else(
                    || option.engageable_enemy_indices.contains(&enemy_index),
                    |projections| projections.get(enemy_index).is_some_and(Option::is_some),
                );
            if reaches_enemy {
                damage_by_enemy[enemy_index] =
                    damage_by_enemy[enemy_index].saturating_add(best_damage(
                        &input.damage_chart,
                        option.stats.unit_type,
                        enemy.stats.unit_type,
                    ));
            }
        }
    }

    build_property_frontier_key(
        input,
        capture_count,
        capture_completion_turns,
        combat_unit_count,
        total_movement,
        &damage_by_enemy,
        state.cost,
        state.option_indices.clone(),
    )
}

/// 既にシミュレーション済みの候補から、物件計画の通常比較で最良の一案を選ぶ。
fn select_best_property_plan<'a>(
    input: &RollingPlanInput,
    candidates: impl Iterator<Item = &'a ForcePackagePlan>,
) -> Option<ForcePackagePlan> {
    candidates.cloned().reduce(|selected, candidate| {
        if exact_property_plan_better(input, &candidate, &selected) {
            candidate
        } else {
            selected
        }
    })
}

/// 直接案を壊さない間接案の中から、シミュレーション比較で最良の一案を選ぶ。
/// 同値の場合だけ、実際に直接射点が密な側を優先する。密度だけで候補を捨てない。
fn select_best_indirect_upgrade<'a>(
    input: &RollingPlanInput,
    candidates: impl Iterator<Item = &'a ForcePackagePlan>,
) -> Option<ForcePackagePlan> {
    candidates.cloned().reduce(|selected, candidate| {
        if exact_property_plan_better(input, &candidate, &selected) {
            candidate
        } else if exact_property_plan_better(input, &selected, &candidate) {
            selected
        } else if candidate.indirect_fire_has_local_support(input)
            && !selected.indirect_fire_has_local_support(input)
        {
            candidate
        } else {
            selected
        }
    })
}

/// 間接部隊を足した案が、直接部隊だけの基準作戦を崩していないか。
///
/// 物件契約、突破口、生産口、占領役、または敵別のユニークな直接射点を一つでも
/// 悪化させる案は火力が高くても採らない。直接unitの頭数ではなく射点を比較するため、
/// 同じ地点へ向かう余剰unitだけを間接火力へ置き換えられる。
fn indirect_upgrade_preserves_direct_plan(
    input: &RollingPlanInput,
    candidate: &ForcePackagePlan,
    direct_plan: &ForcePackagePlan,
) -> bool {
    let (candidate_capture_count, candidate_direct_count) =
        current_direct_role_counts(input, candidate);
    let (direct_capture_count, direct_direct_count) =
        current_direct_role_counts(input, direct_plan);
    let preserves_current_direct_roles = if uses_small_map_tactical_rules(input) {
        // 小規模mapでは、前線の壁役（直接射点）が完全に消滅しない限り、
        // 狭隘地形（橋など）の後方から撃てる間接部隊への置き換えを認める。
        candidate.direct_fire_lane_count > 0 || direct_plan.direct_fire_lane_count == 0
    } else {
        // 標準mapは既存の汎用編成比較をそのまま残す。
        candidate_direct_count >= direct_direct_count
    };
    candidate.cleared_capture_lane_count() >= direct_plan.cleared_capture_lane_count()
        && candidate_capture_count >= direct_capture_count
        && preserves_current_direct_roles
        && candidate.property_contract_completion_profile()
            <= direct_plan.property_contract_completion_profile()
        && candidate.deadline_capture_survivor_count >= direct_plan.deadline_capture_survivor_count
        && candidate.capture_completion_profile() <= direct_plan.capture_completion_profile()
        && candidate.front_breakthrough_rank() <= direct_plan.front_breakthrough_rank()
        && candidate.blocked_next_turn_production_slots
            <= direct_plan.blocked_next_turn_production_slots
        && (!uses_small_map_tactical_rules(input)
            || (candidate.indirect_fire_has_marginal_target()
                || candidate.target_elimination_profile()
                    < direct_plan.target_elimination_profile()))
        && (candidate.remaining_enemy_value(input) < direct_plan.remaining_enemy_value(input)
            || candidate.expected_loss < direct_plan.expected_loss)
}

impl ForcePackagePlan {
    /// 各敵の撃破予測を、未撃破を無限大として比較可能な形にする。
    fn target_elimination_profile(&self) -> Vec<u32> {
        self.target_forecasts
            .iter()
            .map(|target| target.destroyed_turn.unwrap_or(u32::MAX))
            .collect()
    }

    /// 今回買う間接部隊が、既存の間接部隊と直接部隊だけでは倒せない敵へ
    /// 実ダメージを足せているか。射撃対象の重複を「unit数」ではなく敵HPで判定する。
    fn indirect_fire_has_marginal_target(&self) -> bool {
        self.target_forecasts
            .iter()
            .enumerate()
            .any(|(index, target)| {
                let produced = self
                    .produced_indirect_damage_by_target
                    .get(index)
                    .copied()
                    .unwrap_or_default();
                let prior_damage = self
                    .direct_damage_by_target
                    .get(index)
                    .copied()
                    .unwrap_or_default()
                    .saturating_add(
                        self.existing_indirect_damage_by_target
                            .get(index)
                            .copied()
                            .unwrap_or_default(),
                    );
                produced > 0 && prior_damage < target.initial_hp
            })
    }
}

/// 今手番に発注する占領役と、占領を担当しない直接戦闘役の本数。
/// 標準mapでは従来通りこの二つを下限にし、小規模mapだけ射点比較へ切り替える。
fn current_direct_role_counts(input: &RollingPlanInput, plan: &ForcePackagePlan) -> (usize, usize) {
    plan.current_purchases()
        .fold((0, 0), |(capture, direct), purchase| {
            let Some(option) = input
                .production_options
                .iter()
                .find(|option| option.purchase == purchase)
            else {
                return (capture, direct);
            };
            if option.capture_target.is_some() {
                (capture.saturating_add(1), direct)
            } else if option.stats.min_range <= 1 {
                (capture, direct.saturating_add(1))
            } else {
                (capture, direct)
            }
        })
}

/// 今手番に生産する直接部隊が、実シミュレーションで担当した敵ごとのユニークな射点数。
/// 同じ敵・同じ射点へ何体も向かう案は一つと数える。候補ごとのsimulateの終端で一度だけ
/// 計算し、候補比較中には再計算しない。
fn direct_fire_lane_count(
    input: &RollingPlanInput,
    combat_purchases: &[PlannedCombatPurchase],
) -> usize {
    combat_purchases
        .iter()
        .filter_map(|assignment| {
            let option = input
                .production_options
                .iter()
                .find(|option| option.purchase == assignment.purchase)?;
            if assignment.purchase.build_turn != 0 || option.stats.min_range > 1 {
                return None;
            }
            let enemy_index = input
                .enemies
                .iter()
                .position(|enemy| enemy.entity == Some(assignment.target))?;
            let projection = input
                .production_attack_projections
                .get(
                    input
                        .production_options
                        .iter()
                        .position(|candidate| candidate.purchase == assignment.purchase)?,
                )?
                .get(enemy_index)
                .and_then(|projection| *projection)?;
            Some((enemy_index, projection.firing_position))
        })
        .collect::<HashSet<_>>()
        .len()
}

/// 直接部隊だけで敵直接部隊をどこまで駆逐できるかを、実際の相性・到着turn・弾数から
/// 割り当てる。砲を足した候補で直接部隊の攻撃先が変わっても、直接戦線を維持する能力まで
/// 失ったことにはしないため、通常simulationの実行結果とは別に不変の能力として計算する。
fn direct_enemy_remaining_hp_after_assignment(
    friendlies: &[SimFriendly],
    enemies: &[SimEnemy],
    search_turns: u32,
) -> Vec<u32> {
    let mut remaining_hp = enemies
        .iter()
        .map(|enemy| {
            if enemy.source.stats.min_range <= 1 && enemy.source.available_turn <= search_turns {
                enemy.source.hp
            } else {
                0
            }
        })
        .collect::<Vec<_>>();
    // unitごとの攻撃回数を、弾数と実際に攻撃可能なturn数の小さい方に制限する。
    // これで、同じ戦車一両を複数の敵へ無限に割り当てる過大評価を避ける。
    let mut remaining_shots = friendlies
        .iter()
        .map(|friendly| {
            if friendly.stats.min_range > 1 || friendly.available_turn > search_turns {
                0
            } else {
                friendly.attacks_left.min(
                    search_turns
                        .saturating_sub(friendly.available_turn)
                        .saturating_add(1),
                )
            }
        })
        .collect::<Vec<_>>();
    let mut target_order = remaining_hp
        .iter()
        .enumerate()
        .filter(|(_, hp)| **hp > 0)
        .map(|(enemy_index, hp)| {
            let eligible = friendlies
                .iter()
                .filter(|friendly| friendly.stats.min_range <= 1)
                .filter(|friendly| {
                    friendly
                        .attack_profiles
                        .get(enemy_index)
                        .copied()
                        .flatten()
                        .is_some_and(|profile| profile.ready_turn <= search_turns)
                })
                .count();
            (eligible, std::cmp::Reverse(*hp), enemy_index)
        })
        .collect::<Vec<_>>();
    // 対応できる直接兵種が少ない敵から先に担当を確保する。これにより歩兵・装甲車・
    // 戦車の相性差を固定値にせず、実damage表が必要とする担当を残せる。
    target_order.sort_unstable();
    for (_, _, enemy_index) in target_order {
        while remaining_hp[enemy_index] > 0 {
            let best = friendlies
                .iter()
                .enumerate()
                .filter(|(friendly_index, friendly)| {
                    remaining_shots[*friendly_index] > 0 && friendly.stats.min_range <= 1
                })
                .filter_map(|(friendly_index, friendly)| {
                    let profile = friendly
                        .attack_profiles
                        .get(enemy_index)
                        .copied()
                        .flatten()?;
                    (profile.ready_turn <= search_turns).then(|| {
                        let damage = calculate_damage_formula(
                            profile.base_damage,
                            friendly.hp,
                            enemies[enemy_index].source.defense_bonus,
                            false,
                        );
                        (damage > 0).then_some((
                            damage,
                            std::cmp::Reverse(profile.ready_turn),
                            std::cmp::Reverse(friendly_index),
                        ))
                    })?
                })
                .max();
            let Some((damage, _, std::cmp::Reverse(friendly_index))) = best else {
                break;
            };
            remaining_shots[friendly_index] = remaining_shots[friendly_index].saturating_sub(1);
            remaining_hp[enemy_index] = remaining_hp[enemy_index].saturating_sub(damage);
        }
    }
    remaining_hp
}

/// 同じ敵へ向かう直接部隊の射点が飽和し、間接部隊が工場外から別の攻撃地点を
/// 足せるかを判定する。
///
/// 戦力額や兵種ごとの固定点は使わない。既存・今回生産の直接部隊が、間接部隊の
/// 初撃までに実際に立てる射点だけを数える。隣接射点をすべて直接部隊で埋める必要は
/// なく、「空きが一つ以下」なら新たな直接部隊より外側から撃つ間接火力の限界効用が
/// 高い、とゲーム上の攻撃地点の数から判断する。
fn direct_fire_saturates_target(
    input: &RollingPlanInput,
    plan: &ForcePackagePlan,
    enemy_index: usize,
    indirect_ready_turn: u32,
) -> bool {
    let Some(enemy) = input.enemies.get(enemy_index) else {
        return false;
    };
    let target = enemy.position;
    let adjacent_capacity = input.map.get_adjacent(target.x, target.y).len();
    if adjacent_capacity == 0 {
        return false;
    }

    let mut direct_positions = input
        .existing_units
        .iter()
        .filter(|unit| {
            unit.stats.min_range <= 1
                && unit.available_turn <= indirect_ready_turn
                && unit.engageable_enemy_indices.contains(&enemy_index)
        })
        .map(|unit| unit.position)
        .filter(|position| {
            input
                .map
                .distance(position.x, position.y, target.x, target.y)
                <= 1
        })
        .collect::<HashSet<_>>();

    for purchase in &plan.purchases {
        let Some(option_index) = input
            .production_options
            .iter()
            .position(|option| option.purchase == *purchase)
        else {
            continue;
        };
        let option = &input.production_options[option_index];
        // 占領役も、物件を塞ぐ敵へ攻撃する間は直接射点を一つ使う。占領契約だからと
        // 数から外すと、中央で実際に起きている渋滞を見落として砲撃が遅くなる。
        if option.stats.min_range > 1 {
            continue;
        }
        let Some(projection) = input
            .production_attack_projections
            .get(option_index)
            .and_then(|projections| projections.get(enemy_index))
            .and_then(|projection| *projection)
        else {
            continue;
        };
        if input.map.distance(
            projection.firing_position.x,
            projection.firing_position.y,
            target.x,
            target.y,
        ) <= 1
        {
            direct_positions.insert(projection.firing_position);
        }
    }

    direct_positions.len().saturating_add(1) >= adjacent_capacity
}

/// 小規模開幕専用の制約を適用する条件を一箇所に固定する。
fn uses_small_map_opening_rules(input: &RollingPlanInput) -> bool {
    input.current_turn == 1 && uses_small_map_tactical_rules(input)
}

/// 戦略プロファイルは盤面構造から対局中に一度だけ決まる。初手以外にも効かせる
/// 小規模マップ専用の判断はここへ集約し、標準マップのRolling Planを変えない。
fn uses_small_map_tactical_rules(input: &RollingPlanInput) -> bool {
    input.opening_policy == OpeningProductionPolicy::SmallMapExpansion
}

/// 初手の最終編成が、残額で合法に追加できる生産枠を放置していないか判定する。
fn opening_package_spends_all_usable_funds(
    input: &RollingPlanInput,
    state: &SearchState,
    facilities: &[(GridPosition, Vec<usize>)],
) -> bool {
    let remaining_funds = input.current_funds.saturating_sub(state.cost);
    let has_combat_unit = state.option_indices.iter().any(|&idx| {
        let opt = &input.production_options[idx];
        opt.capture_target.is_none() && opt.stats.movement_type != MovementType::Infantry
    });
    let capture_count = state.used_capture_targets.len();
    let capture_satisfied = capture_count >= input.required_capture_survivors;

    !facilities.iter().any(|(facility, option_indices)| {
        let slot = ProductionSlot {
            facility: *facility,
            build_turn: 0,
        };
        !state.used_slots.contains(&slot)
            && option_indices.iter().any(|option_index| {
                let option = &input.production_options[*option_index];
                if option.purchase.cost > remaining_funds {
                    return false;
                }
                if let Some(target) = option.capture_target {
                    !state.used_capture_targets.contains(&target)
                } else {
                    // 戦闘ユニット・占領役ともに確保済みなら、端数資金での追加歩兵の買い足しを強制しない
                    !(has_combat_unit
                        && capture_satisfied
                        && option.stats.movement_type == MovementType::Infantry)
                }
            })
    })
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
            if uses_small_map_tactical_rules(input) {
                // 同じ占領契約を満たすなら、敵占領役へ最初に触れられるturnを先に比べる。
                // これは兵種の固定優先ではなく、実際の射撃位置・移動後攻撃可否から得た
                // first_attack_turnであり、初撃が遅い重歩兵を火力合計だけで選ばないための
                // 順序である。
                let candidate_first_attack = candidate.first_attack_turn.unwrap_or(u32::MAX);
                let current_first_attack = current.first_attack_turn.unwrap_or(u32::MAX);
                if candidate_first_attack != current_first_attack {
                    return candidate_first_attack < current_first_attack;
                }
            }
            if !uses_small_map_tactical_rules(input) && input.required_capture_survivors <= 1 {
                // 標準mapでは、同じ物件契約を成立させた後の早過ぎる掃討のために
                // 追加unitを買わない。次手番に盤面を再観測できるため、費用を先に比べる。
                return (
                    std::cmp::Reverse(candidate.cleared_capture_lane_count()),
                    candidate.property_contract_completion_profile(),
                    std::cmp::Reverse(candidate.deadline_capture_survivor_count),
                    candidate.capture_completion_profile(),
                    candidate.expected_loss,
                    candidate.production_cost,
                    candidate.target_destruction_profile(),
                    candidate.occupation_turn,
                    std::cmp::Reverse(candidate.surviving_combat_value),
                ) < (
                    std::cmp::Reverse(current.cleared_capture_lane_count()),
                    current.property_contract_completion_profile(),
                    std::cmp::Reverse(current.deadline_capture_survivor_count),
                    current.capture_completion_profile(),
                    current.expected_loss,
                    current.production_cost,
                    current.target_destruction_profile(),
                    current.occupation_turn,
                    std::cmp::Reverse(current.surviving_combat_value),
                );
            }
            if uses_small_map_tactical_rules(input) {
                // 小規模マップでは、同じ占領契約を満たすなら、前線に展開した戦闘戦力価値
                // (surviving_combat_value) を優先する。ただ最安歩兵で費用を抑えるだけの
                // 鈍足編成を選ばず、快速screenや防衛火力を前線へ確保する。
                return (
                    std::cmp::Reverse(candidate.cleared_capture_lane_count()),
                    candidate.property_contract_completion_profile(),
                    std::cmp::Reverse(candidate.deadline_capture_survivor_count),
                    candidate.capture_completion_profile(),
                    candidate.target_destruction_profile(),
                    candidate.occupation_turn,
                    candidate.expected_loss,
                    std::cmp::Reverse(candidate.surviving_combat_value),
                    candidate.production_cost,
                ) < (
                    std::cmp::Reverse(current.cleared_capture_lane_count()),
                    current.property_contract_completion_profile(),
                    std::cmp::Reverse(current.deadline_capture_survivor_count),
                    current.capture_completion_profile(),
                    current.target_destruction_profile(),
                    current.occupation_turn,
                    current.expected_loss,
                    std::cmp::Reverse(current.surviving_combat_value),
                    current.production_cost,
                );
            }
            return (
                std::cmp::Reverse(candidate.cleared_capture_lane_count()),
                candidate.property_contract_completion_profile(),
                std::cmp::Reverse(candidate.deadline_capture_survivor_count),
                candidate.capture_completion_profile(),
                candidate.target_destruction_profile(),
                candidate.occupation_turn,
                candidate.expected_loss,
                candidate.production_cost,
                std::cmp::Reverse(candidate.surviving_combat_value),
            ) < (
                std::cmp::Reverse(current.cleared_capture_lane_count()),
                current.property_contract_completion_profile(),
                std::cmp::Reverse(current.deadline_capture_survivor_count),
                current.capture_completion_profile(),
                current.target_destruction_profile(),
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
        // 掃討と占領が対になった成立レーン数を最優先し、その後に全レーンの掃討時刻、
        // 占領時刻、敵残HP、損耗を比較する。一体目だけの突破で後続を見失わない。
        if uses_small_map_tactical_rules(input) {
            return (
                std::cmp::Reverse(candidate.cleared_capture_lane_count()),
                candidate.property_contract_completion_profile(),
                (
                    candidate.front_breakthrough_rank(),
                    candidate.target_destruction_profile(),
                    candidate.remaining_enemy_value(input),
                    std::cmp::Reverse(candidate.surviving_combat_value),
                    candidate.blocked_next_turn_production_slots,
                    candidate.expected_loss,
                    std::cmp::Reverse(candidate.deadline_capture_survivor_count),
                    candidate.capture_completion_profile(),
                    candidate.remaining_hp(),
                    candidate.production_cost,
                ),
            ) < (
                std::cmp::Reverse(current.cleared_capture_lane_count()),
                current.property_contract_completion_profile(),
                (
                    current.front_breakthrough_rank(),
                    current.target_destruction_profile(),
                    current.remaining_enemy_value(input),
                    std::cmp::Reverse(current.surviving_combat_value),
                    current.blocked_next_turn_production_slots,
                    current.expected_loss,
                    std::cmp::Reverse(current.deadline_capture_survivor_count),
                    current.capture_completion_profile(),
                    current.remaining_hp(),
                    current.production_cost,
                ),
            );
        }
        return (
            std::cmp::Reverse(candidate.cleared_capture_lane_count()),
            candidate.property_contract_completion_profile(),
            (
                candidate.front_breakthrough_rank(),
                candidate.remaining_enemy_value(input),
                candidate.blocked_next_turn_production_slots,
                candidate.expected_loss,
                std::cmp::Reverse(candidate.surviving_combat_value),
                candidate.target_destruction_profile(),
                std::cmp::Reverse(candidate.deadline_capture_survivor_count),
                candidate.capture_completion_profile(),
                candidate.remaining_hp(),
                candidate.production_cost,
            ),
        ) < (
            std::cmp::Reverse(current.cleared_capture_lane_count()),
            current.property_contract_completion_profile(),
            (
                current.front_breakthrough_rank(),
                current.remaining_enemy_value(input),
                current.blocked_next_turn_production_slots,
                current.expected_loss,
                std::cmp::Reverse(current.surviving_combat_value),
                current.target_destruction_profile(),
                std::cmp::Reverse(current.deadline_capture_survivor_count),
                current.capture_completion_profile(),
                current.remaining_hp(),
                current.production_cost,
            ),
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
        if candidate.cleared_capture_lane_count() != current.cleared_capture_lane_count() {
            return candidate.cleared_capture_lane_count() > current.cleared_capture_lane_count();
        }
        let candidate_joint = candidate.property_contract_completion_profile();
        let current_joint = current.property_contract_completion_profile();
        if candidate_joint != current_joint {
            return candidate_joint < current_joint;
        }
        if candidate.feasible && current.feasible {
            if !uses_small_map_tactical_rules(input) && input.required_capture_survivors <= 1 {
                // 期限契約を同じく満たせるなら、標準mapは余剰の直接・航空戦力を
                // 増やさず、次手番の観測後に必要なcounterへ資金を残す。
                return (
                    candidate.remaining_enemy_value(input),
                    candidate.blocked_next_turn_production_slots,
                    candidate.expected_loss,
                    candidate.production_cost,
                    candidate.target_destruction_profile(),
                    std::cmp::Reverse(candidate.surviving_combat_value),
                    candidate
                        .interdiction_first_attack_turn()
                        .unwrap_or(u32::MAX),
                ) < (
                    current.remaining_enemy_value(input),
                    current.blocked_next_turn_production_slots,
                    current.expected_loss,
                    current.production_cost,
                    current.target_destruction_profile(),
                    std::cmp::Reverse(current.surviving_combat_value),
                    current.interdiction_first_attack_turn().unwrap_or(u32::MAX),
                );
            }
            return (
                candidate.remaining_enemy_value(input),
                candidate.blocked_next_turn_production_slots,
                candidate.expected_loss,
                std::cmp::Reverse(candidate.surviving_combat_value),
                candidate.target_destruction_profile(),
                candidate.production_cost,
                candidate
                    .interdiction_first_attack_turn()
                    .unwrap_or(u32::MAX),
            ) < (
                current.remaining_enemy_value(input),
                current.blocked_next_turn_production_slots,
                current.expected_loss,
                std::cmp::Reverse(current.surviving_combat_value),
                current.target_destruction_profile(),
                current.production_cost,
                current.interdiction_first_attack_turn().unwrap_or(u32::MAX),
            );
        }
        if uses_small_map_tactical_rules(input) {
            return (
                candidate.target_destruction_profile(),
                candidate.remaining_enemy_value(input),
                candidate.blocked_next_turn_production_slots,
                candidate.expected_loss,
                std::cmp::Reverse(candidate.surviving_combat_value),
                candidate
                    .interdiction_first_attack_turn()
                    .unwrap_or(u32::MAX),
                candidate.production_cost,
            ) < (
                current.target_destruction_profile(),
                current.remaining_enemy_value(input),
                current.blocked_next_turn_production_slots,
                current.expected_loss,
                std::cmp::Reverse(current.surviving_combat_value),
                current.interdiction_first_attack_turn().unwrap_or(u32::MAX),
                current.production_cost,
            );
        }
        // 契約を満たした案同士では、敵のマスター価格に残HPを掛けた実残存価値を減らす。
        // raw HPや購入兵種への固定点ではなく、実盤面の敵編成と相性による撃破結果を使う。
        return (
            candidate.remaining_enemy_value(input),
            candidate.blocked_next_turn_production_slots,
            candidate.expected_loss,
            std::cmp::Reverse(candidate.surviving_combat_value),
            candidate.target_destruction_profile(),
            candidate
                .interdiction_first_attack_turn()
                .unwrap_or(u32::MAX),
            candidate.production_cost,
        ) < (
            current.remaining_enemy_value(input),
            current.blocked_next_turn_production_slots,
            current.expected_loss,
            std::cmp::Reverse(current.surviving_combat_value),
            current.target_destruction_profile(),
            current.interdiction_first_attack_turn().unwrap_or(u32::MAX),
            current.production_cost,
        );
    }

    let required = input.required_capture_survivors;
    if uses_small_map_tactical_rules(input) {
        return (
            std::cmp::Reverse(candidate.cleared_capture_lane_count()),
            candidate.property_contract_completion_profile(),
            required.saturating_sub(
                candidate.completed_property_contracts(&input.interdiction_deadlines, required),
            ),
            candidate.interdiction_deadline_shortfall_profile(&input.interdiction_deadlines),
            candidate.capture_survivor_shortfall(required),
            (
                candidate.front_breakthrough_rank(),
                candidate.target_destruction_profile(),
                candidate.remaining_enemy_value(input),
                std::cmp::Reverse(candidate.surviving_combat_value),
                candidate.blocked_next_turn_production_slots,
                candidate.expected_loss,
            ),
            (
                candidate.interdiction_attack_turn_profile(&input.interdiction_deadlines),
                candidate.remaining_hp(),
                candidate.production_cost,
            ),
        ) < (
            std::cmp::Reverse(current.cleared_capture_lane_count()),
            current.property_contract_completion_profile(),
            required.saturating_sub(
                current.completed_property_contracts(&input.interdiction_deadlines, required),
            ),
            current.interdiction_deadline_shortfall_profile(&input.interdiction_deadlines),
            current.capture_survivor_shortfall(required),
            (
                current.front_breakthrough_rank(),
                current.target_destruction_profile(),
                current.remaining_enemy_value(input),
                std::cmp::Reverse(current.surviving_combat_value),
                current.blocked_next_turn_production_slots,
                current.expected_loss,
            ),
            (
                current.interdiction_attack_turn_profile(&input.interdiction_deadlines),
                current.remaining_hp(),
                current.production_cost,
            ),
        );
    }
    (
        std::cmp::Reverse(candidate.cleared_capture_lane_count()),
        candidate.property_contract_completion_profile(),
        required.saturating_sub(
            candidate.completed_property_contracts(&input.interdiction_deadlines, required),
        ),
        candidate.interdiction_deadline_shortfall_profile(&input.interdiction_deadlines),
        candidate.capture_survivor_shortfall(required),
        (
            candidate.remaining_enemy_value(input),
            candidate.blocked_next_turn_production_slots,
            candidate.expected_loss,
            std::cmp::Reverse(candidate.surviving_combat_value),
            candidate.front_breakthrough_rank(),
            candidate.target_destruction_profile(),
        ),
        (
            candidate.interdiction_attack_turn_profile(&input.interdiction_deadlines),
            candidate.remaining_hp(),
            candidate.production_cost,
        ),
    ) < (
        std::cmp::Reverse(current.cleared_capture_lane_count()),
        current.property_contract_completion_profile(),
        required.saturating_sub(
            current.completed_property_contracts(&input.interdiction_deadlines, required),
        ),
        current.interdiction_deadline_shortfall_profile(&input.interdiction_deadlines),
        current.capture_survivor_shortfall(required),
        (
            current.remaining_enemy_value(input),
            current.blocked_next_turn_production_slots,
            current.expected_loss,
            std::cmp::Reverse(current.surviving_combat_value),
            current.front_breakthrough_rank(),
            current.target_destruction_profile(),
        ),
        (
            current.interdiction_attack_turn_profile(&input.interdiction_deadlines),
            current.remaining_hp(),
            current.production_cost,
        ),
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
    input: &RollingPlanInput,
    best: &mut Option<ForcePackagePlan>,
    candidate: &ForcePackagePlan,
    delay_cost_per_turn: u32,
) {
    let small_map_first_attack = if uses_small_map_tactical_rules(input) {
        candidate.first_attack_turn.unwrap_or(u32::MAX)
    } else {
        0
    };
    let candidate_key = (
        u8::from(!candidate.overmatch_ready),
        small_map_first_attack,
        candidate.utility_cost(delay_cost_per_turn),
        candidate.completion_for_ordering(),
        candidate.production_cost,
    );
    if best.as_ref().is_none_or(|current| {
        let current_first_attack = if uses_small_map_tactical_rules(input) {
            current.first_attack_turn.unwrap_or(u32::MAX)
        } else {
            0
        };
        candidate_key
            < (
                u8::from(!current.overmatch_ready),
                current_first_attack,
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
    let mut friendlies = catalog.existing_units.clone();
    let mut protected_units = catalog.protected_units.clone();
    let mut capture_friendly_indices = Vec::new();
    for index in &state.option_indices {
        let friendly_index = friendlies.len();
        friendlies.push(catalog.production_units[*index].clone());
        if input.production_options[*index].capture_target.is_some() {
            capture_friendly_indices.push(friendly_index);
        }
    }
    let mut enemies = catalog.enemies.clone();
    let direct_enemy_remaining_hp = if uses_small_map_tactical_rules(input) {
        direct_enemy_remaining_hp_after_assignment(&friendlies, &enemies, search_turns)
    } else {
        vec![0; enemies.len()]
    };
    let direct_front_shortfall = direct_enemy_remaining_hp
        .iter()
        .filter(|remaining| **remaining > 0)
        .count();
    let mut first_attack_turn = None;
    let mut front_breakthrough_turn = None;
    let mut deadline_target_first_attack_turn = None;
    let mut target_first_attack_turns = vec![None; enemies.len()];
    let mut turn_forecasts = Vec::new();
    let mut combat_purchases = Vec::new();
    let mut assigned_combat_purchases = HashSet::new();
    let mut blocked_next_turn_production_facilities = HashSet::new();
    let mut protected_survivors_at_completion = None;
    // 砲を増やすかは、同じ標的への砲数ではなく、直接部隊・既存砲・今回の砲が
    // 実際にどの敵HPを削ったかで比較する。候補ごとに既存simulationの攻撃処理で
    // 一度だけ加算するので、別の盤面simulationは増やさない。
    let mut direct_damage_by_target = vec![0_u32; enemies.len()];
    let mut existing_indirect_damage_by_target = vec![0_u32; enemies.len()];
    let mut produced_indirect_damage_by_target = vec![0_u32; enemies.len()];

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
        let lethal_attacker_counts = lethal_attacker_counts(&friendlies, &enemies, turn);
        let front_breakthrough_target = (input.exact_property_control
            && input.interdiction_deadlines.is_empty()
            // 一撃で排除できる相性担当がいる場合、その担当を容易な別標的への
            // 集中射撃へ強制しない。単独撃破が無い局面だけ火力集中を組む。
            && lethal_attacker_counts.iter().all(|count| *count == 0))
        .then(|| select_front_breakthrough_target(&friendlies, &enemies, turn))
        .flatten();
        if front_breakthrough_target.is_some() {
            front_breakthrough_turn.get_or_insert(turn);
        }
        for friendly in &mut friendlies {
            if friendly.hp == 0 || turn < friendly.available_turn {
                continue;
            }
            let capture_blocker = friendly
                .capture_contract
                .as_ref()
                .filter(|contract| contract.completed_turn.is_none())
                .and_then(|contract| contract.blocking_enemy_index);
            let blocker_cleared = capture_blocker
                .and_then(|index| enemies.get(index))
                .is_none_or(|enemy| enemy.hp == 0);
            let capture_ready = friendly.capture_contract.as_ref().is_some_and(|contract| {
                contract.completed_turn.is_none()
                    && turn >= contract.arrival_turn
                    && blocker_cleared
            });
            if capture_ready {
                let power = super::property_control::capture_power_from_hp(friendly.hp);
                let contract = friendly
                    .capture_contract
                    .as_mut()
                    .expect("占領可能判定済みの契約が存在する");
                // 占領と攻撃は同一手番に併用しない。被弾後HPから実占領力を求め、
                // 固定2回で完了したことにしない。
                contract.remaining_durability = contract.remaining_durability.saturating_sub(power);
                if contract.remaining_durability == 0 {
                    contract.completed_turn = Some(turn);
                    contract.survived_at_completion = true;
                }
                continue;
            }
            if friendly.attacks_left == 0 {
                continue;
            }
            // 占領レーンを塞ぐ敵が残る間は、同じunitの初撃をその敵へ割り当てる。
            // これによりCapture用cloneとCombat用cloneでHP・行動を二重計上しない。
            let contract_target = capture_blocker.and_then(|target_index| {
                let enemy = enemies.get(target_index)?;
                let profile = friendly
                    .attack_profiles
                    .get(target_index)
                    .copied()
                    .flatten()?;
                (enemy.hp > 0 && turn >= enemy.source.available_turn && turn >= profile.ready_turn)
                    .then_some((target_index, profile))
            });
            let concentrated_target = contract_target.or_else(|| {
                front_breakthrough_target.and_then(|target_index| {
                    let enemy = enemies.get(target_index)?;
                    let profile = friendly
                        .attack_profiles
                        .get(target_index)
                        .copied()
                        .flatten()?;
                    (enemy.hp > 0
                        && turn >= enemy.source.available_turn
                        && turn >= profile.ready_turn)
                        .then_some((target_index, profile))
                })
            });
            let Some((target_index, attack_profile)) = concentrated_target.or_else(|| {
                select_target(
                    friendly,
                    &enemies,
                    turn,
                    &input.interdiction_deadlines,
                    &target_first_attack_turns,
                    &lethal_attacker_counts,
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
            if turn == 1
                && let Some(purchase) = friendly.purchase
                && purchase.build_turn == 0
                && attack_profile.firing_position == Some(purchase.facility)
                && !attack_profile.requires_movement
            {
                // 生産直後のunitが次手番も施設上から攻撃するなら、その手番の生産枠は
                // 物理的に使えない。価格換算せず、失うslot数として保持する。
                blocked_next_turn_production_facilities.insert(purchase.facility);
            }
            attack_count = attack_count.saturating_add(1);
            if friendly.stats.min_range <= 1 {
                direct_damage_by_target[target_index] =
                    direct_damage_by_target[target_index].saturating_add(damage);
            } else if friendly.purchase.is_some() {
                produced_indirect_damage_by_target[target_index] =
                    produced_indirect_damage_by_target[target_index].saturating_add(damage);
            } else {
                existing_indirect_damage_by_target[target_index] =
                    existing_indirect_damage_by_target[target_index].saturating_add(damage);
            }
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
        for friendly in &mut friendlies {
            if friendly.protection_completion_turn == Some(turn) {
                friendly.survived_protection_completion = Some(friendly.hp > 0);
            }
        }
        if input.capture_completion_turn == Some(turn) {
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
        let all_selected_captures_complete = capture_friendly_indices.iter().all(|index| {
            friendlies[*index]
                .capture_contract
                .as_ref()
                .is_some_and(|contract| contract.completed_turn.is_some())
        });
        let existing_capture_complete = input
            .capture_completion_turn
            .is_none_or(|capture_turn| turn >= capture_turn);
        if enemies.iter().all(|enemy| enemy.hp == 0)
            && all_selected_captures_complete
            && existing_capture_complete
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
    let capture_purchases = capture_friendly_indices
        .iter()
        .filter_map(|index| {
            let friendly = &friendlies[*index];
            let contract = friendly.capture_contract.as_ref()?;
            Some(PlannedCapturePurchase {
                purchase: friendly.purchase?,
                target: contract.target,
                completion_turn: contract.completed_turn,
                blocking_enemy_index: contract.blocking_enemy_index,
                survived_at_completion: contract.survived_at_completion,
            })
        })
        .collect::<Vec<_>>();
    let selected_capture_completion_turn = capture_purchases
        .iter()
        .filter_map(|purchase| purchase.completion_turn)
        .chain(input.capture_completion_turn)
        .max();
    let selected_capture_survivor_count = capture_friendly_indices
        .iter()
        .filter(|index| {
            friendlies[**index]
                .capture_contract
                .as_ref()
                .is_some_and(|contract| contract.survived_at_completion)
        })
        .count();
    let existing_protected_survivor_count = friendlies
        .iter()
        .filter(|friendly| friendly.protection_completion_turn.is_some())
        .filter(|friendly| {
            friendly
                .survived_protection_completion
                .unwrap_or(friendly.hp > 0)
        })
        .count();
    // 物件別の合同探索では、対象物件を実際に取り切った同じunitだけを契約成立へ数える。
    // 戦闘用cloneと保護用cloneへ分けず、購入費・HP・行動を一体分に保つ。
    let protected_survivor_count = protected_survivors_at_completion
        .unwrap_or_else(|| protected_units.iter().filter(|unit| unit.hp > 0).count())
        .saturating_add(existing_protected_survivor_count)
        .saturating_add(selected_capture_survivor_count)
        + if input.exact_property_control {
            0
        } else {
            friendlies
                .iter()
                .filter(|unit| {
                    unit.capture_contract.is_none()
                        && unit.protection_completion_turn.is_none()
                        && unit.stats.can_capture
                        && unit.hp > 0
                })
                .count()
        };
    let capture_deadline = input.hard_deadline.unwrap_or(search_turns);
    let deadline_capture_survivor_count = capture_friendly_indices
        .iter()
        .filter(|index| {
            friendlies[**index]
                .capture_contract
                .as_ref()
                .is_some_and(|contract| {
                    contract.survived_at_completion
                        && contract
                            .completed_turn
                            .is_some_and(|turn| turn <= capture_deadline)
                })
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
    let direct_fire_lane_count = if uses_small_map_tactical_rules(input) {
        direct_fire_lane_count(input, &combat_purchases)
    } else {
        0
    };
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
        blocked_next_turn_production_slots: blocked_next_turn_production_facilities.len(),
        direct_fire_lane_count,
        direct_front_shortfall,
        direct_enemy_remaining_hp,
        direct_damage_by_target,
        existing_indirect_damage_by_target,
        produced_indirect_damage_by_target,
        surviving_combat_value,
        required_overmatch_value,
        overmatch_ready: surviving_combat_value >= required_overmatch_value,
        protected_unit_count: input.protected_units.len()
            + catalog
                .existing_units
                .iter()
                .filter(|unit| unit.protection_completion_turn.is_some())
                .count()
            + capture_friendly_indices.len(),
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
                firing_position: None,
                requires_movement: false,
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
        capture_contract: None,
        protection_completion_turn: source.protection_completion_turn,
        survived_protection_completion: None,
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

/// 現在手番に各敵を単独撃破できる友軍数。
///
/// 「戦車なら装甲車を一撃で落とせるが、他兵科では落とせない」のような相性担当を
/// 容易な歩兵へ浪費しないための希少度であり、兵種名や固定評価点は使わない。
fn lethal_attacker_counts(
    friendlies: &[SimFriendly],
    enemies: &[SimEnemy],
    turn: u32,
) -> Vec<usize> {
    enemies
        .iter()
        .enumerate()
        .map(|(enemy_index, enemy)| {
            if enemy.hp == 0 || turn < enemy.source.available_turn {
                return 0;
            }
            friendlies
                .iter()
                .filter(|friendly| {
                    friendly.hp > 0 && friendly.attacks_left > 0 && turn >= friendly.available_turn
                })
                .filter(|friendly| {
                    friendly
                        .attack_profiles
                        .get(enemy_index)
                        .copied()
                        .flatten()
                        .is_some_and(|profile| {
                            turn >= profile.ready_turn
                                && calculate_damage_formula(
                                    profile.base_damage,
                                    friendly.hp,
                                    enemy.source.defense_bonus,
                                    false,
                                ) >= enemy.hp
                        })
                })
                .count()
        })
        .collect()
}

fn select_target(
    friendly: &SimFriendly,
    enemies: &[SimEnemy],
    turn: u32,
    interdiction_deadlines: &[(usize, u32)],
    target_first_attack_turns: &[Option<u32>],
    lethal_attacker_counts: &[usize],
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
            let damage = calculate_damage_formula(
                profile.base_damage,
                friendly.hp,
                enemy.source.defense_bonus,
                false,
            );
            let lethal = damage >= enemy.hp;
            let lethal_scarcity = if lethal {
                lethal_attacker_counts
                    .get(index)
                    .copied()
                    .unwrap_or(1)
                    .max(1)
            } else {
                usize::MAX
            };
            // 占領能力だけで標的順を固定すると、後方の砲兵を放置したまま前衛へ
            // 損耗を重ねる。まず今この一撃で除去でき、代替担当が少ない敵を選ぶ。
            // 同程度なら自分へ返せる最大与ダメージと勝利条件への影響で決める。
            Some((
                (
                    contract_rank,
                    contract_deadline.unwrap_or(u32::MAX),
                    !lethal,
                    lethal_scarcity,
                    std::cmp::Reverse(profile.incoming_damage),
                    strategic_rank,
                    enemy.hp,
                    index,
                ),
                profile,
            ))
        })
        .min_by_key(|(key, _)| *key)
        .map(|((_, _, _, _, _, _, _, index), profile)| (index, profile))
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
                    capture_durability: None,
                    capture_blocking_enemy_index: None,
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
            current_turn: 1,
            opening_policy: OpeningProductionPolicy::Standard,
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
                    capture_durability: None,
                    capture_blocking_enemy_index: None,
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
                    capture_durability: None,
                    capture_blocking_enemy_index: None,
                },
            ],
            production_attack_projections: Vec::new(),
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
    fn indirect_fire_requires_an_off_factory_shot_after_direct_fire_saturates_the_target() {
        let mut input = input();
        // 中央の同じ敵へ直接部隊が二体集まり、一体分が余っているとする。ここでは、
        // 工場外から撃てる間接部隊が直接部隊の担当を置き換えず火力を増やせる。
        input.existing_units.push(FriendlyPlanUnit {
            stats: stats(UnitType::Bcopters, 7_500, 6),
            position: GridPosition { x: 7, y: 0 },
            hp: 100,
            available_turn: 0,
            engageable_enemy_indices: vec![0],
            protection_completion_turn: None,
        });
        input.existing_units.push(FriendlyPlanUnit {
            stats: stats(UnitType::Bcopters, 7_500, 6),
            position: GridPosition { x: 9, y: 0 },
            hp: 100,
            available_turn: 0,
            engageable_enemy_indices: vec![0],
            protection_completion_turn: None,
        });
        let indirect = ProductionPlanOption {
            purchase: PlannedPurchase {
                facility: GridPosition { x: 1, y: 0 },
                unit_type: UnitType::Rockets,
                build_turn: 0,
                cost: 6_000,
            },
            stats: stats(UnitType::Rockets, 6_000, 5),
            engageable_enemy_indices: vec![0],
            capture_target: None,
            capture_arrival_turn: None,
            capture_completion_turn: None,
            capture_durability: None,
            capture_blocking_enemy_index: None,
        };
        input.production_options.push(indirect.clone());
        input.production_attack_projections = vec![
            vec![None],
            vec![None],
            vec![Some(ProductionAttackProjection {
                ready_turn: 2,
                firing_position: GridPosition { x: 6, y: 0 },
                requires_movement: true,
            })],
        ];
        let mut plan = plan_force_package(&input).expect("a plan");
        plan.purchases = vec![indirect.purchase];

        assert!(plan.indirect_fire_has_local_support(&input));

        input.production_attack_projections[2][0]
            .as_mut()
            .expect("projection")
            .firing_position = indirect.purchase.facility;
        assert!(!plan.indirect_fire_has_local_support(&input));
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
            capture_durability: None,
            capture_blocking_enemy_index: None,
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

        let input = input();
        update_best_feasible(&input, &mut selected, &without_margin, 0);
        update_best_feasible(&input, &mut selected, &with_margin, 0);

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
            capture_durability: None,
            capture_blocking_enemy_index: None,
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
    fn capture_unit_attacks_blocker_then_completes_with_one_action_per_turn() {
        let mut input = input();
        input.production_options = vec![ProductionPlanOption {
            purchase: PlannedPurchase {
                facility: GridPosition { x: 0, y: 0 },
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
            capture_target: Some(GridPosition { x: 1, y: 0 }),
            capture_arrival_turn: Some(1),
            capture_completion_turn: Some(2),
            capture_durability: Some(200),
            capture_blocking_enemy_index: Some(0),
        }];
        input.production_attack_projections = vec![vec![Some(ProductionAttackProjection {
            ready_turn: 1,
            firing_position: GridPosition { x: 0, y: 0 },
            requires_movement: false,
        })]];
        input.current_funds = 1_000;
        input.income_per_turn = 0;
        input.hard_deadline = Some(3);
        input.required_capture_survivors = 1;
        input.exact_property_control = true;
        Arc::make_mut(&mut input.damage_chart).insert_damage(
            UnitType::Infantry,
            UnitType::Infantry,
            100,
        );

        let plan = plan_force_package(&input).expect("同じ歩兵が排除後に占領する合同案");

        assert_eq!(plan.purchases.len(), 1, "購入と費用を二重計上しない");
        assert_eq!(plan.production_cost, 1_000);
        assert_eq!(plan.first_attack_turn, Some(1), "生産当日には攻撃しない");
        assert_eq!(plan.target_forecasts[0].destroyed_turn, Some(1));
        assert_eq!(plan.capture_purchases[0].completion_turn, Some(3));
        assert_eq!(plan.deadline_capture_survivor_count, 1);
        assert_eq!(plan.turn_forecasts[0].attack_count, 1);
        assert_eq!(
            plan.turn_forecasts[1].attack_count, 0,
            "占領手番には攻撃しない"
        );
        assert_eq!(
            plan.turn_forecasts[2].attack_count, 0,
            "占領手番には攻撃しない"
        );
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
            protection_completion_turn: None,
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
            protection_completion_turn: None,
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
            protection_completion_turn: None,
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
            protection_completion_turn: None,
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
                capture_durability: None,
                capture_blocking_enemy_index: None,
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
                capture_durability: None,
                capture_blocking_enemy_index: None,
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
                capture_durability: None,
                capture_blocking_enemy_index: None,
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
                capture_durability: None,
                capture_blocking_enemy_index: None,
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
                capture_durability: None,
                capture_blocking_enemy_index: None,
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
                capture_durability: None,
                capture_blocking_enemy_index: None,
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
                capture_durability: None,
                capture_blocking_enemy_index: None,
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
    fn property_plan_prefers_two_cleared_lanes_over_one_clear_and_extra_capturer() {
        let input = input();
        let mut one_clear = plan_force_package(&input).expect("比較元の作戦案");
        one_clear.feasible = false;
        one_clear.target_forecasts = vec![
            TargetForecast {
                destroyed_turn: Some(1),
                ..one_clear.target_forecasts[0].clone()
            },
            TargetForecast {
                destroyed_turn: None,
                ..one_clear.target_forecasts[0].clone()
            },
            TargetForecast {
                destroyed_turn: None,
                ..one_clear.target_forecasts[0].clone()
            },
        ];
        let capture_purchase = |x, completion_turn| PlannedCapturePurchase {
            purchase: PlannedPurchase {
                facility: GridPosition { x, y: 0 },
                unit_type: UnitType::Infantry,
                build_turn: 0,
                cost: 1_000,
            },
            target: GridPosition { x, y: 1 },
            completion_turn: Some(completion_turn),
            blocking_enemy_index: Some(x),
            survived_at_completion: true,
        };
        one_clear.capture_purchases = vec![
            capture_purchase(0, 3),
            capture_purchase(1, 4),
            capture_purchase(2, 5),
        ];
        one_clear.required_capture_survivor_count = 3;
        one_clear.deadline_capture_survivor_count = 3;

        let mut two_clears = one_clear.clone();
        two_clears.target_forecasts[1].destroyed_turn = Some(2);
        two_clears.capture_purchases.pop();
        two_clears.deadline_capture_survivor_count = 2;

        assert_eq!(one_clear.cleared_capture_lane_count(), 1);
        assert_eq!(two_clears.cleared_capture_lane_count(), 2);
        assert!(exact_property_plan_better(&input, &two_clears, &one_clear));
    }

    #[test]
    fn property_plan_prefers_earlier_joint_arrival_over_earlier_damage_only() {
        let input = input();
        let mut fast_arrival = plan_force_package(&input).expect("快速案");
        fast_arrival.feasible = false;
        fast_arrival.target_forecasts[0].destroyed_turn = Some(2);
        fast_arrival.capture_purchases = vec![PlannedCapturePurchase {
            purchase: fast_arrival.purchases[0],
            target: GridPosition { x: 5, y: 0 },
            completion_turn: Some(3),
            blocking_enemy_index: Some(0),
            survived_at_completion: true,
        }];
        fast_arrival.required_capture_survivor_count = 1;
        fast_arrival.deadline_capture_survivor_count = 1;

        let mut slow_stronger = fast_arrival.clone();
        slow_stronger.target_forecasts[0].destroyed_turn = Some(1);
        slow_stronger.capture_purchases[0].completion_turn = Some(4);
        slow_stronger.expected_loss = 0;
        slow_stronger.surviving_combat_value = u32::MAX;

        assert_eq!(fast_arrival.cleared_capture_lane_count(), 1);
        assert_eq!(slow_stronger.cleared_capture_lane_count(), 1);
        assert!(exact_property_plan_better(
            &input,
            &fast_arrival,
            &slow_stronger
        ));
    }

    #[test]
    fn opening_package_rejects_an_affordable_idle_factory() {
        let input = input();
        let first_facility = GridPosition { x: 0, y: 0 };
        let second_facility = GridPosition { x: 1, y: 0 };
        let facilities = vec![(first_facility, vec![0]), (second_facility, vec![1])];
        let first_only = SearchState {
            option_indices: vec![0],
            used_slots: HashSet::from([ProductionSlot {
                facility: first_facility,
                build_turn: 0,
            }]),
            used_capture_targets: HashSet::new(),
            cost: 7_500,
        };
        let both_facilities = SearchState {
            option_indices: vec![0, 1],
            used_slots: HashSet::from([
                ProductionSlot {
                    facility: first_facility,
                    build_turn: 0,
                },
                ProductionSlot {
                    facility: second_facility,
                    build_turn: 0,
                },
            ]),
            used_capture_targets: HashSet::new(),
            cost: 27_500,
        };

        assert!(!opening_package_spends_all_usable_funds(
            &input,
            &first_only,
            &facilities
        ));
        assert!(opening_package_spends_all_usable_funds(
            &input,
            &both_facilities,
            &facilities
        ));
    }

    #[test]
    fn opening_constraints_are_owned_by_the_small_map_pipeline() {
        let mut input = input();
        assert!(!uses_small_map_opening_rules(&input));

        input.opening_policy = OpeningProductionPolicy::SmallMapExpansion;
        assert!(uses_small_map_opening_rules(&input));

        input.current_turn = 2;
        assert!(!uses_small_map_opening_rules(&input));
    }

    #[test]
    fn property_front_selects_two_unit_breakthrough_and_three_capture_lanes() {
        let mut input = input();
        input.production_options.clear();
        input.production_attack_projections.clear();
        input.current_funds = 11_400;
        input.income_per_turn = 0;
        input.hard_deadline = Some(6);
        input.required_capture_survivors = 3;
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
                capture_durability: None,
                capture_blocking_enemy_index: None,
            });
            input
                .production_attack_projections
                .push(vec![Some(ProductionAttackProjection {
                    ready_turn: 1,
                    firing_position: facility,
                    requires_movement: false,
                })]);

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
                capture_durability: Some(300),
                capture_blocking_enemy_index: Some(0),
            });
            input.production_attack_projections.push(vec![None]);
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

    #[test]
    fn property_control_bounds_many_factory_combinations_before_simulation() {
        assert_eq!(
            property_control_state_limit(5),
            MANY_FACTORY_PROPERTY_STATE_LIMIT
        );
        assert_eq!(
            property_control_state_limit(6),
            MANY_FACTORY_PROPERTY_STATE_LIMIT
        );

        let mut input = input();
        input.map = Arc::new(Map::new(20, 1, Terrain::Plains, GridTopology::Square));
        input.production_options.clear();
        input.production_attack_projections.clear();
        input.current_funds = 100_000;
        input.required_capture_survivors = 2;
        input.exact_property_control = true;
        Arc::make_mut(&mut input.damage_chart).insert_damage(
            UnitType::Recon,
            UnitType::Infantry,
            60,
        );

        // 7工場 x (不生産・直接・占領) で2,187通りになる入力を作る。
        // 上限を超えた場合だけ候補前線へ縮約し、最終候補は実戦闘simで選ぶ。
        for x in 0..7 {
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
                capture_durability: None,
                capture_blocking_enemy_index: None,
            });
            input
                .production_attack_projections
                .push(vec![Some(ProductionAttackProjection {
                    ready_turn: 1,
                    firing_position: facility,
                    requires_movement: false,
                })]);
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
                capture_target: Some(GridPosition { x: x + 10, y: 0 }),
                capture_arrival_turn: Some(2),
                capture_completion_turn: Some(4),
                capture_durability: Some(200),
                capture_blocking_enemy_index: Some(0),
            });
            input.production_attack_projections.push(vec![None]);
        }

        let plan = plan_force_package(&input).expect("bounded property plan");

        assert!(plan.search_truncated);
        assert!(
            plan.candidates_considered <= property_control_state_limit(7),
            "{plan:#?}"
        );
    }

    #[test]
    fn property_frontier_preserves_fast_screen_profile_before_damage_rank() {
        let mut input = input();
        input.production_options.clear();
        input.production_attack_projections.clear();
        input.required_capture_survivors = 2;
        Arc::make_mut(&mut input.damage_chart).insert_damage(
            UnitType::Mech,
            UnitType::Infantry,
            100,
        );
        Arc::make_mut(&mut input.damage_chart).insert_damage(
            UnitType::Recon,
            UnitType::Infantry,
            1,
        );

        let slow_option = |unit_type| ProductionPlanOption {
            purchase: PlannedPurchase {
                facility: GridPosition { x: 0, y: 0 },
                unit_type,
                build_turn: 0,
                cost: 1_000,
            },
            stats: stats(unit_type, 1_000, 3),
            engageable_enemy_indices: vec![0],
            capture_target: None,
            capture_arrival_turn: None,
            capture_completion_turn: None,
            capture_durability: None,
            capture_blocking_enemy_index: None,
        };
        for _ in 0..MANY_FACTORY_PROPERTY_STATE_LIMIT {
            input.production_options.push(slow_option(UnitType::Mech));
            input
                .production_attack_projections
                .push(vec![Some(ProductionAttackProjection {
                    ready_turn: 2,
                    firing_position: GridPosition { x: 0, y: 0 },
                    requires_movement: true,
                })]);
        }
        let fast_index = input.production_options.len();
        input.production_options.push(slow_option(UnitType::Recon));
        input
            .production_attack_projections
            .push(vec![Some(ProductionAttackProjection {
                ready_turn: 1,
                firing_position: GridPosition { x: 1, y: 0 },
                requires_movement: true,
            })]);

        let two_capture_lanes =
            HashSet::from([GridPosition { x: 2, y: 0 }, GridPosition { x: 3, y: 0 }]);
        let mut states = (0..MANY_FACTORY_PROPERTY_STATE_LIMIT)
            .map(|index| SearchState {
                option_indices: vec![index],
                used_slots: HashSet::new(),
                used_capture_targets: two_capture_lanes.clone(),
                cost: 1_000,
            })
            .collect::<Vec<_>>();
        states.push(SearchState {
            option_indices: vec![fast_index],
            used_slots: HashSet::new(),
            used_capture_targets: HashSet::from([
                GridPosition { x: 2, y: 0 },
                GridPosition { x: 3, y: 0 },
                GridPosition { x: 4, y: 0 },
            ]),
            cost: 1_000,
        });

        retain_property_control_frontier(&input, &mut states, MANY_FACTORY_PROPERTY_STATE_LIMIT);

        assert_eq!(states.len(), MANY_FACTORY_PROPERTY_STATE_LIMIT);
        assert!(
            states
                .iter()
                .any(|state| state.option_indices == vec![fast_index])
        );
    }

    #[test]
    fn small_map_frontier_keeps_earlier_capture_over_stronger_late_capture() {
        let mut input = input();
        input.opening_policy = OpeningProductionPolicy::SmallMapExpansion;
        input.required_capture_survivors = 1;
        input.production_options.clear();
        input.production_attack_projections.clear();
        Arc::make_mut(&mut input.damage_chart).insert_damage(
            UnitType::Mech,
            UnitType::Infantry,
            100,
        );
        Arc::make_mut(&mut input.damage_chart).insert_damage(
            UnitType::Infantry,
            UnitType::Infantry,
            10,
        );

        let target = GridPosition { x: 5, y: 0 };
        let slow = ProductionPlanOption {
            purchase: PlannedPurchase {
                facility: GridPosition { x: 0, y: 0 },
                unit_type: UnitType::Mech,
                build_turn: 0,
                cost: 2_000,
            },
            stats: UnitStats {
                can_capture: true,
                ..stats(UnitType::Mech, 2_000, 2)
            },
            engageable_enemy_indices: vec![0],
            capture_target: Some(target),
            capture_arrival_turn: Some(3),
            capture_completion_turn: Some(5),
            capture_durability: Some(100),
            capture_blocking_enemy_index: None,
        };
        let fast = ProductionPlanOption {
            purchase: PlannedPurchase {
                facility: GridPosition { x: 0, y: 0 },
                unit_type: UnitType::Infantry,
                build_turn: 0,
                cost: 1_000,
            },
            stats: UnitStats {
                can_capture: true,
                ..stats(UnitType::Infantry, 1_000, 3)
            },
            engageable_enemy_indices: vec![0],
            capture_target: Some(target),
            capture_arrival_turn: Some(2),
            capture_completion_turn: Some(4),
            capture_durability: Some(100),
            capture_blocking_enemy_index: None,
        };
        input.production_options = vec![slow, fast];
        input.production_attack_projections = vec![vec![None], vec![None]];

        let slow_state = SearchState {
            option_indices: vec![0],
            used_slots: HashSet::new(),
            used_capture_targets: HashSet::from([target]),
            cost: 2_000,
        };
        let fast_state = SearchState {
            option_indices: vec![1],
            used_slots: HashSet::new(),
            used_capture_targets: HashSet::from([target]),
            cost: 1_000,
        };

        assert!(
            property_control_frontier_key(&input, &fast_state)
                < property_control_frontier_key(&input, &slow_state)
        );
    }
}
