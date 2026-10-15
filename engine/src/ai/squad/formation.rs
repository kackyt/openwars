//! 手番ごとの部隊再編（Squad編成）。
//!
//! 1回の再編を次の段に分け、各段は前段の結果だけを受け取る。
//!
//! 1. 前手番の掃除と再計画の準備（[`super::update_squads`]・Reserve解放）
//! 2. 戦略分析と採用（[`PlanningPolicy::adopt_portfolio`]）
//! 3. campaign編成（採用portfolioの防衛・攻勢と、V4の配備Squad）
//! 4. 汎用編成（輸送・占領・攻撃・余剰吸収）。盤面の事実は [`FormationBoard`]
//! 5. 仕上げ（V4所属修復・所属の正規化・Roadmap観測）
//!
//! 通常（V2/V3・標準マップV4）と小規模マップV4の差は [`PlanningPolicy`] に集める。

use super::assignment::{AssignmentProposal, SquadDraft, SquadSlot};
use super::*;
use crate::ai::context::board::{BoardUnit, PlayerBoard};
use crate::ai::island_campaign::{IslandCampaignAssignment, IslandCampaignPortfolio};
use crate::ai::islands::{IslandId, IslandMap};
use crate::ai::planning::campaign;
use crate::ai::planning::situation::{self, StrategicSituation};
use crate::systems::movement::OccupantInfo;
use std::borrow::Cow;

/// 自軍のfree unit（Entity・位置・能力値）。
pub(super) type FreeUnit = (Entity, GridPosition, UnitStats);

/// 再編の方策。通常経路と小規模マップV4拡張経路で異なる判断だけを持つ。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PlanningPolicy {
    /// V2/V3と標準マップのV4。
    Standard,
    /// 小規模マップのV4拡張経路（首都ルートDAG・Roadmapでの採用を加える）。
    SmallMap,
}

impl PlanningPolicy {
    /// SquadManagerをWorldから外す前の準備（小規模V4: 戦略パイプラインの事前処理）。
    fn before_manager_removal(self, world: &mut World, player: PlayerId, is_v4: bool) {
        if self == Self::SmallMap && is_v4 {
            crate::ai::v4::strategy_pipeline::before_squad_planning(world, player);
        }
    }

    /// 戦略分析のportfolioから、この手番に採用するportfolioを決める。
    pub(super) fn adopt_portfolio<'a>(
        self,
        world: &mut World,
        player: PlayerId,
        manager: &SquadManager,
        portfolio: &'a IslandCampaignPortfolio,
        is_v4: bool,
    ) -> Cow<'a, IslandCampaignPortfolio> {
        match self {
            Self::Standard => Cow::Borrowed(portfolio),
            Self::SmallMap => Cow::Owned(extended::adopt_portfolio(
                world, player, manager, portfolio, is_v4,
            )),
        }
    }

    /// campaign assignmentの局地Squad（占領・攻撃・防衛）を準備する。
    pub(super) fn prepare_local_assignment(
        self,
        world: &World,
        manager: &mut SquadManager,
        player: PlayerId,
        assignment: &IslandCampaignAssignment,
        connectivity: &mut TerrainConnectivity,
    ) {
        match self {
            // 親の関数は実行中の経路で拡張実装へ委譲する。
            Self::Standard => prepare_campaign_local_assignment_with_connectivity(
                world,
                manager,
                player,
                assignment,
                connectivity,
            ),
            // 拡張側は委譲点を経由せず、子の独立ID付き実装を直接呼ぶ。
            Self::SmallMap => extended::prepare_campaign_local_assignment_with_connectivity(
                world,
                manager,
                player,
                assignment,
                connectivity,
            ),
        }
    }

    /// 汎用占領の目標順を決める。
    pub(super) fn sort_capture_targets(
        self,
        world: &mut World,
        player: PlayerId,
        targets: &mut [GridPosition],
        context: &CapturePlanningContext<'_>,
    ) {
        match self {
            Self::Standard => sort_capture_targets_by_nearest_infantry(targets, context),
            Self::SmallMap => {
                extended::sort_small_map_capture_targets(world, player, targets, context)
            }
        }
    }

    /// 汎用占領の目標へ割り当てる同島の歩兵を選ぶ。
    pub(super) fn select_capture_infantry(
        self,
        context: &CapturePlanningContext<'_>,
        island: IslandId,
        target: &GridPosition,
    ) -> Option<usize> {
        match self {
            Self::Standard => select_first_reachable_infantry(context, island, target),
            Self::SmallMap => extended::select_nearest_capture_infantry(context, island, target),
        }
    }

    /// 所属の正規化の前のRoadmap観測（通常V4）。
    fn before_final_reconcile(
        self,
        world: &mut World,
        player: PlayerId,
        portfolio: &IslandCampaignPortfolio,
        manager: &SquadManager,
        is_v4: bool,
    ) {
        if self == Self::Standard && is_v4 {
            crate::ai::v4::victory_roadmap::reconcile_campaign_roadmap(
                world, player, portfolio, manager,
            );
        }
    }

    /// 所属の正規化の後の仕上げ（小規模V4: 経路commitmentとRoadmapでの採用）。
    fn after_final_reconcile(
        self,
        world: &mut World,
        player: PlayerId,
        portfolio: &IslandCampaignPortfolio,
        manager: &mut SquadManager,
        is_v4: bool,
    ) {
        if self == Self::SmallMap && is_v4 {
            crate::ai::v4::strategy_pipeline::after_squad_planning(
                world, player, portfolio, manager,
            );
        }
    }
}

/// 通常の占領目標順: 最寄りのfree歩兵からの距離順。V3は生産施設を優先する。
pub(super) fn sort_capture_targets_by_nearest_infantry(
    targets: &mut [GridPosition],
    context: &CapturePlanningContext<'_>,
) {
    targets.sort_by_key(|target| {
        let distance = context
            .infantry
            .iter()
            .map(|(_, position, _)| position.x.abs_diff(target.x) + position.y.abs_diff(target.y))
            .min()
            .unwrap_or(usize::MAX);
        let facility_bonus = if context.is_v3
            && context
                .map
                .get_terrain(target.x, target.y)
                .is_some_and(|terrain| context.registry.is_production_facility(terrain.as_str()))
        {
            PRODUCTION_FACILITY_DIST_BONUS
        } else {
            0
        };
        (distance.saturating_sub(facility_bonus), target.x, target.y)
    });
}

/// 通常の占領要員: 目標と同じ島で地形上到達できる最初のfree歩兵。
fn select_first_reachable_infantry(
    context: &CapturePlanningContext<'_>,
    island: IslandId,
    target: &GridPosition,
) -> Option<usize> {
    context.infantry.iter().position(|(_, position, stats)| {
        context
            .island_map
            .get_island_at(position)
            .is_some_and(|candidate| candidate.id == island)
            && is_terrain_reachable(
                context.map,
                context.registry,
                (position.x, position.y),
                (target.x, target.y),
                stats.movement_type,
            )
    })
}

/// campaign編成の結果。汎用編成はここに含まれる島・Entityを避ける。
struct CampaignFormation {
    /// 採用portfolioの防衛・攻勢（優先順）。
    assignments: Vec<IslandCampaignAssignment>,
    /// campaign・安全占領・配備が予約したEntity。
    reserved_entities: HashSet<Entity>,
    /// 輸送が成立せず封鎖された島。
    blocked_islands: HashSet<IslandId>,
    /// 一時停止中の島。
    paused_islands: HashSet<IslandId>,
}

/// 汎用編成で共有する読み取り専用の盤面の事実。地図・unit・物件は [`PlayerBoard`] から借りる。
struct FormationBoard<'a> {
    player: PlayerId,
    is_v3: bool,
    map: &'a Map,
    registry: &'a MasterDataRegistry,
    island_map: &'a IslandMap,
    /// 盤面snapshot（汎用編成のunit走査に使う）。
    snapshot: &'a PlayerBoard,
    /// 経路探索用の占有情報（編成の占有モデル: 搭載中のunitを除く）。
    unit_positions: HashMap<(usize, usize), OccupantInfo>,
    /// 物件の座標と所有者。
    ownership: HashMap<GridPosition, Option<PlayerId>>,
    /// 自軍物件のある島と、占領対象の物件が残る島。
    base_islands: HashSet<IslandId>,
    target_islands: Vec<IslandId>,
    /// 島ごとの物件座標。
    island_properties: HashMap<IslandId, Vec<GridPosition>>,
    /// 自軍の工場・首都の座標。
    own_production_bases: Vec<GridPosition>,
    /// 島ごとの敵の工場・首都の数と、敵物件のある島。
    enemy_production_count: HashMap<IslandId, u32>,
    enemy_owned_islands: HashSet<IslandId>,
    /// 自軍首都（列挙順で最初のもの）。
    capital: Option<GridPosition>,
}

impl<'a> FormationBoard<'a> {
    fn observe(snapshot: &'a PlayerBoard, is_v3: bool) -> Self {
        let player = snapshot.player;
        let map = &snapshot.map;
        let island_map = &snapshot.island_map;
        let unit_positions = snapshot.formation_occupancy();
        let ownership = snapshot.property_ownership();
        let capital = snapshot
            .properties
            .iter()
            .find(|property| property.terrain == Terrain::Capital && property.owner == Some(player))
            .map(|property| property.position);
        let (base, targets) = island_map.classify_islands(player, &ownership);
        let base_islands: HashSet<_> = base.iter().copied().collect();
        let target_islands = targets.to_vec();

        // V1からの統合: 島ごとの期待値（IslandScore）の算出に使う物件の分布。
        let mut own_production_bases = Vec::new();
        let mut enemy_production_count = HashMap::default();
        let mut enemy_owned_islands = HashSet::default();
        let mut island_properties = HashMap::default();
        for (pos, owner) in &ownership {
            let Some(island) = island_map.get_island_at(pos) else {
                continue;
            };
            island_properties
                .entry(island.id)
                .or_insert_with(Vec::new)
                .push(*pos);
            let terrain = map.get_terrain(pos.x, pos.y).unwrap_or(Terrain::City);
            let is_production_base = terrain == Terrain::Factory || terrain == Terrain::Capital;
            if *owner == Some(player) {
                if is_production_base {
                    own_production_bases.push(*pos);
                }
            } else if owner.is_some() {
                enemy_owned_islands.insert(island.id);
                if is_production_base {
                    *enemy_production_count.entry(island.id).or_insert(0) += 1;
                }
            }
        }

        Self {
            player,
            is_v3,
            map,
            registry: &snapshot.registry,
            island_map,
            snapshot,
            unit_positions,
            ownership,
            base_islands,
            target_islands,
            island_properties,
            own_production_bases,
            enemy_production_count,
            enemy_owned_islands,
            capital,
        }
    }

    /// 自軍以外のunit（搭載中を含む）。
    fn enemy_units(&self) -> impl Iterator<Item = &'a BoardUnit> + '_ {
        let player = self.player;
        self.snapshot
            .units
            .iter()
            .filter(move |unit| unit.owner != player)
    }

    fn capture_context<'b>(&'b self, infantry: &'b [FreeUnit]) -> CapturePlanningContext<'b> {
        CapturePlanningContext {
            map: self.map,
            registry: self.registry,
            island_map: self.island_map,
            infantry,
            capital: self.capital,
            is_v3: self.is_v3,
        }
    }

    /// 2点間のターン距離（移動力・射程1）。
    fn turn_distance(
        &self,
        from: GridPosition,
        to: GridPosition,
        stats: &UnitStats,
        turn_cache: &mut TurnDistanceCache,
    ) -> crate::ai::turn_distance::TurnDistance {
        calculate_turn_distance(
            self.map,
            self.registry,
            &self.unit_positions,
            (from.x, from.y),
            (to.x, to.y),
            stats.movement_type,
            stats.max_movement,
            1,
            self.player,
            turn_cache,
        )
    }
}

/// 汎用編成へ回すfree unit。どのSquadにも予約にも属さない自軍unitを兵種別に持つ。
struct FreeUnits {
    combat: Vec<FreeUnit>,
    infantry: Vec<FreeUnit>,
    transports: Vec<FreeUnit>,
}

impl FreeUnits {
    fn collect(
        board: &FormationBoard<'_>,
        manager: &SquadManager,
        reserved: &HashSet<Entity>,
    ) -> Self {
        // 他の部隊にすでに割り当て済みの全メンバー
        let mut busy_entities = HashSet::default();
        for squad in &manager.squads {
            busy_entities.extend(squad.members.iter().copied());
            busy_entities.extend(squad.cargo_entities.iter().copied());
            busy_entities.extend(squad.delivered_cargo.iter().copied());
        }

        let mut free = Self {
            combat: Vec::new(),
            infantry: Vec::new(),
            transports: Vec::new(),
        };
        for unit in &board.snapshot.units {
            let entity = unit.entity;
            if unit.owner != board.player
                || busy_entities.contains(&entity)
                || reserved.contains(&entity)
                || manager.solo_fallbacks.contains(&entity)
                || unit.transported
                || unit.fuel == Some(0)
            {
                continue;
            }
            let stats = &unit.stats;
            let is_transport = stats.unit_type == UnitType::TransportHelicopter
                || stats.unit_type == UnitType::Lander;
            let is_infantry =
                stats.unit_type == UnitType::Infantry || stats.unit_type == UnitType::Mech;
            if is_transport {
                if unit.cargo.is_some_and(|capacity| capacity.max > 0) {
                    free.transports.push((entity, unit.position, stats.clone()));
                }
            } else if is_infantry {
                free.infantry.push((entity, unit.position, stats.clone()));
            } else {
                free.combat.push((entity, unit.position, stats.clone()));
            }
        }
        free
    }
}

/// 再編で使い回す距離cache。地形連結判定はマップ全体のフラッドフィルを毎回やり直さない。
///
/// `turn_cache` は編成の占有モデルで求めたターン距離なので汎用編成だけが使う。
/// `connectivity` は占有に依存しないため、campaign編成・配備・汎用編成で共有する。
#[derive(Default)]
struct FormationCaches {
    turn_cache: TurnDistanceCache,
    connectivity: TerrainConnectivity,
}

/// ゲームの戦略状況に基づいて、部隊の構築と新規メンバーの割り当てを行う。
pub(super) fn plan_squads_with_policy(world: &mut World, player: PlayerId, policy: PlanningPolicy) {
    // 1. まず既存部隊のクリーンアップと SoloFallback 判定を実行
    update_squads(world, player);
    let version = crate::ai::resolve_player_ai_version(world, player);
    // V3 の戦略拡張 (#53: 敵拠点の奪取目標化) を有効にするかどうか
    let is_v3 = version.uses_v3_tactics();
    let is_v4 = version.uses_operation_driven_production();
    policy.before_manager_removal(world, player, is_v4);
    let mut manager = world.remove_resource::<SquadManager>().unwrap_or_default();
    prepare_squads_for_replanning(world, &mut manager, player, is_v4);
    // 盤面（unit・物件・地図）は再編の間に変わらないため、戦略分析と汎用編成で1つを共有する。
    let snapshot = PlayerBoard::observe(world, player);

    // 2. 戦略分析と採用。cache・campaign編成・汎用輸送は同じ採用portfolioを参照する。
    let analysis = analyze_strategy_for_formation(world, &mut manager, &snapshot, is_v3, is_v4);
    let adopted =
        policy.adopt_portfolio(world, player, &manager, &analysis.campaign_portfolio, is_v4);
    let enemy_clusters = detect_enemy_clusters(world, player);

    // 地形連結cacheは地図と移動種別だけで決まるため、campaign編成・配備・汎用編成で共有する。
    let mut caches = FormationCaches::default();

    // 3. campaign編成
    let mut campaign = form_campaign_squads(
        world,
        &mut manager,
        player,
        &adopted,
        is_v3,
        policy,
        &mut caches.connectivity,
    );
    // V4のCombat枠で生産した実Entityは、campaign再配分とgeneric free poolより前に
    // 要求元の局地Attack任務へ予約する。
    // 既存campaign assignmentへ残っていても、明示的な生産目的を優先して切り離す。
    if is_v4 {
        let deployment_reserved =
            crate::ai::v4::deployment::prepare_deployment_squads_with_connectivity(
                world,
                &mut manager,
                player,
                &HashSet::default(),
                &mut caches.connectivity,
            );
        campaign.reserved_entities.extend(deployment_reserved);
    }
    // #53 (V3): メンバーが全滅した占領部隊を解散する。
    // 残しておくと「そのターゲットには部隊が存在する」と誤判定され続け
    // (dedupe に引っかかる)、失敗した奪取目標へ二度と部隊が送られなくなる
    if is_v3 {
        OperationLedger::squad_list(&mut manager)
            .retain(|s| s.mission_type != MissionType::Capture || !s.members.is_empty());
    }

    // 4. 汎用編成。各段はSquadの新設・追加を提案へ積み、最後に1回だけ確定する。
    let board = FormationBoard::observe(&snapshot, is_v3);
    let mut free = FreeUnits::collect(&board, &manager, &campaign.reserved_entities);
    let mut proposal = AssignmentProposal::new(world, &manager, player);
    let objectives = generic_transport_objectives(&board, &campaign);
    assign_generic_transports(
        world,
        &mut proposal,
        &board,
        &adopted,
        &objectives,
        &mut free,
        &mut caches,
    );
    unload_unreserved_loaded_transports(world, &mut proposal, &mut free);
    if is_v4 {
        assign_capture_depth_transports(
            world,
            &mut proposal,
            &board,
            &campaign.assignments,
            &mut free,
            &mut caches.connectivity,
        );
    }
    form_capture_squads(
        world,
        &mut proposal,
        &board,
        &analysis.situation,
        &campaign.paused_islands,
        policy,
        &mut free,
    );
    form_attack_squads(
        &mut proposal,
        &board,
        &enemy_clusters,
        &mut free,
        &mut caches.turn_cache,
    );
    absorb_surplus_combat_units(
        &mut proposal,
        &board,
        &enemy_clusters,
        &mut free,
        &mut caches.turn_cache,
    );
    proposal.commit(&mut manager);

    // 5. 仕上げ
    finalize_formation(
        world,
        &mut manager,
        player,
        policy,
        &adopted,
        &campaign.assignments,
        is_v4,
    );
    world.insert_resource(manager);
}

/// 再編に使う戦略分析の結果。生産需要は含まない（生産判断の分析が別に求める）。
struct FormationAnalysis {
    situation: StrategicSituation,
    /// 盤面から提案したcampaign portfolio（V2は空）。
    campaign_portfolio: IslandCampaignPortfolio,
}

/// 状況判断とcampaign提案。campaign提案はSquadManagerをWorldから読むため、提案の間だけ戻す。
fn analyze_strategy_for_formation(
    world: &mut World,
    manager: &mut SquadManager,
    snapshot: &PlayerBoard,
    is_v3: bool,
    is_v4: bool,
) -> FormationAnalysis {
    let player = snapshot.player;
    let campaign_portfolio = if is_v3 {
        // V3以降は防衛もcampaign portfolioの通常優先度・排他割当で扱う。
        // V4のdeploymentだけは生産目的が解消されるまで通常free poolから保護する。
        let deployment_entities = if is_v4 {
            world
                .get_resource::<crate::ai::v4::deployment::V4DeploymentRegistry>()
                .map(|deployments| deployments.active_entities(player))
                .unwrap_or_default()
        } else {
            HashSet::default()
        };
        world.insert_resource(std::mem::take(manager));
        let portfolio = campaign::propose(world, player, &deployment_entities);
        *manager = world.remove_resource::<SquadManager>().unwrap_or_default();
        portfolio
    } else {
        IslandCampaignPortfolio::default()
    };
    FormationAnalysis {
        situation: situation::assess(snapshot),
        campaign_portfolio,
    }
}

/// 採用portfolioの防衛・攻勢をSquadへ展開する（V3以降）。V2では放棄されたplaceholderの掃除だけを行う。
fn form_campaign_squads(
    world: &mut World,
    manager: &mut SquadManager,
    player: PlayerId,
    adopted: &IslandCampaignPortfolio,
    is_v3: bool,
    policy: PlanningPolicy,
    connectivity: &mut TerrainConnectivity,
) -> CampaignFormation {
    if is_v3 {
        let mut cache = world
            .remove_resource::<AdoptedTurnPortfolio>()
            .unwrap_or_default();
        cache.set_campaign_portfolio(player, adopted.clone());
        world
            .get_resource_or_insert_with(AiTurnProgress::default)
            .mark_squads_planned(player);
        world.insert_resource(cache);
        claim_campaign_portfolio_assignments(world, player, adopted);
    }
    let paused_islands = if is_v3 {
        campaign_paused_islands(adopted)
    } else {
        HashSet::default()
    };
    let secure_reserved_entities = if is_v3 {
        apply_campaign_pauses(world, manager, player, &paused_islands);
        prepare_secure_local_captures(world, manager, player, adopted)
    } else {
        HashSet::default()
    };
    let mut assignments: Vec<_> = if is_v3 {
        adopted
            .defenses
            .iter()
            .chain(adopted.active_offensives.iter())
            .cloned()
            .collect()
    } else {
        Vec::new()
    };
    assignments.sort_by_key(campaign_assignment_priority);
    let active_islands: HashSet<_> = assignments
        .iter()
        .map(|assignment| assignment.island_id)
        .collect();
    remove_abandoned_campaign_placeholders(world, manager, player, &active_islands);

    let mut reserved_entities: HashSet<_> = assignments
        .iter()
        .flat_map(|assignment| {
            assignment
                .transport_entities
                .iter()
                .chain(assignment.capture_entities.iter())
                .chain(assignment.combat_entities.iter())
        })
        .copied()
        .collect();
    reserved_entities.extend(secure_reserved_entities);
    let mut blocked_islands = HashSet::default();
    for assignment in &assignments {
        if prepare_campaign_transport_assignment_with_connectivity(
            world,
            manager,
            player,
            assignment,
            connectivity,
        ) {
            blocked_islands.insert(assignment.island_id);
        }
        policy.prepare_local_assignment(world, manager, player, assignment, connectivity);
    }
    CampaignFormation {
        assignments,
        reserved_entities,
        blocked_islands,
        paused_islands,
    }
}

/// 汎用輸送の目標島を、島の期待値で並べて返す。
///
/// V3以降は、portfolio assignmentが島別に所有するため、旧経路は未予約の島内目標に限る。
fn generic_transport_objectives(
    board: &FormationBoard,
    campaign: &CampaignFormation,
) -> Vec<crate::ai::objectives::Objective> {
    let mut objectives = Vec::new();
    for &target_id in &board.target_islands {
        let mut min_distance = i32::MAX;
        let mut props_with_terrain = Vec::new();
        if let Some(positions) = board.island_properties.get(&target_id) {
            for tile in positions {
                if board.ownership.get(tile) == Some(&Some(board.player)) {
                    continue; // 自分の拠点はスキップ
                }
                props_with_terrain.push((
                    *tile,
                    board
                        .map
                        .get_terrain(tile.x, tile.y)
                        .unwrap_or(Terrain::City),
                ));

                let mut local_min_dist = i32::MAX;
                let mut nearest_base_pos = None;
                for base_pos in &board.own_production_bases {
                    let dist = (tile.x as i32 - base_pos.x as i32).abs()
                        + (tile.y as i32 - base_pos.y as i32).abs();
                    if dist < local_min_dist {
                        local_min_dist = dist;
                        nearest_base_pos = Some(*base_pos);
                    }
                }
                // 徒歩圏内（6マス以下かつ同じ島）は除外
                if nearest_base_pos
                    .and_then(|p| board.island_map.get_island_at(&p))
                    .filter(|base_island| base_island.id == target_id && local_min_dist <= 6)
                    .is_some()
                {
                    continue;
                }
                min_distance = min_distance.min(local_min_dist);
            }
        }
        let distance_to_nearest_base = if min_distance == i32::MAX {
            10
        } else {
            min_distance
        };
        let enemy_prod = *board.enemy_production_count.get(&target_id).unwrap_or(&0);
        objectives.push(crate::ai::objectives::Objective::evaluate(
            target_id,
            &props_with_terrain,
            distance_to_nearest_base,
            enemy_prod,
            board.registry,
        ));
    }

    if board.is_v3 {
        let assigned_islands: HashSet<_> = campaign
            .assignments
            .iter()
            .map(|assignment| assignment.island_id)
            .collect();
        objectives.retain(|objective| {
            !assigned_islands.contains(&objective.target_island)
                && !campaign.blocked_islands.contains(&objective.target_island)
                && !campaign.paused_islands.contains(&objective.target_island)
                && board.base_islands.contains(&objective.target_island)
                && !board.enemy_owned_islands.contains(&objective.target_island)
        });
    }

    // スコア降順ソート（海を渡る必要がある別島を最優先）。
    // 同点時は島IDで決定し、HashSet や ECS の列挙順へ依存しない。
    objectives.sort_by_key(|objective| {
        let is_same_island = board.base_islands.contains(&objective.target_island);
        let group = if is_same_island { 1u8 } else { 0u8 }; // 別島=0(高優先), 同島=1(低優先)
        (
            group,
            std::cmp::Reverse(objective.priority_score),
            objective.target_island.0,
        )
    });
    objectives
}

/// 目標島の敵兵種（重複なし・兵種順）。
fn enemy_types_on_island(board: &FormationBoard<'_>, island: IslandId) -> Vec<UnitType> {
    let mut enemy_types: Vec<_> = board
        .enemy_units()
        .filter(|unit| {
            board
                .island_map
                .get_island_at(&unit.position)
                .is_some_and(|candidate| candidate.id == island)
        })
        .map(|unit| unit.stats.unit_type)
        .collect();
    enemy_types.sort_by_key(|unit_type| *unit_type as u8);
    enemy_types.dedup();
    enemy_types
}

/// 1つの目標島へ向かう汎用輸送のカーゴ探索条件。
struct CargoSearch<'a> {
    board: &'a FormationBoard<'a>,
    target_island: IslandId,
    target_position: Option<GridPosition>,
    enemy_types: &'a [UnitType],
    damage_chart: Option<&'a crate::resources::DamageChart>,
}

impl CargoSearch<'_> {
    /// 輸送役へ積める最寄りの候補の添字。`require_combat` なら有効な戦闘要員に限る。
    fn nearest(
        &self,
        candidates: &[FreeUnit],
        transport: &FreeUnit,
        caches: &mut FormationCaches,
        require_combat: bool,
    ) -> Option<usize> {
        let board = self.board;
        select_nearest_compatible_cargo(
            candidates,
            transport.1,
            &transport.2,
            self.target_island,
            self.target_position,
            board.island_map,
            board.map,
            board.registry,
            &board.unit_positions,
            board.player,
            &mut caches.turn_cache,
            &mut caches.connectivity,
            self.enemy_types,
            self.damage_chart,
            require_combat,
        )
    }
}

/// A. 汎用輸送: 優先順位の高い島から、輸送役と不足する占領要員・護衛を組む。
fn assign_generic_transports(
    world: &World,
    proposal: &mut AssignmentProposal,
    board: &FormationBoard,
    adopted_portfolio: &IslandCampaignPortfolio,
    objectives: &[crate::ai::objectives::Objective],
    free: &mut FreeUnits,
    caches: &mut FormationCaches,
) {
    // 輸送役の選択順を座標と Entity ID で固定する。
    // 逆順にソートし、後続のループで pop() (O(1)) から取り出せるようにする。
    free.transports
        .sort_by_key(|(entity, pos, _)| std::cmp::Reverse((pos.y, pos.x, entity.to_bits())));

    for objective in objectives {
        if free.transports.is_empty() {
            break;
        }
        let mut to_assign = objective.needed_infantry.0;
        let target_position = resolve_generic_transport_target(
            adopted_portfolio,
            objective.target_island,
            board.player,
            &board.island_properties,
            &board.ownership,
        );
        let enemy_types = enemy_types_on_island(board, objective.target_island);
        let damage_chart = world
            .get_resource::<crate::resources::DamageChart>()
            .cloned();
        let search = CargoSearch {
            board,
            target_island: objective.target_island,
            target_position,
            enemy_types: &enemy_types,
            damage_chart: damage_chart.as_ref(),
        };

        while to_assign > 0 && !free.transports.is_empty() {
            let transport = free.transports.pop().unwrap();
            if let Some(capture_count) =
                load_generic_transport(world, proposal, &search, transport, free, caches)
            {
                to_assign = to_assign.saturating_sub(capture_count);
            }
        }
    }
}

/// 輸送役1体へカーゴを選んで輸送Squadを作る。作ったSquadの占領要員数を返す。
fn load_generic_transport(
    world: &World,
    proposal: &mut AssignmentProposal,
    search: &CargoSearch<'_>,
    transport: FreeUnit,
    free: &mut FreeUnits,
    caches: &mut FormationCaches,
) -> Option<usize> {
    let is_v3 = search.board.is_v3;
    let (transport_entity, transport_position, _) = transport;
    let capacity = world
        .get::<crate::components::CargoCapacity>(transport_entity)
        .map(|cargo| cargo.max as usize)
        .unwrap_or(0);
    if capacity == 0 {
        return None;
    }

    let mut cargo_entities = world
        .get::<crate::components::CargoCapacity>(transport_entity)
        .map(|cargo| cargo.loaded.clone())
        .unwrap_or_default();
    cargo_entities.sort_by_key(|entity| entity.to_bits());
    cargo_entities.dedup();
    cargo_entities.truncate(capacity);
    let initially_loaded_count = cargo_entities.len();
    let mut assigned_capture_count = cargo_entities
        .iter()
        .filter(|entity| {
            world
                .get::<UnitStats>(**entity)
                .is_some_and(|stats| stats.can_capture)
        })
        .count();
    let mut selected_entries = Vec::new();

    if cargo_entities.len() < capacity {
        // V3 は不足している占領要員を先に補い、残り容量へ有効な戦闘要員を積む。
        // V2 は従来どおり戦闘要員を先に検討する。
        let search_order = if is_v3 && assigned_capture_count == 0 {
            [false, true]
        } else {
            [true, false]
        };
        for search_combat in search_order {
            if cargo_entities.len() >= capacity {
                break;
            }
            let candidates = if search_combat {
                &free.combat
            } else {
                &free.infantry
            };
            let Some(index) = search.nearest(candidates, &transport, caches, search_combat) else {
                continue;
            };
            let entry = if search_combat {
                free.combat.swap_remove(index)
            } else {
                free.infantry.swap_remove(index)
            };
            if entry.2.can_capture {
                assigned_capture_count += 1;
            }
            cargo_entities.push(entry.0);
            selected_entries.push((search_combat, entry));
            // V2 は単一カーゴ運用を維持し、V3 のみ残り容量へ護衛を追加する。
            if !is_v3 {
                break;
            }
        }

        while is_v3 && cargo_entities.len() < capacity {
            let Some(index) = search.nearest(&free.infantry, &transport, caches, false) else {
                break;
            };
            let entry = free.infantry.swap_remove(index);
            if entry.2.can_capture {
                assigned_capture_count += 1;
            }
            cargo_entities.push(entry.0);
            selected_entries.push((false, entry));
        }
    }

    if cargo_entities.is_empty() {
        return None;
    }
    let requires_pickup = cargo_entities.len() > initially_loaded_count;
    let mut pickup_position = if requires_pickup {
        select_pickup_position(
            world,
            search.board.player,
            transport_position,
            &transport.2,
            &cargo_entities[initially_loaded_count..],
            &mut caches.connectivity,
        )
    } else {
        Some(transport_position)
    };
    if pickup_position.is_none() {
        // 不成立の組み合わせは free pool へ戻し、搭載済みカーゴだけで継続する。
        for (is_combat, entry) in selected_entries.drain(..) {
            if entry.2.can_capture {
                assigned_capture_count = assigned_capture_count.saturating_sub(1);
            }
            if is_combat {
                free.combat.push(entry);
            } else {
                free.infantry.push(entry);
            }
        }
        cargo_entities.truncate(initially_loaded_count);
        if cargo_entities.is_empty() {
            return None;
        }
        pickup_position = Some(transport_position);
    }

    let phase = if requires_pickup && !selected_entries.is_empty() {
        TransportPhase::Pickup
    } else {
        TransportPhase::Transit
    };
    let mut draft = SquadDraft::transport(transport_entity, cargo_entities, phase);
    draft.target_island = Some(search.target_island);
    draft.target = search.target_position;
    draft.pickup_position = pickup_position;
    draft.allow_partial_departure = true;
    proposal.create(draft);
    Some(assigned_capture_count)
}

/// campaignへ予約されていない搭載済み輸送機は、別assignmentへ転用せず安全に降ろす。
fn unload_unreserved_loaded_transports(
    world: &World,
    proposal: &mut AssignmentProposal,
    free: &mut FreeUnits,
) {
    free.transports
        .sort_by_key(|(entity, position, _)| (position.y, position.x, entity.to_bits()));
    let mut i = 0;
    while i < free.transports.len() {
        let (transport_entity, transport_position, _) = free.transports[i].clone();
        let mut cargo_entities = world
            .get::<crate::components::CargoCapacity>(transport_entity)
            .map(|cargo| cargo.loaded.clone())
            .unwrap_or_default();
        cargo_entities.sort_by_key(|entity| entity.to_bits());
        cargo_entities.dedup();

        if cargo_entities.is_empty() {
            i += 1;
            continue;
        }
        let mut draft =
            SquadDraft::transport(transport_entity, cargo_entities, TransportPhase::Drop);
        draft.pickup_position = Some(transport_position);
        proposal.create(draft);
        free.transports.remove(i);
    }
}

/// 島ごとの敵unit数（搭載中を除く）。
fn enemy_count_by_island(board: &FormationBoard<'_>) -> HashMap<IslandId, usize> {
    let mut counts = HashMap::default();
    for unit in board.enemy_units().filter(|unit| !unit.transported) {
        if let Some(island) = board.island_map.get_island_at(&unit.position) {
            counts
                .entry(island.id)
                .and_modify(|count: &mut usize| *count = count.saturating_add(1))
                .or_insert(1_usize);
        }
    }
    counts
}

/// V4: 通常の不足数を満たした後も、未所有施設と敵の反撃余地が残る島には
/// 損耗補充用の予備便を割り当てる。free poolへ残った歩兵・輸送役を
/// 生産拠点周辺の恒常的な遊兵にしない。
fn assign_capture_depth_transports(
    world: &World,
    proposal: &mut AssignmentProposal,
    board: &FormationBoard,
    campaign_assignments: &[IslandCampaignAssignment],
    free: &mut FreeUnits,
    connectivity: &mut TerrainConnectivity,
) {
    if free.transports.is_empty() || free.infantry.is_empty() {
        return;
    }
    let enemy_counts = enemy_count_by_island(board);
    for assignment in campaign_assignments {
        if free.transports.is_empty() || free.infantry.is_empty() {
            break;
        }
        if matches!(
            assignment.decision,
            crate::ai::island_campaign::IslandCampaignDecision::Observe
                | crate::ai::island_campaign::IslandCampaignDecision::Withdraw
                | crate::ai::island_campaign::IslandCampaignDecision::Defend
        ) {
            continue;
        }
        // 占領対象ごとの担当に加え、敵がいる島では最大2体の損耗補充を用意する。
        // 価格や固定倍率ではなく、未所有施設数と観測敵Entity数から導く。
        let desired_capture_depth = assignment
            .capture_target_positions
            .len()
            .max(1)
            .saturating_add(
                enemy_counts
                    .get(&assignment.island_id)
                    .copied()
                    .unwrap_or(0)
                    .min(2),
            );
        let mut remaining = desired_capture_depth.saturating_sub(assignment.capture_entities.len());
        while remaining > 0 && !free.transports.is_empty() && !free.infantry.is_empty() {
            let Some(loaded) = load_capture_depth_transport(
                world,
                proposal,
                board,
                assignment,
                remaining,
                free,
                connectivity,
            ) else {
                break;
            };
            remaining = remaining.saturating_sub(loaded);
        }
    }
}

/// 予備便1便を組む。成立したら積んだ歩兵数、輸送役か歩兵が見つからなければNone。
fn load_capture_depth_transport(
    world: &World,
    proposal: &mut AssignmentProposal,
    board: &FormationBoard,
    assignment: &IslandCampaignAssignment,
    remaining: usize,
    free: &mut FreeUnits,
    connectivity: &mut TerrainConnectivity,
) -> Option<usize> {
    let transport_index = free.transports.iter().position(|(_, position, stats)| {
        board
            .island_map
            .islands
            .iter()
            .find(|island| island.id == assignment.island_id)
            .and_then(|island| {
                get_target_position_for_island(
                    board.map,
                    board.registry,
                    island,
                    *position,
                    stats.movement_type,
                )
            })
            .is_some()
    })?;
    let (transport, transport_position, transport_stats) = free.transports.remove(transport_index);
    let capacity = world
        .get::<crate::components::CargoCapacity>(transport)
        .map(|cargo| cargo.max as usize)
        .unwrap_or(0);
    let mut compatible = free
        .infantry
        .iter()
        .enumerate()
        .filter(|(_, (_, _, stats))| {
            transport_stats
                .loadable_unit_types
                .contains(&stats.unit_type)
        })
        .filter_map(|(index, (entity, _, _))| {
            cargo_pickup_rank(
                world,
                board.player,
                transport_position,
                &transport_stats,
                *entity,
                connectivity,
            )
            .map(|rank| (rank, index))
        })
        .collect::<Vec<_>>();
    compatible.sort_by_key(|(rank, _)| *rank);
    let mut selected_indices = compatible
        .into_iter()
        .take(capacity.min(remaining))
        .map(|(_, index)| index)
        .collect::<Vec<_>>();
    selected_indices.sort_unstable_by(|left, right| right.cmp(left));
    let mut cargo_entities = Vec::new();
    for index in selected_indices {
        cargo_entities.push(free.infantry.swap_remove(index).0);
    }
    cargo_entities.sort_by_key(|entity| entity.to_bits());
    if cargo_entities.is_empty() {
        free.transports
            .push((transport, transport_position, transport_stats));
        return None;
    }
    let Some(pickup_position) = select_pickup_position(
        world,
        board.player,
        transport_position,
        &transport_stats,
        &cargo_entities,
        connectivity,
    ) else {
        for cargo in cargo_entities {
            if let (Some(position), Some(stats)) = (
                world.get::<GridPosition>(cargo).copied(),
                world.get::<UnitStats>(cargo).cloned(),
            ) {
                free.infantry.push((cargo, position, stats));
            }
        }
        free.transports
            .push((transport, transport_position, transport_stats));
        return None;
    };

    let loaded = cargo_entities.len();
    let is_assault =
        assignment.decision == crate::ai::island_campaign::IslandCampaignDecision::Assault;
    let mut draft = SquadDraft::transport(transport, cargo_entities, TransportPhase::Pickup);
    draft.pickup_position = Some(pickup_position);
    draft.target_island = Some(assignment.island_id);
    draft.target = Some(assignment.target_position);
    draft.allow_partial_departure = !is_assault;
    draft.departure_authorized = assignment.operation_ready || !is_assault;
    proposal.create(draft);
    Some(loaded)
}

/// 新しいSquadへ、目標へのターン距離が近い順にfreeの戦闘unitを最大3体入れる。
fn assign_nearest_combat_units(
    members: &mut BTreeSet<Entity>,
    board: &FormationBoard,
    target: GridPosition,
    free_combat: &mut Vec<FreeUnit>,
    turn_cache: &mut TurnDistanceCache,
) {
    free_combat.sort_by_key(|(_, pos, stats)| board.turn_distance(*pos, target, stats, turn_cache));
    let assign_count = free_combat.len().min(3);
    for _ in 0..assign_count {
        let (entity, _, _) = free_combat.remove(0);
        members.insert(entity);
    }
}

/// #53 (V3): 敵生産施設への奪取部隊は同時に1個まで (集中スピアヘッド)。
/// 複数同時に編成すると前線から兵力が抜かれすぎて防衛線が崩壊し、
/// 逐次投入の各個撃破で全部隊が溶けるため
const MAX_CONCURRENT_FACILITY_CAPTURES: usize = 1;

/// C. 占領部隊の立ち上げ（Expansionフェーズ）。
///
/// #53 (V3): 中立拠点に加えて敵所有拠点も奪取目標に含める。従来は中立拠点のみが
/// 対象だったため、中立を取り切ると占領部隊が編成されなくなり、敵領土の奪取が発生しなかった。
fn form_capture_squads(
    world: &mut World,
    proposal: &mut AssignmentProposal,
    board: &FormationBoard,
    situation: &StrategicSituation,
    paused_islands: &HashSet<IslandId>,
    policy: PlanningPolicy,
    free: &mut FreeUnits,
) {
    let player = board.player;
    let is_v3 = board.is_v3;
    let mut capture_targets: Vec<GridPosition> =
        situation.unowned_properties.iter().cloned().collect();
    if is_v3 {
        capture_targets.extend(situation.enemy_properties.iter().cloned());
        capture_targets.retain(|target| {
            board
                .island_map
                .get_island_at(target)
                .is_none_or(|island| !paused_islands.contains(&island.id))
        });
    }
    let is_enemy_facility = |t: &GridPosition| -> bool {
        board
            .ownership
            .get(t)
            .is_some_and(|o| o.is_some() && *o != Some(player))
            && board
                .map
                .get_terrain(t.x, t.y)
                .is_some_and(|tr| board.registry.is_production_facility(tr.as_str()))
    };
    let mut active_facility_captures = proposal
        .squads()
        .filter(|(_, squad)| {
            squad.mutable
                && squad.mission_type == MissionType::Capture
                && squad.members > 0
                && squad.target.as_ref().is_some_and(is_enemy_facility)
        })
        .count();
    // 最寄りのフリー歩兵から近い順に割り当てる (HashSet 順の非決定性も排除し、
    // 限られた歩兵を近い目標へ優先的に振り分ける)。
    // #53 (V3): 工場・首都などの生産施設は敵の増援源であり、奪取すれば
    // 前線の消耗戦を根本から崩せるため、多少遠くても優先する
    policy.sort_capture_targets(
        world,
        player,
        &mut capture_targets,
        &board.capture_context(&free.infantry),
    );

    // #53 (V3): 敵生産施設への突入 (スピアヘッド) は戦力優勢 (Assault フェーズ)
    // のときのみ許可する。拮抗・劣勢時に前線から兵力を抜くと防衛線が崩壊する
    let allow_facility_capture = situation.phase == crate::ai::strategy::GamePhase::Assault;

    for target in &capture_targets {
        let target_is_enemy_facility = is_enemy_facility(target);
        if target_is_enemy_facility
            && (!allow_facility_capture
                || active_facility_captures >= MAX_CONCURRENT_FACILITY_CAPTURES)
        {
            continue;
        }
        let Some(target_island) = board.island_map.get_island_at(target) else {
            continue;
        };
        let is_on_base_island = board.base_islands.contains(&target_island.id);
        // 到達可能な同島歩兵の選択方策だけをpolicyへ渡す。
        let same_island_infantry = policy.select_capture_infantry(
            &board.capture_context(&free.infantry),
            target_island.id,
            target,
        );
        // V3の洋上移動はportfolio transportだけが担当し、generic Captureは到達可能な現地要員に限定する。
        let can_capture = same_island_infantry.is_some()
            || !is_v3 && is_on_base_island && !free.infantry.is_empty();
        if !can_capture {
            continue;
        }
        if proposal.has_squad(MissionType::Capture, *target) {
            continue;
        }

        let mut draft = SquadDraft::new(MissionType::Capture, MissionPhase::Forming);
        draft.target = Some(*target);
        if is_v3 {
            draft.target_island = Some(target_island.id);
        }
        // 同じ島にいる歩兵がいればそれを優先、いなければ free_infantry の最後の歩兵を割り当てる
        let assigned_index = same_island_infantry.unwrap_or_else(|| free.infantry.len() - 1);
        let (infantry, _, infantry_stats) = free.infantry.remove(assigned_index);
        draft.members.insert(infantry);

        if is_v3 && target_is_enemy_facility {
            assign_facility_capture_escorts(
                world,
                &mut draft.members,
                *target,
                infantry_stats.max_movement,
                &mut free.combat,
            );
            active_facility_captures += 1;
        }
        proposal.create(draft);
    }
}

/// #53 (V3): 敵生産施設 (工場・首都等) への奪取部隊には護衛の戦闘ユニットを最大2両随伴させる。
///
/// 敵の生産圏は防御が厚く、単独の歩兵では到達前に撃破されて奪取が成立しないため。
/// 護衛は「歩兵に随伴できる機動力があり戦闘可能」なユニットに限定する
/// (鈍足の砲台や弾切れ・損傷ユニットを組み込むと部隊が機能しないため)
fn assign_facility_capture_escorts(
    world: &World,
    members: &mut BTreeSet<Entity>,
    target: GridPosition,
    infantry_movement: u32,
    free_combat: &mut Vec<FreeUnit>,
) {
    // 対象拠点から「遠い順」に並べ替え、末尾 (最も近いユニット) から pop() で
    // 取り出すことで Vec 先頭削除による O(N) シフトを避ける (#57 レビュー対応)。
    free_combat.sort_by_key(|(_, pos, _)| {
        std::cmp::Reverse(pos.x.abs_diff(target.x) + pos.y.abs_diff(target.y))
    });
    let mut assigned = 0;
    // 護衛条件を満たさず不採用としたユニットは後で free pool に戻す
    let mut rejected = Vec::new();
    while assigned < 2 {
        let Some((entity, position, stats)) = free_combat.pop() else {
            break;
        };
        let hp = world
            .get::<crate::components::Health>(entity)
            .map(|h| h.current)
            .unwrap_or(100);
        let (ammo1, ammo2) = world
            .get::<crate::components::Ammo>(entity)
            .map(|a| (a.ammo1, a.ammo2))
            .unwrap_or((u32::MAX, u32::MAX));
        if escort_is_eligible(&stats, infantry_movement, hp, ammo1, ammo2) {
            members.insert(entity);
            assigned += 1;
        } else {
            rejected.push((entity, position, stats));
        }
    }
    // 護衛に採用しなかった候補は他部隊が利用できるよう free pool へ戻す
    free_combat.append(&mut rejected);
}

/// D. 攻撃部隊の立ち上げ: 敵クラスタごとに攻撃Squadを作る。
fn form_attack_squads(
    proposal: &mut AssignmentProposal,
    board: &FormationBoard,
    enemy_clusters: &[crate::ai::cluster::AttackCluster],
    free: &mut FreeUnits,
    turn_cache: &mut TurnDistanceCache,
) {
    for cluster in enemy_clusters {
        if free.combat.is_empty() {
            break;
        }
        if proposal.has_squad(MissionType::Attack, cluster.center) {
            continue;
        }
        let mut draft = SquadDraft::new(MissionType::Attack, MissionPhase::Forming);
        draft.target = Some(cluster.center);
        assign_nearest_combat_units(
            &mut draft.members,
            board,
            cluster.center,
            &mut free.combat,
            turn_cache,
        );
        proposal.create(draft);
    }
}

/// 1部隊の最大人数。これを超えると後続ユニットが前線で渋滞を起こす。
const MAX_SQUAD_SIZE: usize = 3;

/// 定員管理付きの余剰戦闘unitの割り当て（交通渋滞解消）。
///
/// 定員未満の既存Attack/Defense部隊のうち最も近いものへ吸収し、すべて定員なら
/// 最も近い敵クラスタ（15ターン超なら最寄りの未所有・敵物件）へ新しい攻撃部隊を作る。
fn absorb_surplus_combat_units(
    proposal: &mut AssignmentProposal,
    board: &FormationBoard,
    enemy_clusters: &[crate::ai::cluster::AttackCluster],
    free: &mut FreeUnits,
    turn_cache: &mut TurnDistanceCache,
) {
    while let Some((entity, pos, stats)) = free.combat.pop() {
        if let Some(slot) = nearest_open_squad(proposal, board, pos, &stats, turn_cache) {
            proposal.join(slot, entity);
            continue;
        }
        // 目標がまったく存在しない場合は放置（SoloFallback として機能）
        if let Some(target) = second_wave_target(board, enemy_clusters, pos, &stats, turn_cache) {
            let mut draft = SquadDraft::new(MissionType::Attack, MissionPhase::Forming);
            draft.target = Some(target);
            draft.members.insert(entity);
            proposal.create(draft);
        }
    }
}

/// 定員未満の既存Attack/Defense部隊（新設提案を含む）のうち、目標へのターン距離が最も近いもの。
fn nearest_open_squad(
    proposal: &AssignmentProposal,
    board: &FormationBoard,
    pos: GridPosition,
    stats: &UnitStats,
    turn_cache: &mut TurnDistanceCache,
) -> Option<SquadSlot> {
    let mut best_slot = None;
    let mut min_dist = crate::ai::turn_distance::TurnDistance {
        turns: u32::MAX,
        used_mp: u32::MAX,
    };
    for (slot, squad) in proposal.squads() {
        if !squad.mutable
            || !(squad.mission_type == MissionType::Attack
                || squad.mission_type == MissionType::Defense)
            || squad.members >= MAX_SQUAD_SIZE
        {
            continue;
        }
        let Some(target) = squad.target else {
            continue;
        };
        let dist = board.turn_distance(pos, target, stats, turn_cache);
        if dist < min_dist {
            min_dist = dist;
            best_slot = Some(slot);
        }
    }
    best_slot
}

/// 既存部隊がすべて定員のときに新設する第2波の目標。
fn second_wave_target(
    board: &FormationBoard,
    enemy_clusters: &[crate::ai::cluster::AttackCluster],
    pos: GridPosition,
    stats: &UnitStats,
    turn_cache: &mut TurnDistanceCache,
) -> Option<GridPosition> {
    let mut nearest_cluster_dist = crate::ai::turn_distance::TurnDistance {
        turns: u32::MAX,
        used_mp: u32::MAX,
    };
    let mut nearest_cluster_center = None;
    for cluster in enemy_clusters {
        let dist = board.turn_distance(pos, cluster.center, stats, turn_cache);
        if dist < nearest_cluster_dist {
            nearest_cluster_dist = dist;
            nearest_cluster_center = Some(cluster.center);
        }
    }
    if nearest_cluster_dist.turns <= 15 {
        return nearest_cluster_center;
    }
    // 敵が15ターン以上遠い場合や存在しない場合は、最寄りの拠点（未占領または敵所有）を目標にする。
    // 処理時間を削減するためマンハッタン距離による概算を使用
    let mut nearest_prop_dist = u32::MAX;
    let mut nearest_prop_pos = None;
    for (p_pos, p_owner) in &board.ownership {
        if *p_owner == Some(board.player) {
            continue;
        }
        let dist = (pos.x as i32 - p_pos.x as i32).unsigned_abs()
            + (pos.y as i32 - p_pos.y as i32).unsigned_abs();
        if dist < nearest_prop_dist {
            nearest_prop_dist = dist;
            nearest_prop_pos = Some(*p_pos);
        }
    }
    nearest_prop_pos.or(nearest_cluster_center)
}

/// 5. 仕上げ: V4の所属修復、全任務を構築した後の一度だけの正規化、Roadmap観測。
fn finalize_formation(
    world: &mut World,
    manager: &mut SquadManager,
    player: PlayerId,
    policy: PlanningPolicy,
    adopted: &IslandCampaignPortfolio,
    campaign_assignments: &[IslandCampaignAssignment],
    is_v4: bool,
) {
    if is_v4 {
        // campaign/deployment/generic候補の勝者を先に確定する。正規化前の重複Squadを
        // 「任務あり」と数えると、この直後に敗者参照が落ちてownerだけの遊兵が残る。
        // 手番末の固定点を作る有界線形パスであり、行動候補ごとの走査ではない。
        reconcile_unique_operation_assignments(world, manager, player);
        reconnect_v4_operation_fixed_point(world, manager, player, campaign_assignments);
        assign_explicit_reserve_squads(world, manager, player, campaign_assignments);
    }
    // Roadmap観測位置は方策で異なるため、正規化の前後で分ける。
    policy.before_final_reconcile(world, player, adopted, manager, is_v4);
    // campaign・deployment・汎用任務をすべて構築した後に一度だけ正規化する。
    // これ以降の行動選択は重複Squadを見ず、所有者照会も平均O(1)になる。
    reconcile_unique_operation_assignments(world, manager, player);
    policy.after_final_reconcile(world, player, adopted, manager, is_v4);
}
