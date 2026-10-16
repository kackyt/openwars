//! 小規模V4生産が読む盤面snapshot。

use super::capital_route::force_demand::split_bridge_route_objectives;
use crate::ai::v4::capital_route::node_operation::capital_route_assault_authorized_from_dag;
use crate::ai::v4::capital_route::topology::{
    armored_entry_bridge_gates, same_land_capital_front_reached,
};

use crate::ai::v4::*;

impl BoardScan {
    pub(in crate::ai::v4) fn collect_extended(
        world: &mut World,
        player_id: PlayerId,
    ) -> Option<Self> {
        if !crate::ai::code_path::ext_enabled("v4/mod.rs::collect") {
            return Self::collect(world, player_id);
        }
        let map = Arc::new(world.get_resource::<Map>()?.clone());
        let unit_registry = world.get_resource::<UnitRegistry>()?.clone();
        let damage_chart = Arc::new(world.get_resource::<DamageChart>()?.clone());
        let master_data = Arc::new(world.get_resource::<MasterDataRegistry>()?.clone());
        let current_turn = world
            .get_resource::<MatchState>()
            .map_or(1, |state| state.current_turn_number.0);
        let opening_policy = match crate::ai::strategy_profile::profile_for(world, player_id) {
            crate::ai::strategy_profile::V4StrategyProfile::Standard => {
                OpeningProductionPolicy::Standard
            }
            crate::ai::strategy_profile::V4StrategyProfile::SmallMapExpansion => {
                OpeningProductionPolicy::SmallMapExpansion
            }
        };
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
        let pending_capture_replacements = world
            .get_resource::<victory_roadmap::VictoryRoadmapRegistry>()
            .map(|roadmap| {
                roadmap
                    .operations_for(player_id)
                    .into_iter()
                    .filter(|operation| {
                        roadmap.capture_replacement_pending(player_id, operation.island_id)
                    })
                    .map(|operation| operation.island_id)
                    .collect::<HashSet<_>>()
            })
            .unwrap_or_default();
        let cached_portfolio = world
            .get_resource::<AdoptedTurnPortfolio>()
            .and_then(|cache| cache.campaign_portfolio(player_id))
            .cloned();
        let portfolio = cached_portfolio.unwrap_or_else(|| {
            let mut portfolio =
                crate::ai::island_campaign_analysis::analyze_island_campaign(world, player_id);
            crate::ai::v4::strategy_pipeline::prepare_strategy(world, player_id, &mut portfolio);
            portfolio
        });
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
            let mut q = world.query::<(
                Entity,
                &GridPosition,
                &Faction,
                &UnitStats,
                Option<&Health>,
                Option<&CargoCapacity>,
                Option<&Transporting>,
            )>();
            for (entity, pos, faction, stats, health, cargo, transporting) in q.iter(world) {
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
            let mut q = world.query::<(Entity, &GridPosition, &Property)>();
            let mut enemy_capitals = HashMap::default();
            for (entity, pos, prop) in q.iter(world) {
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
            for (_entity, pos, prop) in q.iter(world) {
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

/// 橋で分岐する同一陸塊の局地前線を、Gateごとの独立Operationへ分ける。
/// 必要占領兵と既存所属は一意に配分し、同じEntityを複数ルートへ二重計上しない。
/// 【一時的な診断専用】campaign quota（required_capture_survivors）と
/// protected供給（protected_capture_entities）が同一objectiveに帰属しているかを
/// 突き合わせるための監査。`OPENWARS_CAMPAIGN_SCOPE_AUDIT` 設定時のみ出力する。
pub(in crate::ai::v4) fn campaign_scope_audit_enabled() -> bool {
    std::env::var_os("OPENWARS_CAMPAIGN_SCOPE_AUDIT").is_some()
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
