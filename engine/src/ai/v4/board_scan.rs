//! 小規模V4生産が読む盤面snapshot。

use super::capital_route::force_demand::split_bridge_route_objectives;
use crate::ai::v4::capital_route::node_operation::capital_route_assault_authorized_from_dag;
use crate::ai::v4::capital_route::topology::{
    armored_entry_bridge_gates, same_land_capital_front_reached,
};
use crate::ai::v4::trace::campaign_scope_audit_enabled;

use crate::ai::v4::{
    AdoptedTurnPortfolio, Arc, CampaignPlanningObjective, CargoCapacity, DamageChart,
    EnemyFacilitySnapshot, EnemyProductionForecastTrace, Entity, Faction, GridPosition, HashMap,
    HashSet, Health, Map, MasterDataRegistry, MatchState, OccupantInfo, OpeningProductionPolicy,
    OperationKind, PlayerId, Players, ProductionPolicy, Property, Terrain, Transporting,
    UnitRegistry, UnitSnapshot, UnitStats, UnitType, World, logistics_plan, strategy_pipeline,
    victory_roadmap,
};

impl BoardScan {
    pub(in crate::ai::v4) fn collect_extended(
        world: &World,
        player_id: PlayerId,
        prepared: Option<&strategy_pipeline::ProductionScanPreparation>,
        queries: &BoardScanQueries,
    ) -> Option<Self> {
        let map = Arc::new(world.get_resource::<Map>()?.clone());
        let unit_registry = world.get_resource::<UnitRegistry>()?.clone();
        let damage_chart = Arc::new(world.get_resource::<DamageChart>()?.clone());
        let master_data = Arc::new(world.get_resource::<MasterDataRegistry>()?.clone());
        let current_turn = world
            .get_resource::<MatchState>()
            .map_or(1, |state| state.current_turn_number.0);
        let prepared = prepared?;
        let opening_policy = prepared.opening_policy;
        let funds = world
            .get_resource::<Players>()?
            .0
            .iter()
            .find(|p| p.id == player_id)
            .map(|p| p.funds)?;
        let logistics_plan = &prepared.logistics_plan;
        let logistics_ranks = logistics_plan
            .as_ref()
            .map(|plan| {
                plan.route_islands
                    .iter()
                    .enumerate()
                    .map(|(rank, island)| (*island, u32::try_from(rank).unwrap_or(u32::MAX)))
                    .collect::<HashMap<_, _>>()
            })
            .unwrap_or_default();
        let pending_capture_replacements = &prepared.pending_capture_replacements;
        let portfolio = &prepared.portfolio;
        let mut campaign_objectives: Vec<CampaignPlanningObjective> = portfolio
            .defenses
            .iter()
            .chain(portfolio.active_offensives.iter())
            .map(|assignment| {
                let eta = portfolio
                    .islands
                    .iter()
                    .find(|assessment| assessment.island_id == assignment.island_id)
                    .and_then(|assessment| assessment.friendly_capture_eta);
                if campaign_scope_audit_enabled() {
                    eprintln!(
                        "CAMPAIGN_SCOPE_AUDIT stage=producer turn={} player={:?} island={:?} decision={:?} anchor={:?} requirement_capture_units={} purchase_shortfall_capture_units={} capture_entities={:?} capture_target_positions={:?}",
                        current_turn,
                        player_id,
                        assignment.island_id,
                        assignment.decision,
                        assignment.target_position,
                        assignment.requirement.capture_units,
                        assignment.purchase_shortfall.capture_units,
                        assignment.capture_entities,
                        assignment.capture_target_positions,
                    );
                }
                CampaignPlanningObjective {
                    island_id: assignment.island_id,
                    kind: if assignment.decision
                        == crate::ai::island_campaign::IslandCampaignDecision::Defend
                    {
                        OperationKind::Defense
                    } else {
                        OperationKind::Capture
                    },
                    anchor: assignment.target_position,
                    objective_properties: assignment.capture_target_positions.clone(),
                    capture_eta: eta,
                    required_capture_survivors: usize::try_from(
                        assignment.requirement.capture_units,
                    )
                    .unwrap_or(usize::MAX),
                    logistics_rank: logistics_ranks.get(&assignment.island_id).copied(),
                    forced_target_enemies: HashSet::default(),
                    protected_capture_entities: world.get_resource::<crate::ai::planning::decision::AdoptedTurnPortfolio>()
                    .and_then(|cache| cache.committed_capture_reservations(player_id, assignment.island_id))
                    .cloned().unwrap_or_else(|| assignment.capture_entities.iter().copied().collect()),
                    capture_replacement_pending: pending_capture_replacements
                        .contains(&assignment.island_id),
                    staging_anchor: assignment.target_position,
                    execution_authorized: true,
                }
            })
            .collect();

        // 同一ターン内で生産に失敗した施設を除外するためのクールダウン
        let cooldown: HashSet<(usize, usize)> = world
            .get_resource::<crate::ai::runtime::production_queue::AiProductionCooldown>()
            .map(|c| c.0.clone())
            .unwrap_or_default();

        // --- ユニットの走査 ---
        let mut occupied = HashSet::default();
        let mut my_units = Vec::new();
        let mut enemy_units = Vec::new();
        {
            for UnitRow {
                entity,
                pos,
                faction,
                stats,
                health,
                cargo,
                transporting,
            } in unit_rows(world, queries)
            {
                // 輸送中のユニットは盤面を占有しない
                if transporting.is_some() {
                    continue;
                }
                occupied.insert(*pos);
                let snapshot = UnitSnapshot {
                    entity: Some(entity),
                    pos: *pos,
                    hp: health.map_or(100, |h| h.current),
                    free_cargo: cargo.map_or(stats.max_cargo, |c| {
                        stats.max_cargo.saturating_sub(c.loaded.len() as u32)
                    }),
                    stats: stats.clone(),
                };
                if faction.0 == player_id {
                    my_units.push(snapshot);
                } else {
                    enemy_units.push(snapshot);
                }
            }
        }

        // ECSのテーブル順は部品の追加や登録順で変わる。戦闘予測の攻撃順へ
        // 格納順を持ち込まず、実EntityのID順を計画入力の正規順序とする。
        my_units.sort_unstable_by_key(|unit| unit.entity.map(Entity::to_bits));
        enemy_units.sort_unstable_by_key(|unit| unit.entity.map(Entity::to_bits));

        // --- 拠点の走査 ---
        let mut capital_pos = None;
        let mut open_properties = Vec::new();
        let mut neutral_properties = Vec::new();
        let mut facilities = Vec::new();
        let mut production_facilities = Vec::new();
        let mut enemy_income = 0u32;
        let mut enemy_production_slots = 0u32;
        let mut enemy_facilities = Vec::new();
        let mut my_income = 0u32;
        let mut owned_airport_count = 0u32;
        let mut owned_properties = Vec::new();
        let mut enemy_capital = world
            .get_resource::<victory_roadmap::VictoryRoadmapRegistry>()
            .and_then(|registry| registry.roadmap(player_id))
            .and_then(|roadmap| roadmap.enemy_capital);
        let mut property_entities = HashMap::default();
        {
            let properties = property_rows(world, queries).collect::<Vec<_>>();
            let mut enemy_capitals = HashMap::default();
            for PropertyRow { entity, pos, prop } in properties.iter().copied() {
                property_entities.insert(*pos, entity);
                if prop.owner_id == Some(player_id) && prop.terrain == Terrain::Capital {
                    capital_pos = Some(*pos);
                } else if let Some(owner) = prop.owner_id
                    && prop.terrain == Terrain::Capital
                {
                    enemy_capitals.insert(owner, *pos);
                    enemy_capital.get_or_insert(*pos);
                }
            }
            for PropertyRow { pos, prop, .. } in properties.iter().copied() {
                let income = master_data.landscape_income(prop.terrain.as_str());
                let is_facility = master_data.is_production_facility(prop.terrain.as_str());
                match prop.owner_id {
                    Some(owner) if owner == player_id => {
                        my_income = my_income.saturating_add(income);
                        owned_properties.push((*pos, prop.terrain));
                        if prop.terrain == Terrain::Airport
                            && crate::systems::production::is_within_production_range(
                                capital_pos.as_slice(),
                                pos.x,
                                pos.y,
                                map.topology,
                            )
                        {
                            owned_airport_count = owned_airport_count.saturating_add(1);
                        }
                        let is_in_production_range = is_facility
                            && crate::systems::production::is_within_production_range(
                                capital_pos.as_slice(),
                                pos.x,
                                pos.y,
                                map.topology,
                            );
                        if is_in_production_range {
                            production_facilities.push((*pos, prop.terrain));
                        }
                        // 現在手番に命令できるのは、全生産施設のうち空いているものだけ。
                        if is_in_production_range
                            && !occupied.contains(pos)
                            && !cooldown.contains(&(pos.x, pos.y))
                        {
                            facilities.push((*pos, prop.terrain));
                        }
                    }
                    Some(owner) => {
                        enemy_income = enemy_income.saturating_add(income);
                        let is_usable_enemy_facility = is_facility
                            && enemy_capitals.get(&owner).is_some_and(|capital| {
                                crate::systems::production::is_within_production_range(
                                    std::slice::from_ref(capital),
                                    pos.x,
                                    pos.y,
                                    map.topology,
                                )
                            });
                        if is_usable_enemy_facility {
                            enemy_production_slots += 1;
                            enemy_facilities.push(EnemyFacilitySnapshot {
                                pos: *pos,
                                terrain: prop.terrain,
                            });
                        }
                        open_properties.push(*pos);
                    }
                    None => {
                        open_properties.push(*pos);
                        neutral_properties.push(*pos);
                    }
                }
            }
        }

        if facilities.is_empty() {
            return None;
        }

        let mut available_types: Vec<(UnitType, UnitStats)> = unit_registry
            .0
            .iter()
            .map(|(unit_type, stats)| (*unit_type, stats.clone()))
            .collect();
        // HashMapの走査順で同値候補の採否が変わらないよう、兵種IDも固定する。
        available_types.sort_unstable_by_key(|(unit_type, _)| unit_type.as_str());

        let island_map = crate::ai::islands::IslandMap::analyze(&map);
        let enemy_capital_island = enemy_capital
            .and_then(|position| island_map.get_island_at(&position))
            .map(|island| island.id);
        let home_island = capital_pos
            .and_then(|position| island_map.get_island_at(&position))
            .map(|island| island.id);
        if home_island.is_some()
            && home_island == enemy_capital_island
            && let (Some(home), Some(capital)) = (capital_pos, enemy_capital)
        {
            let gates = armored_entry_bridge_gates(&map, &master_data, home, capital);
            let unit_targets = world
                .get_resource::<crate::ai::squad::SquadManager>()
                .map(|manager| {
                    manager
                        .squads
                        .iter()
                        .filter(|squad| squad.plan.owner_id == Some(player_id))
                        .flat_map(|squad| {
                            squad.plan.target.into_iter().flat_map(move |target| {
                                squad.members.iter().map(move |entity| (*entity, target))
                            })
                        })
                        .collect::<HashMap<_, _>>()
                })
                .unwrap_or_default();
            campaign_objectives = split_bridge_route_objectives(
                &map,
                campaign_objectives,
                &gates,
                &my_units,
                &unit_targets,
            );
        }
        if campaign_scope_audit_enabled() {
            for (index, objective) in campaign_objectives.iter().enumerate() {
                eprintln!(
                    "CAMPAIGN_SCOPE_AUDIT stage=post_split turn={} player={:?} idx={} island={:?} kind={:?} anchor={:?} properties={:?} quota={} protected={:?}",
                    current_turn,
                    player_id,
                    index,
                    objective.island_id,
                    objective.kind,
                    objective.anchor,
                    objective.objective_properties,
                    objective.required_capture_survivors,
                    objective.protected_capture_entities,
                );
            }
        }
        let completed_route_island = logistics_plan.as_ref().and_then(|plan| {
            plan.route_islands
                .iter()
                .rev()
                .copied()
                .find(|island| !plan.selected_islands.contains(island))
        });
        let property_positions = owned_properties
            .iter()
            .map(|(position, _)| *position)
            .chain(open_properties.iter().copied())
            .collect::<HashSet<_>>();
        let staging_anchor = enemy_capital.and_then(|capital| {
            let forward_property = owned_properties
                .iter()
                .filter(|(position, _)| {
                    completed_route_island.is_none_or(|expected| {
                        island_map
                            .get_island_at(position)
                            .is_some_and(|island| island.id == expected)
                    })
                })
                .min_by_key(|(position, _)| {
                    map.distance(position.x, position.y, capital.x, capital.y)
                })
                .or_else(|| {
                    owned_properties.iter().min_by_key(|(position, _)| {
                        map.distance(position.x, position.y, capital.x, capital.y)
                    })
                })
                .map(|(position, _)| *position)?;
            let expected_island = island_map
                .get_island_at(&forward_property)
                .map(|island| island.id);
            // 集結地点は施設上へ置かない。前線拠点に隣接する空き地を優先し、
            // 完全編成待ちのCombat Entityが生産口を塞ぐことを防ぐ。
            (0..map.height)
                .flat_map(|y| (0..map.width).map(move |x| GridPosition { x, y }))
                .filter(|position| !property_positions.contains(position))
                .filter(|position| !occupied.contains(position))
                .filter(|position| {
                    island_map.get_island_at(position).map(|island| island.id) == expected_island
                })
                .min_by_key(|position| {
                    (
                        map.distance(
                            position.x,
                            position.y,
                            forward_property.x,
                            forward_property.y,
                        ),
                        map.distance(position.x, position.y, capital.x, capital.y),
                        position.y,
                        position.x,
                    )
                })
                .or(Some(forward_property))
        });
        let capital_assault_authorized = enemy_capital_island.is_some_and(|capital_island| {
            if home_island != Some(capital_island) {
                return logistics_plan
                    .as_ref()
                    .is_some_and(|plan| plan.selected_islands.is_empty());
            }
            let Some(home) = capital_pos else {
                return false;
            };
            let Some(capital) = enemy_capital else {
                return false;
            };
            let route_distance = map.distance(home.x, home.y, capital.x, capital.y).max(1);
            let forward_distance = owned_properties
                .iter()
                .filter(|(position, _)| {
                    island_map
                        .get_island_at(position)
                        .is_some_and(|island| island.id == capital_island)
                })
                .map(|(position, _)| map.distance(position.x, position.y, capital.x, capital.y))
                .min()
                .unwrap_or(u32::MAX);
            // DAGが存在する地図では、側方の所有地ではなく各routeの先頭未確保拠点を
            // 正本にする。Topology未初期化の単体テストなどだけ、従来の距離判定へ
            // フォールバックする。
            capital_route_assault_authorized_from_dag(world, player_id, capital_island)
                .unwrap_or_else(|| {
                    same_land_capital_front_reached(route_distance, forward_distance)
                })
        });
        if let (Some(capital), Some(capital_island), Some(staging_anchor)) =
            (enemy_capital, enemy_capital_island, staging_anchor)
        {
            campaign_objectives.push(CampaignPlanningObjective {
                island_id: capital_island,
                kind: OperationKind::AssaultCapital,
                anchor: capital,
                objective_properties: vec![capital],
                capture_eta: None,
                required_capture_survivors: 0,
                logistics_rank: None,
                // 同じ陸塊の全敵を強制対象にしない。局地Captureと首都のanchorへ
                // 到着できる敵を最寄りOperationへ一意に割り当てる。
                forced_target_enemies: HashSet::default(),
                protected_capture_entities: HashSet::default(),
                capture_replacement_pending: false,
                staging_anchor,
                execution_authorized: capital_assault_authorized,
            });
        }

        Some(BoardScan {
            map,
            master_data,
            damage_chart,
            current_turn,
            opening_policy: Some(opening_policy),
            funds,
            capital_position: capital_pos,
            free_facilities: facilities,
            production_facilities,
            available_types,
            my_units,
            enemy_units,
            owned_airport_count,
            property_entities,
            open_properties,
            neutral_properties,
            enemy_income,
            enemy_production_slots,
            enemy_facilities,
            my_income,
            campaign_objectives,
            capital_assault_authorized,
            capital_staging_anchor: staging_anchor,
            enemy_production_forecast: EnemyProductionForecastTrace::default(),
        })
    }
}

/// 生産候補の経路計算に必要な、現在の占有マスを組み立てる。
///
/// `BoardScan` は相手のPlayerIdを保持しないため、相手側には自軍と異なるダミーIDを
/// 置く。距離計算が必要とするのは「自軍か否か」だけであり、敵勢力どうしを区別する
/// 必要はない。
pub(in crate::ai::v4) fn scanned_occupants(
    scan: &BoardScan,
    player_id: PlayerId,
) -> HashMap<(usize, usize), OccupantInfo> {
    let opposing_player = PlayerId(player_id.0.saturating_add(1));
    scan.my_units
        .iter()
        .map(|unit| (unit, player_id))
        .chain(scan.enemy_units.iter().map(|unit| (unit, opposing_player)))
        .map(|(unit, owner)| {
            (
                (unit.pos.x, unit.pos.y),
                OccupantInfo {
                    player_id: owner,
                    is_transport: unit.stats.max_cargo > 0,
                    unit_type: unit.stats.unit_type,
                    loadable_types: unit.stats.loadable_unit_types.clone(),
                    free_slots: unit.free_cargo,
                },
            )
        })
        .collect()
}

/// 盤面から生産判断に必要な観測量をすべて取り出したもの。
pub(in crate::ai::v4) struct BoardScan {
    /// 同じ手番の複数作戦へ渡す不変盤面。作戦ごとの全量cloneを避ける。
    pub(in crate::ai::v4) map: Arc<Map>,
    pub(in crate::ai::v4) master_data: Arc<MasterDataRegistry>,
    pub(in crate::ai::v4) damage_chart: Arc<DamageChart>,
    pub(in crate::ai::v4) current_turn: u32,
    /// テスト用の直接構築では未指定を標準側として扱う。実盤面走査では必ず設定する。
    pub(in crate::ai::v4) opening_policy: Option<OpeningProductionPolicy>,
    pub(in crate::ai::v4) funds: u32,
    /// 砲台の首都側防衛可否を、作戦名でなく実座標から判定する。
    pub(in crate::ai::v4) capital_position: Option<GridPosition>,
    /// 生産可能な施設（未占有・生産範囲内・クールダウン対象外）
    pub(in crate::ai::v4) free_facilities: Vec<(GridPosition, Terrain)>,
    /// 次ターン以降に空くことを見込める、生産範囲内の全所有施設。
    pub(in crate::ai::v4) production_facilities: Vec<(GridPosition, Terrain)>,
    pub(in crate::ai::v4) available_types: Vec<(UnitType, UnitStats)>,
    pub(in crate::ai::v4) my_units: Vec<UnitSnapshot>,
    pub(in crate::ai::v4) enemy_units: Vec<UnitSnapshot>,
    /// 首都の生産範囲内にある所有空港総数（占有中を含む）
    pub(in crate::ai::v4) owned_airport_count: u32,
    /// 盤面上の物件位置から安定ID（Entity）を引く辞書
    pub property_entities: HashMap<GridPosition, Entity>,
    /// 自軍が保有していない拠点（中立・敵）
    pub(in crate::ai::v4) open_properties: Vec<GridPosition>,
    /// まだどの勢力も所有していない拠点。初動の物件レース対象を敵領から分離する。
    pub(in crate::ai::v4) neutral_properties: Vec<GridPosition>,
    pub(in crate::ai::v4) enemy_income: u32,
    pub(in crate::ai::v4) enemy_production_slots: u32,
    pub(in crate::ai::v4) enemy_facilities: Vec<EnemyFacilitySnapshot>,
    pub(in crate::ai::v4) my_income: u32,
    /// 島campaignが実行を決めた作戦。汎用clusterへ事後照合せず、このanchorと
    /// IslandIdをCombat生産計画の入力としてそのまま使う。
    pub(in crate::ai::v4) campaign_objectives: Vec<CampaignPlanningObjective>,
    /// 首都攻略部隊を前進させてよいのは、固定兵站経路の確保後だけである。
    pub(in crate::ai::v4) capital_assault_authorized: bool,
    /// 編成中の首都攻略部隊を、生産施設から退避させて集結する自軍拠点。
    pub(in crate::ai::v4) capital_staging_anchor: Option<GridPosition>,
    /// 敵の実生産履歴から推定した、次手番の生産量。将来脅威の見込み額にだけ使う。
    pub(in crate::ai::v4) enemy_production_forecast: EnemyProductionForecastTrace,
}

impl BoardScan {
    pub(in crate::ai::v4) fn collect(
        world: &World,
        player_id: PlayerId,
        prepared: Option<&strategy_pipeline::ProductionScanPreparation>,
        queries: &BoardScanQueries,
        policy: ProductionPolicy,
    ) -> Option<Self> {
        // 小規模マップのV4は extended 側の実装を使う。
        if policy == ProductionPolicy::SmallMap {
            return Self::collect_extended(world, player_id, prepared, queries);
        }
        let map = Arc::new(world.get_resource::<Map>()?.clone());
        let unit_registry = world.get_resource::<UnitRegistry>()?.clone();
        let damage_chart = Arc::new(world.get_resource::<DamageChart>()?.clone());
        let master_data = Arc::new(world.get_resource::<MasterDataRegistry>()?.clone());
        let funds = world
            .get_resource::<Players>()?
            .0
            .iter()
            .find(|p| p.id == player_id)
            .map(|p| p.funds)?;
        let logistics_plan = world
            .get_resource::<logistics_plan::V4LogisticsPlanRegistry>()
            .and_then(|registry| registry.plan(player_id))
            .cloned();
        let logistics_ranks = logistics_plan
            .as_ref()
            .map(|plan| {
                plan.route_islands
                    .iter()
                    .enumerate()
                    .map(|(rank, island)| (*island, u32::try_from(rank).unwrap_or(u32::MAX)))
                    .collect::<HashMap<_, _>>()
            })
            .unwrap_or_default();
        let mut campaign_objectives: Vec<CampaignPlanningObjective> = world
            .get_resource::<AdoptedTurnPortfolio>()
            .and_then(|cache| cache.campaign_portfolio(player_id))
            .map(|portfolio| {
                portfolio
                    .defenses
                    .iter()
                    .chain(portfolio.active_offensives.iter())
                    .map(|assignment| {
                        let eta = portfolio
                            .islands
                            .iter()
                            .find(|assessment| assessment.island_id == assignment.island_id)
                            .and_then(|assessment| assessment.friendly_capture_eta);
                        CampaignPlanningObjective {
                            capture_replacement_pending: false,
                            island_id: assignment.island_id,
                            kind: if assignment.decision
                                == crate::ai::island_campaign::IslandCampaignDecision::Defend
                            {
                                OperationKind::Defense
                            } else {
                                OperationKind::Capture
                            },
                            anchor: assignment.target_position,
                            objective_properties: assignment.capture_target_positions.clone(),
                            capture_eta: eta,
                            required_capture_survivors: usize::try_from(
                                assignment.requirement.capture_units,
                            )
                            .unwrap_or(usize::MAX),
                            logistics_rank: logistics_ranks.get(&assignment.island_id).copied(),
                            forced_target_enemies: HashSet::default(),
                            protected_capture_entities: world.get_resource::<crate::ai::planning::decision::AdoptedTurnPortfolio>()
                    .and_then(|cache| cache.committed_capture_reservations(player_id, assignment.island_id))
                    .cloned().unwrap_or_else(|| assignment.capture_entities.iter().copied().collect()),
                            staging_anchor: assignment.target_position,
                            execution_authorized: true,
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();

        // 同一ターン内で生産に失敗した施設を除外するためのクールダウン
        let cooldown: HashSet<(usize, usize)> = world
            .get_resource::<crate::ai::runtime::production_queue::AiProductionCooldown>()
            .map(|c| c.0.clone())
            .unwrap_or_default();

        // --- ユニットの走査 ---
        let mut occupied = HashSet::default();
        let mut my_units = Vec::new();
        let mut enemy_units = Vec::new();
        {
            for UnitRow {
                entity,
                pos,
                faction,
                stats,
                health,
                cargo,
                transporting,
            } in unit_rows(world, queries)
            {
                // 輸送中のユニットは盤面を占有しない
                if transporting.is_some() {
                    continue;
                }
                occupied.insert(*pos);
                let snapshot = UnitSnapshot {
                    entity: Some(entity),
                    pos: *pos,
                    hp: health.map_or(100, |h| h.current),
                    free_cargo: cargo.map_or(stats.max_cargo, |c| {
                        stats.max_cargo.saturating_sub(c.loaded.len() as u32)
                    }),
                    stats: stats.clone(),
                };
                if faction.0 == player_id {
                    my_units.push(snapshot);
                } else {
                    enemy_units.push(snapshot);
                }
            }
        }

        // --- 拠点の走査 ---
        let mut capital_pos = None;
        let mut open_properties = Vec::new();
        let mut facilities = Vec::new();
        let mut production_facilities = Vec::new();
        let mut enemy_income = 0u32;
        let mut enemy_production_slots = 0u32;
        let mut enemy_facilities = Vec::new();
        let mut my_income = 0u32;
        let mut owned_airport_count = 0u32;
        let mut owned_properties = Vec::new();
        let mut enemy_capital = world
            .get_resource::<victory_roadmap::VictoryRoadmapRegistry>()
            .and_then(|registry| registry.roadmap(player_id))
            .and_then(|roadmap| roadmap.enemy_capital);
        {
            let properties = property_rows(world, queries).collect::<Vec<_>>();
            let mut enemy_capitals = HashMap::default();
            for PropertyRow { pos, prop, .. } in properties.iter().copied() {
                if prop.owner_id == Some(player_id) && prop.terrain == Terrain::Capital {
                    capital_pos = Some(*pos);
                } else if let Some(owner) = prop.owner_id
                    && prop.terrain == Terrain::Capital
                {
                    enemy_capitals.insert(owner, *pos);
                    enemy_capital.get_or_insert(*pos);
                }
            }
            for PropertyRow { pos, prop, .. } in properties.iter().copied() {
                let income = master_data.landscape_income(prop.terrain.as_str());
                let is_facility = master_data.is_production_facility(prop.terrain.as_str());
                match prop.owner_id {
                    Some(owner) if owner == player_id => {
                        my_income = my_income.saturating_add(income);
                        owned_properties.push((*pos, prop.terrain));
                        if prop.terrain == Terrain::Airport
                            && crate::systems::production::is_within_production_range(
                                capital_pos.as_slice(),
                                pos.x,
                                pos.y,
                                map.topology,
                            )
                        {
                            owned_airport_count = owned_airport_count.saturating_add(1);
                        }
                        let is_in_production_range = is_facility
                            && crate::systems::production::is_within_production_range(
                                capital_pos.as_slice(),
                                pos.x,
                                pos.y,
                                map.topology,
                            );
                        if is_in_production_range {
                            production_facilities.push((*pos, prop.terrain));
                        }
                        // 現在手番に命令できるのは、全生産施設のうち空いているものだけ。
                        if is_in_production_range
                            && !occupied.contains(pos)
                            && !cooldown.contains(&(pos.x, pos.y))
                        {
                            facilities.push((*pos, prop.terrain));
                        }
                    }
                    Some(owner) => {
                        enemy_income = enemy_income.saturating_add(income);
                        let is_usable_enemy_facility = is_facility
                            && enemy_capitals.get(&owner).is_some_and(|capital| {
                                crate::systems::production::is_within_production_range(
                                    std::slice::from_ref(capital),
                                    pos.x,
                                    pos.y,
                                    map.topology,
                                )
                            });
                        if is_usable_enemy_facility {
                            enemy_production_slots += 1;
                            enemy_facilities.push(EnemyFacilitySnapshot {
                                pos: *pos,
                                terrain: prop.terrain,
                            });
                        }
                        open_properties.push(*pos);
                    }
                    None => open_properties.push(*pos),
                }
            }
        }

        if facilities.is_empty() {
            return None;
        }

        let available_types: Vec<(UnitType, UnitStats)> = unit_registry
            .0
            .iter()
            .map(|(unit_type, stats)| (*unit_type, stats.clone()))
            .collect();

        let island_map = crate::ai::islands::IslandMap::analyze(&map);
        let enemy_capital_island = enemy_capital
            .and_then(|position| island_map.get_island_at(&position))
            .map(|island| island.id);
        let home_island = capital_pos
            .and_then(|position| island_map.get_island_at(&position))
            .map(|island| island.id);
        let completed_route_island = logistics_plan.as_ref().and_then(|plan| {
            plan.route_islands
                .iter()
                .rev()
                .copied()
                .find(|island| !plan.selected_islands.contains(island))
        });
        let property_positions = owned_properties
            .iter()
            .map(|(position, _)| *position)
            .chain(open_properties.iter().copied())
            .collect::<HashSet<_>>();
        let staging_anchor = enemy_capital.and_then(|capital| {
            let forward_property = owned_properties
                .iter()
                .filter(|(position, _)| {
                    completed_route_island.is_none_or(|expected| {
                        island_map
                            .get_island_at(position)
                            .is_some_and(|island| island.id == expected)
                    })
                })
                .min_by_key(|(position, _)| {
                    map.distance(position.x, position.y, capital.x, capital.y)
                })
                .or_else(|| {
                    owned_properties.iter().min_by_key(|(position, _)| {
                        map.distance(position.x, position.y, capital.x, capital.y)
                    })
                })
                .map(|(position, _)| *position)?;
            let expected_island = island_map
                .get_island_at(&forward_property)
                .map(|island| island.id);
            // 集結地点は施設上へ置かない。前線拠点に隣接する空き地を優先し、
            // 完全編成待ちのCombat Entityが生産口を塞ぐことを防ぐ。
            (0..map.height)
                .flat_map(|y| (0..map.width).map(move |x| GridPosition { x, y }))
                .filter(|position| !property_positions.contains(position))
                .filter(|position| !occupied.contains(position))
                .filter(|position| {
                    island_map.get_island_at(position).map(|island| island.id) == expected_island
                })
                .min_by_key(|position| {
                    (
                        map.distance(
                            position.x,
                            position.y,
                            forward_property.x,
                            forward_property.y,
                        ),
                        map.distance(position.x, position.y, capital.x, capital.y),
                        position.y,
                        position.x,
                    )
                })
                .or(Some(forward_property))
        });
        let capital_assault_authorized = enemy_capital_island.is_some_and(|capital_island| {
            if home_island != Some(capital_island) {
                return logistics_plan
                    .as_ref()
                    .is_some_and(|plan| plan.selected_islands.is_empty());
            }
            let Some(home) = capital_pos else {
                return false;
            };
            let Some(capital) = enemy_capital else {
                return false;
            };
            let owned_count = owned_properties
                .iter()
                .filter(|(position, _)| {
                    island_map
                        .get_island_at(position)
                        .is_some_and(|island| island.id == capital_island)
                })
                .count();
            let enemy_count = open_properties
                .iter()
                .filter(|position| {
                    island_map
                        .get_island_at(position)
                        .is_some_and(|island| island.id == capital_island)
                })
                .count();
            let route_distance = map.distance(home.x, home.y, capital.x, capital.y).max(1);
            let forward_distance = owned_properties
                .iter()
                .filter(|(position, _)| {
                    island_map
                        .get_island_at(position)
                        .is_some_and(|island| island.id == capital_island)
                })
                .map(|(position, _)| map.distance(position.x, position.y, capital.x, capital.y))
                .min()
                .unwrap_or(u32::MAX);
            // 同一大陸であること自体はGo条件にしない。局地拠点を奪って所有数で
            // 優位を作り、少なくとも戦線が両首都の中間まで進んだときだけ本隊を解放する。
            owned_count > enemy_count && forward_distance.saturating_mul(2) <= route_distance
        });
        if let (Some(capital), Some(capital_island), Some(staging_anchor)) =
            (enemy_capital, enemy_capital_island, staging_anchor)
        {
            campaign_objectives.push(CampaignPlanningObjective {
                capture_replacement_pending: false,
                island_id: capital_island,
                kind: OperationKind::AssaultCapital,
                anchor: capital,
                objective_properties: vec![capital],
                capture_eta: None,
                required_capture_survivors: 0,
                logistics_rank: None,
                // 同じ陸塊の全敵を強制対象にしない。局地Captureと首都のanchorへ
                // 到着できる敵を最寄りOperationへ一意に割り当てる。
                forced_target_enemies: HashSet::default(),
                protected_capture_entities: HashSet::default(),
                staging_anchor,
                execution_authorized: capital_assault_authorized,
            });
        }

        Some(BoardScan {
            current_turn: 0,
            opening_policy: None,
            capital_position: None,
            property_entities: HashMap::default(),
            neutral_properties: Vec::new(),
            enemy_production_forecast: Default::default(),
            map,
            master_data,
            damage_chart,
            funds,
            free_facilities: facilities,
            production_facilities,
            available_types,
            my_units,
            enemy_units,
            owned_airport_count,
            open_properties,
            enemy_income,
            enemy_production_slots,
            enemy_facilities,
            my_income,
            campaign_objectives,
            capital_assault_authorized,
            capital_staging_anchor: staging_anchor,
        })
    }

    /// 指定した拠点で `unit_type` を生産できるか。
    pub(in crate::ai::v4) fn can_produce(&self, terrain: Terrain, unit_type: UnitType) -> bool {
        self.master_data
            .can_produce_unit(terrain.as_str(), unit_type)
    }

    /// 生産可能なユニットのうち、占領可能で最も安いものを「基準占領ユニット」とする。
    /// 展開リードタイムや到達可能性はこのユニットの足で測る。
    pub(in crate::ai::v4) fn reference_capture_unit(&self) -> Option<&UnitStats> {
        self.available_types
            .iter()
            .filter(|(unit_type, stats)| {
                stats.can_capture
                    && self
                        .production_facilities
                        .iter()
                        .any(|(_, terrain)| self.can_produce(*terrain, *unit_type))
            })
            .min_by_key(|(_, stats)| stats.cost)
            .map(|(_, stats)| stats)
    }
}

// QueryStateは入口でだけ準備し、snapshotは同じWorldの登録済みQueryを読み取る。
// Bevyのcomponent index順を使うため、table ID順へ独自に並べ替えない。
type UnitQuery = bevy_ecs::query::QueryState<(
    Entity,
    &'static GridPosition,
    &'static Faction,
    &'static UnitStats,
    Option<&'static Health>,
    Option<&'static CargoCapacity>,
    Option<&'static Transporting>,
)>;
type PropertyQuery =
    bevy_ecs::query::QueryState<(Entity, &'static GridPosition, &'static Property)>;

/// 1回の収集に使うQueryのメタデータ。ゲーム状態や戦略判断は保持しない。
pub(in crate::ai::v4) struct BoardScanQueries {
    units: UnitQuery,
    properties: PropertyQuery,
}

impl BoardScanQueries {
    /// World更新を許す呼出し元でQueryを初期化し、以後の収集は共有参照だけで行う。
    pub(in crate::ai::v4) fn prepare(world: &mut World) -> Self {
        Self {
            units: world.query(),
            properties: world.query(),
        }
    }
}

/// 所有者・位置・能力のそろったユニット行。輸送中かの判断は収集側へ残す。
struct UnitRow<'a> {
    entity: Entity,
    pos: &'a GridPosition,
    faction: &'a Faction,
    stats: &'a UnitStats,
    health: Option<&'a Health>,
    cargo: Option<&'a CargoCapacity>,
    transporting: Option<&'a Transporting>,
}

fn unit_rows<'a>(
    world: &'a World,
    queries: &'a BoardScanQueries,
) -> impl Iterator<Item = UnitRow<'a>> {
    queries.units.iter_manual(world).map(
        |(entity, pos, faction, stats, health, cargo, transporting)| UnitRow {
            entity,
            pos,
            faction,
            stats,
            health,
            cargo,
            transporting,
        },
    )
}

/// 首都走査と施設走査が同じ順序の不変行を共有する。
#[derive(Clone, Copy)]
struct PropertyRow<'a> {
    entity: Entity,
    pos: &'a GridPosition,
    prop: &'a Property,
}

fn property_rows<'a>(
    world: &'a World,
    queries: &'a BoardScanQueries,
) -> impl Iterator<Item = PropertyRow<'a>> {
    queries
        .properties
        .iter_manual(world)
        .map(|(entity, pos, prop)| PropertyRow { entity, pos, prop })
}

#[cfg(test)]
mod tests;
