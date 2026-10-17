//! 生産判断の一時入力・購入・配備の型。

use crate::ai::v4::*;

/// 揚陸可否キャッシュのキー。
///
/// 移動種別と座標を生タプルの順番で取り違えないよう、用途ごとの値オブジェクトにする。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(in crate::ai::v4) struct DeliveryKey {
    pub(in crate::ai::v4) transport_movement: MovementType,
    pub(in crate::ai::v4) cargo_movement: MovementType,
    pub(in crate::ai::v4) start: GridPosition,
    pub(in crate::ai::v4) target: GridPosition,
}

/// 射程内への到達可否キャッシュのキー。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(in crate::ai::v4) struct EngagementKey {
    pub(in crate::ai::v4) movement_type: MovementType,
    pub(in crate::ai::v4) start: GridPosition,
    pub(in crate::ai::v4) target: GridPosition,
    pub(in crate::ai::v4) range: u32,
}

/// 到達性まわりの計算結果を、1 回の生産判断のあいだだけ再利用するためのコンテキスト。
///
/// 揚陸可否の判定はマップ全域の走査を伴うため、施設と目標の組み合わせごとに
/// 結果を憶えておかないと候補評価のたびに同じ探索を繰り返すことになる。
#[derive(Default)]
pub(in crate::ai::v4) struct ReachCtx {
    pub(in crate::ai::v4) terrain: TerrainConnectivity,
    pub(in crate::ai::v4) delivery: HashMap<DeliveryKey, bool>,
    pub(in crate::ai::v4) engagement: HashMap<EngagementKey, bool>,
    /// 生産地点から射撃可能位置までの実行ターン。余剰増援の候補表だけで使い、
    /// 移動後に攻撃できない間接unitを「今すぐ有効」と誤認しない。
    pub(in crate::ai::v4) action_turns: ActionTurnDistanceCache,
}

impl ReachCtx {
    /// 地形連結による到達判定（`TerrainConnectivity` への委譲）。
    pub(in crate::ai::v4) fn is_reachable(
        &mut self,
        map: &Map,
        registry: &MasterDataRegistry,
        start: (usize, usize),
        target: (usize, usize),
        movement_type: MovementType,
    ) -> bool {
        self.terrain
            .is_reachable(map, registry, start, target, movement_type)
    }

    /// 標的位置そのものではなく、射程内の合法地形へ到達できるかを判定する。
    /// 艦船の遠距離射撃を「陸上座標へ入れない」という理由で候補外にしない。
    pub(in crate::ai::v4) fn can_reach_engagement_envelope(
        &mut self,
        map: &Map,
        registry: &MasterDataRegistry,
        start: (usize, usize),
        target: (usize, usize),
        movement_type: MovementType,
        max_range: u32,
    ) -> bool {
        let key = EngagementKey {
            movement_type,
            start: GridPosition {
                x: start.0,
                y: start.1,
            },
            target: GridPosition {
                x: target.0,
                y: target.1,
            },
            range: max_range.max(1),
        };
        if let Some(cached) = self.engagement.get(&key) {
            return *cached;
        }
        let range = max_range.max(1);
        let reachable = (0..map.height).any(|y| {
            (0..map.width).any(|x| {
                map.distance(x, y, target.0, target.1) <= range
                    && map.get_terrain(x, y).is_some_and(|terrain| {
                        crate::systems::movement::get_valid_movement_cost(
                            registry,
                            movement_type,
                            terrain,
                        )
                        .is_some()
                    })
                    && self.is_reachable(map, registry, start, (x, y), movement_type)
            })
        });
        self.engagement.insert(key, reachable);
        reachable
    }
}

/// 敵がこのターン数以内に到達できる自軍拠点は防衛作戦の対象とする。
pub(in crate::ai::v4) const DEFENSE_THREAT_ETA: u32 = 2;

/// 占領開始後に拠点を確保し切るまでに必要な最小手番数。
pub(in crate::ai::v4) const CAPTURE_COMPLETION_TURNS: u32 = 2;

/// 盤面から取り出したユニット 1 体分の情報。
#[derive(Debug, Clone)]
pub(in crate::ai::v4) struct UnitSnapshot {
    /// 盤面上の実Entity。純粋関数テストの合成snapshotではNoneを許容する。
    pub(in crate::ai::v4) entity: Option<Entity>,
    pub(in crate::ai::v4) pos: GridPosition,
    pub(in crate::ai::v4) stats: UnitStats,
    pub(in crate::ai::v4) hp: u32,
    pub(in crate::ai::v4) free_cargo: u32,
}

/// 敵が実際に生産へ使える施設。所有者の首都から生産範囲内にある施設だけを保持する。
#[derive(Debug, Clone, Copy)]
pub(in crate::ai::v4) struct EnemyFacilitySnapshot {
    pub(in crate::ai::v4) pos: GridPosition,
    pub(in crate::ai::v4) terrain: Terrain,
}

/// 1体の敵に対する実盤面情報。
///
/// 戦力を購入価格へ換算しない。必要戦力はRollingPlanがHP・与ダメージ・移動・
/// 攻撃可能回数を手番ごとにシミュレーションして決める。
#[derive(Debug, Clone)]
pub(in crate::ai::v4) struct ThreatTarget {
    pub(in crate::ai::v4) entity: Option<Entity>,
    pub(in crate::ai::v4) stats: UnitStats,
    pub(in crate::ai::v4) position: GridPosition,
    pub(in crate::ai::v4) current_hp: u32,
    /// 0は現在の局地敵。1以上はanchorへ到着してから交戦可能になる観測済み増援。
    pub(in crate::ai::v4) available_turn: u32,
}

impl ThreatTarget {
    pub(in crate::ai::v4) fn from_snapshot(unit: &UnitSnapshot) -> Self {
        Self {
            entity: unit.entity,
            stats: unit.stats.clone(),
            position: unit.pos,
            current_hp: unit.hp,
            available_turn: 0,
        }
    }
}

/// 1 つの作戦。対象拠点のまとまりと、そこから導出された枠を保持する。
#[derive(Debug)]
pub(in crate::ai::v4) struct Operation {
    pub(in crate::ai::v4) kind: OperationKind,
    /// 島campaignの永続identity。anchorや未所有施設集合が変わっても維持する。
    pub(in crate::ai::v4) island_id: Option<crate::ai::islands::IslandId>,
    /// V4中央契約との作戦ID。
    pub(in crate::ai::v4) operation_id: Option<plan_contract::PropertyContractId>,
    /// 作戦の代表地点（距離計算の基準）
    pub(in crate::ai::v4) anchor: GridPosition,
    /// 編成中の戦力を逐次投入せず集結させる、自軍側の安全な地点。
    pub(in crate::ai::v4) staging_anchor: GridPosition,
    /// falseの首都作戦は生産だけ進め、攻撃任務へ切り替えない。
    pub(in crate::ai::v4) execution_authorized: bool,
    /// 占領完了まで敵の攻撃から生存させる必要があるcampaign占領Entity。
    pub(in crate::ai::v4) protected_capture_entities: HashSet<Entity>,
    /// 初回編成ではなく、撃破済み占領担当1体の再契約を要求しているか。
    pub(in crate::ai::v4) capture_replacement_pending: bool,
    /// anchorが所有権変化で動いても同じ目的を照合するための拠点集合。
    pub(in crate::ai::v4) objective_properties: Vec<GridPosition>,
    /// Capture cluster全体ではなく、今この手番に成立させる一つの物件制御契約。
    pub(in crate::ai::v4) property_control: Option<PropertyControlObjective>,
    /// 同時に別々の敵Entityへ成立させる必要がある物件制御契約。
    pub(in crate::ai::v4) property_controls: Vec<PropertyControlObjective>,
    /// 直取りまたは期限内妨害が成立する物件ごとの占領任務先。
    /// 一つのclusterを代表点へ畳まず、生産した占領役を別々の物件へ割り当てる。
    pub(in crate::ai::v4) capture_lane_targets: Vec<GridPosition>,
    pub(in crate::ai::v4) planned_capture_units: Vec<PlannedCaptureUnit>,
    /// 防衛の硬い期限と、旧枠の診断にだけ使う作戦時間幅。
    /// 敵を作戦へ帰属させる条件には使わない。
    pub(in crate::ai::v4) threat_horizon: u32,
    pub(in crate::ai::v4) facts: OperationFacts,
    pub(in crate::ai::v4) slots: OperationSlots,
    /// この生産計画の中で既に購入した分
    pub(in crate::ai::v4) filled: OperationSlots,
    /// 自軍が生産しうるどの移動タイプでも到達できない位置にいる敵（＝迎え撃つしかない敵）
    pub(in crate::ai::v4) unreachable_threats: Vec<ThreatTarget>,
    /// 自軍が生産しうるいずれかの移動タイプで到達できる位置にいる敵（＝殴りに行ける敵）
    pub(in crate::ai::v4) reachable_threats: Vec<ThreatTarget>,
    /// 通常見積りで前線へ到着する将来増援。RollingPlanの仮想敵として含める。
    pub(in crate::ai::v4) expected_reinforcements: Vec<EnemyPlanUnit>,
    /// main互換経路: 観測後のcounterでは接触前に止められない将来増援だけを現在編成へ含める。
    pub(in crate::ai::v4) unavoidable_reinforcements: Vec<EnemyPlanUnit>,
    /// 観測後に間に合うため、具体的counterと生産slotだけを予約する条件付き計画。
    pub(in crate::ai::v4) reinforcement_contingencies: Vec<ReinforcementContingency>,
    /// Expectedを上回る敵の最大対抗生産額。Goは止めず、次回の再計画理由として記録する。
    pub(in crate::ai::v4) stress_reinforcement_funds: u32,
    pub(in crate::ai::v4) contingency_reserve_funds: u32,
}

#[derive(Debug, Clone, Copy)]
pub(in crate::ai::v4) struct ReinforcementContingency {
    pub(in crate::ai::v4) enemy_type: UnitType,
    pub(in crate::ai::v4) enemy_contact_turn: u32,
    pub(in crate::ai::v4) counter_type: UnitType,
    pub(in crate::ai::v4) counter_facility: GridPosition,
    pub(in crate::ai::v4) counter_build_turn: u32,
    pub(in crate::ai::v4) counter_contact_turn: u32,
    pub(in crate::ai::v4) attacks_required: u32,
    pub(in crate::ai::v4) reserve_cost: u32,
}

/// 1手番の全施設について一度だけ作ったV4生産計画。
/// 1命令ごとの再計画で残存脅威台帳を初期化しないため、同じ計画を順に消費する。
#[derive(Resource, Debug, Default)]
pub(in crate::ai::v4) struct V4ProductionTurnPlan {
    pub(in crate::ai::v4) player_id: Option<PlayerId>,
    pub(in crate::ai::v4) turn: u32,
    pub(in crate::ai::v4) commands: VecDeque<PlannedProduction>,
}

/// 生産命令を施設ごとに返す間も、作戦実績の盤面scanは1手番に一度だけ行います。
#[derive(Resource, Debug, Default)]
pub(in crate::ai::v4) struct V4ProductionObservationCache {
    pub(in crate::ai::v4) player_id: Option<PlayerId>,
    pub(in crate::ai::v4) turn: u32,
}

/// 島嶼キャンペーンの完全パッケージをV4の汎用作戦より先に処理した結果。
pub(in crate::ai::v4) enum CampaignProductionControl {
    /// 今回の呼び出しで発行するキャンペーン生産命令。
    Command(ProduceUnitCommand),
    /// 高優先作戦を完成できないため、汎用生産へ予算を流さず終了する。
    BlockGeneric,
    /// 島嶼作戦の予約額を除いた余剰だけで、迎撃・戦闘枠を生産する。
    ContinueWithSurplus(u32),
    /// キャンペーン要求が無いか全行を完成済みなので、V4汎用生産へ進める。
    Continue,
}

/// 生産候補 1 件。
#[derive(Debug, Clone, Copy)]
pub(in crate::ai::v4) struct SlotCandidate {
    pub(in crate::ai::v4) unit_type: UnitType,
    pub(in crate::ai::v4) cost: u32,
    pub(in crate::ai::v4) facility: GridPosition,
    /// 枠への適合度。大きいほど良い。
    pub(in crate::ai::v4) fitness: f32,
}

#[derive(Debug, Clone, Copy)]
pub(in crate::ai::v4) struct CandidateConstraints {
    pub(in crate::ai::v4) remaining_funds: u32,
    pub(in crate::ai::v4) per_slot_budget: u32,
}

/// 生産命令と、その命令だけが持つV4作戦意図。
///
/// 役割・費用・目的は購入案自身が持ち、価格や兵種から後で推測しない。
#[derive(Debug)]
pub(in crate::ai::v4) struct PlannedProduction {
    pub(in crate::ai::v4) command: ProduceUnitCommand,
    /// 購入した枠の役割。
    pub(in crate::ai::v4) role: SlotKind,
    /// 配分時に確保した生産費。
    pub(in crate::ai::v4) cost: u32,
    /// 継続Planの工程か、計画を持たない即時購入か。
    pub(in crate::ai::v4) purpose: PurchasePurpose<PlanStepRef>,
    pub(in crate::ai::v4) deployment: Option<PlannedDeployment>,
    /// Capture枠は完成時点からRoadmapの受入Squadに所属させる。Combat deploymentとは
    /// 別に保持し、占領歩兵をReserve/Defenseへ落とさない。
    pub(in crate::ai::v4) capture_intent: Option<campaign_execution::CampaignProductionIntent>,
}

/// Combat / Intercept枠からpending deploymentへ渡す情報。
#[derive(Debug)]
pub(in crate::ai::v4) struct PlannedDeployment {
    pub(in crate::ai::v4) anchor: GridPosition,
    pub(in crate::ai::v4) staging_anchor: GridPosition,
    pub(in crate::ai::v4) posture: deployment::DeploymentPosture,
    pub(in crate::ai::v4) slot_kind: SlotKind,
    pub(in crate::ai::v4) priority_enemies: Vec<Entity>,
    pub(in crate::ai::v4) threat_horizon: u32,
    pub(in crate::ai::v4) forecast: deployment::DeploymentForecast,
    /// 永続Combat計画の生産step。旧Intercept等ではNone。
    pub(in crate::ai::v4) plan_step: Option<PlanStepRef>,
    /// V4中央契約との軽量バインディング。
    pub(in crate::ai::v4) operation_binding: Option<plan_contract::OperationBinding>,
}

impl std::ops::Deref for PlannedProduction {
    type Target = ProduceUnitCommand;

    fn deref(&self) -> &Self::Target {
        &self.command
    }
}

#[derive(Debug, Clone)]
pub(in crate::ai::v4) struct CampaignPlanningObjective {
    pub(in crate::ai::v4) island_id: crate::ai::islands::IslandId,
    pub(in crate::ai::v4) kind: OperationKind,
    pub(in crate::ai::v4) anchor: GridPosition,
    /// 現在同時に進める局地前線。勝利ロードマップの全目標とは分けて保持する。
    pub(in crate::ai::v4) objective_properties: Vec<GridPosition>,
    pub(in crate::ai::v4) capture_eta: Option<u32>,
    /// anchorの局所clusterではなく、島作戦本体が要求する占領完了時の生存兵数。
    pub(in crate::ai::v4) required_capture_survivors: usize,
    /// 固定兵站経路内の工程順。経路外campaignはNone。
    pub(in crate::ai::v4) logistics_rank: Option<u32>,
    /// 同じ島の敵を別前線へ分配せず、この戦略作戦へ所属させる。
    pub(in crate::ai::v4) forced_target_enemies: HashSet<Entity>,
    /// 施設数だけでなく、実際の占領兵が完了時点まで生存する案を評価する。
    pub(in crate::ai::v4) protected_capture_entities: HashSet<Entity>,
    /// 永続Roadmap上で占領担当撃破後の再契約が未完了か。
    pub(in crate::ai::v4) capture_replacement_pending: bool,
    /// 編成中は目的地へ逐次投入せず、ここへ集結させる。
    pub(in crate::ai::v4) staging_anchor: GridPosition,
    /// falseなら生産は行うが、作戦地点への攻撃任務はまだ発行しない。
    pub(in crate::ai::v4) execution_authorized: bool,
}

/// 敵が保持する生産施設と収入から、作戦地点へ期限内に到着できる増援列を作る。
///
/// 現在数へ固定値を足すのではなく、各手番の資金、facility slot、生産可能兵種、移動ETAを
/// 同じ時間軸へ置く。敵の現在資金は非公開なので0から始め、将来収入だけを使う。
/// Expectedは通常の継続戦力、Stressは観測後counterの再計画対象として用途を分ける。
#[derive(Debug, Default)]
pub(in crate::ai::v4) struct ReinforcementAssessment {
    pub(in crate::ai::v4) reinforcements: Vec<EnemyPlanUnit>,
    /// main互換経路: 観測後のcounterでは止められない増援。
    pub(in crate::ai::v4) unavoidable: Vec<EnemyPlanUnit>,
    pub(in crate::ai::v4) contingencies: Vec<ReinforcementContingency>,
}
