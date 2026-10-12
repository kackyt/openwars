//! 拡張経路（小規模マップ戦略・非V4）の実装。
//!
//! 親モジュールはmain互換の実装を持ち、本文の異なる関数は
//! `crate::ai::code_path::is_extended()` でここへ委譲する。

#[allow(unused_imports)]
use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) fn candidate_target_position(
    map: &Map,
    registry: &MasterDataRegistry,
    island: &Island,
    properties: &[PropertySnapshot],
    units: &[UnitSnapshot],
    player_id: PlayerId,
    decision: IslandCampaignDecision,
    existing_operation: Option<&ExistingCampaignOperation>,
) -> Option<GridPosition> {
    let origins = capture_front_origins(island, properties, units, player_id, existing_operation);
    let mut targets = properties
        .iter()
        .filter(|snapshot| island.tiles.contains(&snapshot.position))
        .filter(|snapshot| {
            snapshot.property.max_capture_points > 0
                && snapshot.property.owner_id != Some(player_id)
        })
        .map(|snapshot| snapshot.position)
        .collect::<Vec<_>>();
    let target_priority = |position: GridPosition| {
        capture_front_priority(
            map, registry, island, properties, units, player_id, &origins, position,
        )
    };
    targets.sort_unstable_by_key(|position| target_priority(*position));
    let nearest = targets.first().copied();

    // 防衛作戦のアンカーは、奪取対象ではなく守るべき自軍施設である。敵が航空機だけで
    // 島外から接近している場合でも、候補自体を失うとThreatened島の防衛編成が作れない。
    if decision == IslandCampaignDecision::Defend {
        let mut defensive_targets = properties
            .iter()
            .filter(|snapshot| island.tiles.contains(&snapshot.position))
            .filter(|snapshot| {
                snapshot.property.max_capture_points > 0
                    && snapshot.property.owner_id == Some(player_id)
            })
            .map(|snapshot| snapshot.position)
            .collect::<Vec<_>>();
        defensive_targets.sort_unstable_by_key(|position| target_priority(*position));
        defensive_targets.dedup();
        if let Some(operation) = existing_operation
            && defensive_targets.contains(&operation.target_position)
        {
            return Some(operation.target_position);
        }
        if let Some(target) = defensive_targets.first().copied() {
            return Some(target);
        }
    }

    // 進行中目標も無条件には固定しない。より近い前線より2手以上遠くなった目標は
    // 敵の再占領や担当損耗で前提が崩れたものとして、現在の最短前線へ戻す。
    if let (Some(operation), Some(nearest)) = (existing_operation, nearest)
        && targets.contains(&operation.target_position)
    {
        let distance_from_front = |position: GridPosition| target_priority(position).1;
        if origins.is_empty()
            || distance_from_front(operation.target_position)
                <= distance_from_front(nearest).saturating_add(2)
        {
            return Some(operation.target_position);
        }
    }
    // 未所有の施設がなくなった島へ、任意の島タイルを輸送目標としてはいけない。
    // とくに島の先頭タイルが浅瀬だと、揚陸艇はcargoを降ろせない浅瀬まで進んで
    // Drop候補を失う。継続する必要があるのは、実際に残る地上敵への対処だけである。
    let mut ground_enemy_targets = units
        .iter()
        .filter(|unit| unit.faction != player_id && unit.health.current > 0)
        .filter(|unit| unit.transporting.is_none())
        .filter(|unit| island.tiles.contains(&unit.position))
        .filter(|unit| {
            !matches!(
                unit.stats.movement_type,
                MovementType::Air | MovementType::Ship
            )
        })
        // 輸送cargoが上陸できない浅瀬などを、敵の位置だけを理由に目的地へしない。
        .filter(|unit| {
            map.get_terrain(unit.position.x, unit.position.y)
                .and_then(|terrain| {
                    get_valid_movement_cost(registry, MovementType::Infantry, terrain)
                })
                .is_some()
        })
        .map(|unit| unit.position)
        .collect::<Vec<_>>();
    ground_enemy_targets.sort_unstable_by_key(|position| target_priority(*position));
    ground_enemy_targets.dedup();

    nearest.or_else(|| {
        // 進行中の作戦も、現在の地上敵を指している場合だけ継続する。既に消えた
        // 施設や任意タイルを保持すると、輸送隊だけが残るゾンビ作戦になる。
        existing_operation
            .filter(|operation| ground_enemy_targets.contains(&operation.target_position))
            .map(|operation| operation.target_position)
            .or_else(|| ground_enemy_targets.first().copied())
    })
}

#[allow(clippy::too_many_arguments)]
pub(super) fn candidate_capture_target_positions(
    map: &Map,
    registry: &MasterDataRegistry,
    island: &Island,
    properties: &[PropertySnapshot],
    units: &[UnitSnapshot],
    player_id: PlayerId,
    primary_target: GridPosition,
    existing_operation: Option<&ExistingCampaignOperation>,
) -> Vec<GridPosition> {
    let origins = capture_front_origins(island, properties, units, player_id, existing_operation);
    let mut targets: Vec<_> = properties
        .iter()
        .filter(|snapshot| island.tiles.contains(&snapshot.position))
        .filter(|snapshot| {
            snapshot.property.max_capture_points > 0
                && snapshot.property.owner_id != Some(player_id)
        })
        .map(|snapshot| snapshot.position)
        .collect();
    let target_priority = |position: GridPosition| {
        capture_front_priority(
            map, registry, island, properties, units, player_id, &origins, position,
        )
    };
    targets.sort_by_key(|position| target_priority(*position));

    let own_capital = properties
        .iter()
        .find(|snapshot| {
            island.tiles.contains(&snapshot.position)
                && snapshot.property.terrain == Terrain::Capital
                && snapshot.property.owner_id == Some(player_id)
        })
        .map(|snapshot| snapshot.position);
    let enemy_capital = properties
        .iter()
        .find(|snapshot| {
            island.tiles.contains(&snapshot.position)
                && snapshot.property.terrain == Terrain::Capital
                && snapshot
                    .property
                    .owner_id
                    .is_some_and(|owner| owner != player_id)
        })
        .map(|snapshot| snapshot.position);

    let route_fronts = if let (Some(own_cap), Some(enemy_cap)) = (own_capital, enemy_capital) {
        crate::ai::v4::armored_capital_route_fronts(map, registry, own_cap, enemy_cap)
    } else {
        Vec::new()
    };

    // 既に足場がある陸塊では、最短前線とその隣接帯だけを同時実行対象にする。
    // 全未所有施設は毎手番再観測するため捨てず、前線取得後に次の帯へ昇格する。
    // 足場のない島は輸送波の目的目的一覧を縮めると再上陸が遅れるため従来どおり保持する。
    if !origins.is_empty() {
        if route_fronts.len() >= 2 {
            // 同一陸塊で複数の進軍軸（上段・下段等）がある場合、各ルートの最寄り拠点を
            // 独立に保持する。単一前線で一括切断すると、片方のルートの占領が消滅する。
            let nearest_route = |position: GridPosition| {
                route_fronts
                    .iter()
                    .enumerate()
                    .min_by_key(|(route, front)| {
                        (
                            map.distance(position.x, position.y, front.x, front.y),
                            *route,
                        )
                    })
                    .map_or(0, |(route, _)| route)
            };
            let mut min_distance_by_route: crate::collections::HashMap<usize, u32> =
                crate::collections::HashMap::default();
            for target in &targets {
                let route = nearest_route(*target);
                let distance = target_priority(*target).1;
                min_distance_by_route
                    .entry(route)
                    .and_modify(|min| *min = (*min).min(distance))
                    .or_insert(distance);
            }
            targets.retain(|target| {
                let route = nearest_route(*target);
                let distance = target_priority(*target).1;
                let min_distance = min_distance_by_route.get(&route).copied().unwrap_or(0);
                distance <= min_distance.saturating_add(2)
            });
        } else if let Some(nearest_distance) =
            targets.first().map(|target| target_priority(*target).1)
        {
            targets
                .retain(|target| target_priority(*target).1 <= nearest_distance.saturating_add(2));
        }
    }
    if let Some(index) = targets
        .iter()
        .position(|position| *position == primary_target)
    {
        targets.remove(index);
        targets.insert(0, primary_target);
    }
    targets
}

#[allow(clippy::too_many_arguments)]
pub(super) fn collect_campaign_resource_pool(
    player_id: PlayerId,
    map: &Map,
    registry: &MasterDataRegistry,
    island_map: &IslandMap,
    properties: &[PropertySnapshot],
    units: &[UnitSnapshot],
    operations: &[ExistingCampaignOperation],
    produced_assignments: &HashMap<Entity, IslandId>,
    roadmap_assignments: &HashMap<Entity, IslandId>,
    assigned_transport_phases: &HashMap<Entity, AssignedTransportState>,
    unavailable_entities: &HashSet<Entity>,
    available_funds: u32,
    audit_turn: u32,
) -> CampaignResourcePool {
    let unit_by_entity: HashMap<_, _> = units.iter().map(|unit| (unit.entity, unit)).collect();
    let assigned_by_entity =
        merge_campaign_assignment_sources(roadmap_assignments, operations, produced_assignments);
    let mut assigned_entities_by_island: HashMap<IslandId, HashSet<Entity>> = HashMap::default();
    for (entity, island_id) in &assigned_by_entity {
        assigned_entities_by_island
            .entry(*island_id)
            .or_default()
            .insert(*entity);
    }

    let mut candidates = Vec::new();
    // 地形連結成分は移動種別ごとに1回だけ構築する。候補マスごとに再構築すると、
    // ユニット数×全島タイル数だけマップ全走査が発生する。
    let mut terrain_connectivity = TerrainConnectivity::default();
    for unit in units.iter().filter(|unit| unit.faction == player_id) {
        let assigned_island = assigned_by_entity.get(&unit.entity).copied();
        let transport_state = assigned_transport_phases.get(&unit.entity);
        let loaded_cargo_is_assignment_owned = assigned_island.is_some_and(|island_id| {
            assigned_entities_by_island
                .get(&island_id)
                .is_some_and(|assigned| {
                    unit.loaded_cargo_entities.iter().all(|cargo| {
                        assigned.contains(cargo)
                            && unit_by_entity.get(cargo).is_some_and(|cargo_unit| {
                                cargo_unit.transporting == Some(unit.entity)
                            })
                    })
                })
        });
        let transport_is_available = match transport_state {
            None => unit.loaded_cargo_entities.is_empty(),
            Some(AssignedTransportState::Forming(target, cargo_is_assigned)) => {
                target.is_some()
                    && *target == assigned_island
                    && *cargo_is_assigned
                    && loaded_cargo_is_assignment_owned
            }
            Some(AssignedTransportState::Phase(phase, target, cargo_is_assigned)) => {
                (is_live_transport_phase(*phase)
                    || (*phase == TransportPhase::Return && unit.loaded_cargo_entities.is_empty()))
                    && target.is_some()
                    && *target == assigned_island
                    && ((*phase == TransportPhase::Return && unit.loaded_cargo_entities.is_empty())
                        || (*cargo_is_assigned && loaded_cargo_is_assignment_owned))
            }
            Some(AssignedTransportState::Other) => false,
        };
        let is_offshore_transport = matches!(
            unit.stats.unit_type,
            UnitType::TransportHelicopter | UnitType::Lander
        );
        let loaded_cargo_entities = if loaded_cargo_is_assignment_owned {
            unit.loaded_cargo_entities.clone()
        } else {
            Vec::new()
        };
        let available_cargo_slots = unit
            .free_cargo_slots
            .saturating_add(loaded_cargo_entities.len() as u32);
        // targetless safe Drop、Completed、関連外cargo、実capacity無しの輸送役は再予約しない。
        // 空Returnだけは次便のpickupへ直接向け直し、帰投だけの空走を挟まない。
        // 【一時的な診断専用】除外理由を先に判定してから継続する。
        let exclusion_reason = if unavailable_entities.contains(&unit.entity) {
            Some("unavailable_set")
        } else if is_offshore_transport && unit.fuel.is_some_and(|fuel| fuel == 0) {
            Some("transport_fuel_zero")
        } else if is_offshore_transport && !transport_is_available {
            Some("transport_state_unavailable")
        } else if is_offshore_transport && available_cargo_slots == 0 {
            Some("transport_no_cargo_slot")
        } else if unit.transporting.is_some() && assigned_island.is_none() {
            Some("loaded_cargo_without_assignment")
        } else {
            None
        };
        if pool_audit_enabled() {
            eprintln!(
                "POOL_AUDIT stage=producer turn={} player={:?} entity={:?} type={:?} can_capture={} pos=({},{}) island={:?} assigned_island={:?} transporting={:?} excluded={:?}",
                audit_turn,
                player_id,
                unit.entity,
                unit.stats.unit_type,
                unit.stats.can_capture,
                unit.position.x,
                unit.position.y,
                island_map
                    .get_island_at(&unit.position)
                    .map(|island| island.id),
                assigned_island,
                unit.transporting,
                exclusion_reason,
            );
        }
        if exclusion_reason.is_some() {
            continue;
        }
        let island_id = island_map
            .get_island_at(&unit.position)
            .map(|island| island.id);
        let can_secure_local_property = unit.stats.can_capture
            && unit.transporting.is_none()
            && island_id.is_some_and(|island_id| {
                properties.iter().any(|snapshot| {
                    snapshot.property.owner_id != Some(player_id)
                        && island_map
                            .get_island_at(&snapshot.position)
                            .is_some_and(|island| island.id == island_id)
                        && terrain_connectivity.is_reachable(
                            map,
                            registry,
                            (unit.position.x, unit.position.y),
                            (snapshot.position.x, snapshot.position.y),
                            unit.stats.movement_type,
                        )
                })
            });
        let mut reachable_positions: Vec<_> = island_map
            .islands
            .iter()
            .flat_map(|island| island.tiles.iter())
            .filter(|target| {
                terrain_connectivity.is_reachable(
                    map,
                    registry,
                    (unit.position.x, unit.position.y),
                    (target.x, target.y),
                    unit.stats.movement_type,
                )
            })
            .copied()
            .collect();
        reachable_positions.sort_by_key(|position| (position.y, position.x));
        candidates.push(CampaignUnitCandidate {
            entity: unit.entity,
            unit_type: unit.stats.unit_type,
            cost: unit.stats.cost,
            can_capture: unit.stats.can_capture,
            can_secure_local_property,
            available_cargo_slots,
            loaded_cargo_entities,
            loadable_unit_types: unit.stats.loadable_unit_types.clone(),
            is_transporting: unit.transporting.is_some(),
            reachable_positions,
            island_id,
            assigned_island,
        });
    }
    candidates.sort_by_key(|unit| {
        (
            unit.assigned_island.map(|island| island.0),
            unit.island_id.map(|island| island.0),
            unit.cost,
            campaign_unit_type_rank(unit.unit_type),
            unit.entity.to_bits(),
        )
    });
    CampaignResourcePool {
        available_funds,
        units: candidates,
    }
}

/// 既存の実行任務へ予約済みのEntityを共有資源から除外して島嶼作戦を分析します。
pub(crate) fn analyze_island_campaign_excluding(
    world: &mut World,
    player_id: PlayerId,
    reserved_entities: &HashSet<Entity>,
) -> IslandCampaignPortfolio {
    if !crate::ai::code_path::ext_enabled(
        "island_campaign_analysis.rs::analyze_island_campaign_excluding",
    ) {
        return super::analyze_island_campaign_excluding(world, player_id, reserved_entities);
    }
    let facts = collect_island_campaign_facts(world, player_id);
    let map = world.resource::<Map>().clone();
    let island_map = world
        .get_resource::<IslandMap>()
        .cloned()
        .unwrap_or_else(|| IslandMap::analyze(&map));
    let manager = world
        .get_resource::<SquadManager>()
        .cloned()
        .unwrap_or_default();
    let registry = world
        .get_resource::<MasterDataRegistry>()
        .cloned()
        .unwrap_or_default();
    let properties = collect_property_snapshots(world);
    let (friendly_income_per_turn, enemy_income_per_turn) = properties.iter().fold(
        (0_u32, 0_u32),
        |(friendly_income, enemy_income), snapshot| {
            let income = registry.landscape_income(snapshot.property.terrain.as_str());
            match snapshot.property.owner_id {
                Some(owner) if owner == player_id => {
                    (friendly_income.saturating_add(income), enemy_income)
                }
                Some(_) => (friendly_income, enemy_income.saturating_add(income)),
                None => (friendly_income, enemy_income),
            }
        },
    );
    let home_property = properties.iter().find(|snapshot| {
        snapshot.property.owner_id == Some(player_id)
            && snapshot.property.terrain == Terrain::Capital
    });
    let home_position = home_property.map(|snapshot| snapshot.position);
    let home_island = home_property
        .and_then(|snapshot| island_map.get_island_at(&snapshot.position))
        .map(|island| island.id);
    let enemy_capital = properties
        .iter()
        .find(|snapshot| {
            snapshot.property.terrain == Terrain::Capital
                && snapshot.property.owner_id != Some(player_id)
        })
        .and_then(|snapshot| {
            island_map
                .get_island_at(&snapshot.position)
                .map(|island| (island.id, snapshot.position))
        });
    let mut forward_sustainment_coverage = CampaignSustainmentCoverage::default();
    if let Some(home_island) = home_island {
        for snapshot in properties.iter().filter(|snapshot| {
            snapshot.property.owner_id == Some(player_id)
                && snapshot.property.max_capture_points > 0
                && island_map
                    .get_island_at(&snapshot.position)
                    .is_some_and(|island| island.id != home_island)
        }) {
            match snapshot.property.terrain {
                Terrain::Capital | Terrain::City | Terrain::Factory => {
                    forward_sustainment_coverage.ground = true
                }
                Terrain::Airport => forward_sustainment_coverage.air = true,
                Terrain::Port => forward_sustainment_coverage.sea = true,
                _ => {}
            }
        }
    }
    let producible_transports =
        producible_campaign_transports(&map, &registry, &island_map, &properties, player_id);
    let factions = collect_faction_snapshots(world);
    let units = campaign_unit_snapshots(&manager, collect_unit_snapshots(world), &factions);
    let assigned_transport_phases = collect_assigned_transport_phases(&manager, &units, player_id);
    let (operations, _) = collect_existing_operations(&island_map, &manager, &units, player_id);
    let operations_by_island: HashMap<_, _> = operations
        .iter()
        .filter(|operation| operation_has_live_capability(operation))
        .map(|operation| (operation.island_id, operation.clone()))
        .collect();
    let available_funds = world
        .get_resource::<Players>()
        .and_then(|players| players.0.iter().find(|player| player.id == player_id))
        .map_or(0, |player| player.funds);
    let produced_assignments = world
        .get_resource::<crate::ai::v4::campaign_execution::V4CampaignExecutionRegistry>()
        .map(|registry| registry.produced_entity_assignments(player_id))
        .unwrap_or_default();
    let roadmap_assignments = world
        .get_resource::<crate::ai::operation_assignment::UnitOperationRegistry>()
        .map(|registry| registry.campaign_entity_assignments(player_id))
        .unwrap_or_default();
    let mut unavailable_entities = unavailable_campaign_entities(&manager, &units, player_id);
    let audit_turn = world
        .get_resource::<MatchState>()
        .map_or(0, |state| state.current_turn_number.0);
    if pool_audit_enabled() {
        let mut squad_unavailable: Vec<_> = unavailable_entities.iter().copied().collect();
        squad_unavailable.sort_by_key(|entity| entity.to_bits());
        let mut reserved: Vec<_> = reserved_entities.iter().copied().collect();
        reserved.sort_by_key(|entity| entity.to_bits());
        eprintln!(
            "POOL_AUDIT stage=exclusion_sources turn={audit_turn} player={player_id:?} squad_unavailable={squad_unavailable:?} caller_reserved={reserved:?}"
        );
    }
    unavailable_entities.extend(reserved_entities.iter().copied());
    let pool = collect_campaign_resource_pool(
        player_id,
        &map,
        &registry,
        &island_map,
        &properties,
        &units,
        &operations,
        &produced_assignments,
        &roadmap_assignments,
        &assigned_transport_phases,
        &unavailable_entities,
        available_funds,
        audit_turn,
    );

    let facts_by_island: HashMap<_, _> = facts
        .iter()
        .map(|island_facts| (island_facts.island_id, island_facts))
        .collect();
    let mut islands = island_map.islands.clone();
    islands.sort_by_key(|island| island.id.0);
    let uses_operation_driven_production =
        crate::ai::resolve_player_ai_version(world, player_id).uses_operation_driven_production();
    // 島ごとの候補構築は同じ盤面snapshotを読むだけで互いに独立している。
    // nativeでは並列化し、IslandId順の結果を維持することでV3/V4の判断順を変えない。
    // 評価と実行候補は別物である。前者は全島の状態をRoadmapと診断へ渡し、後者だけを
    // allocatorへ渡す。目標がない島を候補から外しても、評価を消してはならない。
    let evaluated_islands = crate::ai::deterministic_parallel::map_ordered(islands, |island| {
        let island_facts = facts_by_island.get(&island.id).copied()?;
        let mut assessment = assess_island(island_facts);
        // 陸塊が1つだけでも島嶼mapと同じ継続campaignとして扱う。違いは
        // `reachable` と輸送routeから輸送工程が必要になるかどうかだけである。
        let mut requirement = requirement_for_assessment(island_facts, &mut assessment);
        let existing_operation = operations_by_island.get(&island.id).cloned();
        let assault_wave_is_live = existing_operation.as_ref().is_some_and(|operation| {
            matches!(
                operation.transport_phase,
                Some(TransportPhase::Pickup | TransportPhase::Transit | TransportPhase::Drop)
            )
        });
        let assault_transport_types =
            if assessment.decision == IslandCampaignDecision::Assault && assault_wave_is_live {
                let operation = existing_operation.as_ref().expect("live operation exists");
                let mut transport_types = operation
                    .transport_entities
                    .iter()
                    .filter_map(|entity| {
                        units
                            .iter()
                            .find(|unit| unit.entity == *entity)
                            .map(|unit| unit.stats.unit_type)
                    })
                    .collect::<Vec<_>>();
                transport_types.sort_by_key(|unit_type| campaign_unit_type_rank(*unit_type));
                let entity_cost = |entity: &Entity| {
                    units
                        .iter()
                        .find(|unit| unit.entity == *entity)
                        .map_or(0, |unit| unit.stats.cost)
                };
                requirement.preferred_transport = transport_types.first().copied();
                requirement.transport_slots = operation
                    .transport_entities
                    .iter()
                    .filter_map(|entity| units.iter().find(|unit| unit.entity == *entity))
                    .fold(0_u32, |total, unit| {
                        total.saturating_add(unit.free_cargo_slots)
                    });
                requirement.capture_units =
                    u32::try_from(operation.capture_entities.len()).unwrap_or(u32::MAX);
                // Combat必要数は時系列planの出力であり、既存便の台数から再構築しない。
                requirement.ground_combat_units = 0;
                requirement.combat_units =
                    u32::try_from(operation.combat_entities.len()).unwrap_or(u32::MAX);
                requirement.total_budget = operation
                    .transport_entities
                    .iter()
                    .chain(&operation.capture_entities)
                    .fold(0_u32, |total, entity| {
                        total.saturating_add(entity_cost(entity))
                    });
                assessment.required_budget = requirement.total_budget;
                transport_types
            } else {
                adapt_assault_requirement_to_production_route(
                    &mut assessment,
                    &mut requirement,
                    &producible_transports,
                )
            };
        let mut priority_enemy_types = units
            .iter()
            .filter(|unit| unit.faction != player_id && unit.transporting.is_none())
            // 地上揚陸波の編成対象だけを渡す。航空・艦船はV4 rolling planが
            // 別経路で処理し、地上unitに不可能な対空・対艦要件を課さない。
            .filter(|unit| {
                !matches!(
                    unit.stats.movement_type,
                    MovementType::Air | MovementType::Ship
                )
            })
            .filter(|unit| island.tiles.contains(&unit.position))
            .map(|unit| unit.stats.unit_type)
            .collect::<Vec<_>>();
        priority_enemy_types.sort_by_key(|unit_type| campaign_unit_type_rank(*unit_type));
        priority_enemy_types.dedup();
        let target_position = candidate_target_position(
            &map,
            &registry,
            &island,
            &properties,
            &units,
            player_id,
            assessment.decision,
            existing_operation.as_ref(),
        );
        let Some(target_position) = target_position else {
            return Some((assessment, None));
        };
        let capture_target_positions = candidate_capture_target_positions(
            &map,
            &registry,
            &island,
            &properties,
            &units,
            player_id,
            target_position,
            existing_operation.as_ref(),
        );
        let has_foothold = properties.iter().any(|property| {
            island.tiles.contains(&property.position)
                && property.property.owner_id == Some(player_id)
        });
        if has_foothold {
            // 同一陸塊の全未所有施設ではなく、今手番に選んだ前線帯だけを同時要求する。
            // 後方の施設は前線取得後に毎手番再評価するため、将来分の占領兵費用を
            // 現在作戦へ先取りしない。
            let active_capture_units =
                u32::try_from(capture_target_positions.len()).unwrap_or(u32::MAX);
            let released_capture_units = requirement
                .capture_units
                .saturating_sub(active_capture_units);
            requirement.capture_units = active_capture_units;
            requirement.transport_slots = requirement.transport_slots.min(active_capture_units);
            requirement.total_budget = requirement
                .total_budget
                .saturating_sub(released_capture_units.saturating_mul(1_000));
            assessment.required_budget = requirement.total_budget;
        }
        let (ground_sustainment_sites, air_sustainment_sites, sea_sustainment_sites) =
            sustainment_site_counts(&island, &properties);
        let sustainment_targets = if Some(island.id) == home_island {
            CampaignSustainmentTargets::default()
        } else {
            sustainment_targets(&island, &properties, player_id)
        };
        Some((
            assessment.clone(),
            Some(IslandCampaignCandidate {
                assessment,
                target_position,
                capture_target_positions,
                priority_enemy_types,
                roi_production_sites: island_facts.roi_production_sites,
                transport_eta: island_facts.transport_eta,
                ground_sustainment_sites,
                air_sustainment_sites,
                sea_sustainment_sites,
                sustainment_targets,
                island_income_per_turn: island_facts.island_income_per_turn,
                logistics_prerequisite: false,
                logistics_priority_rank: None,
                requirement,
                assault_transport_types,
                producible_transports: producible_transports.clone(),
                existing_operation,
            }),
        ))
    })
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();
    let assessments = evaluated_islands
        .iter()
        .map(|(assessment, _)| assessment.clone())
        .collect::<Vec<_>>();
    let mut candidates = evaluated_islands
        .into_iter()
        .filter_map(|(_, candidate)| candidate)
        .collect::<Vec<_>>();
    let mut capital_launch_gate = None;
    if uses_operation_driven_production {
        let turn = world
            .get_resource::<MatchState>()
            .map_or(0, |state| state.current_turn_number.0);
        let mut logistics_registry = world
            .remove_resource::<crate::ai::v4::logistics_plan::V4LogisticsPlanRegistry>()
            .unwrap_or_default();
        crate::ai::v4::logistics_plan::apply_persistent_logistics_plan(
            &mut logistics_registry,
            player_id,
            turn,
            &mut candidates,
            &map,
            friendly_income_per_turn,
            enemy_income_per_turn,
            available_funds,
            forward_sustainment_coverage,
            home_position,
            enemy_capital,
        );
        let logistics_ready = logistics_registry
            .plan(player_id)
            .is_some_and(|plan| plan.selected_islands.is_empty());
        if let Some((capital_island, _)) = enemy_capital {
            if let Some(candidate) = candidates
                .iter_mut()
                .find(|candidate| candidate.assessment.island_id == capital_island)
            {
                prepare_capital_assault_wave(candidate);
            }
            let combat_plan_ready = world
                .get_resource::<crate::ai::v4::plan_revision::V4RollingPlanRegistry>()
                .is_some_and(|registry| registry.operation_launch_ready(player_id, capital_island));
            capital_launch_gate = Some((capital_island, logistics_ready, combat_plan_ready));
        }
        world.insert_resource(logistics_registry);
    } else {
        promote_logistics_prerequisite(
            &mut candidates,
            &map,
            friendly_income_per_turn,
            enemy_income_per_turn,
            forward_sustainment_coverage,
            home_position,
        );
    }
    let mut portfolio = allocate_campaign_portfolio_with_assessments(candidates, pool, assessments);
    if let Some((capital_island, logistics_ready, combat_plan_ready)) = capital_launch_gate {
        gate_capital_assault_launch(
            &mut portfolio,
            capital_island,
            logistics_ready,
            combat_plan_ready,
        );
    }
    portfolio
}

/// 局地占領作戦の前線は、所有施設ではなく実際にその作戦へ所属する占領兵から測る。
/// 担当兵が損耗した場合は同じ島の未所属占領兵へ再評価し、実働兵がいない場合だけ
/// 揚陸前・再編成中の互換fallbackとして所有施設を用いる。
pub(super) fn capture_front_origins(
    island: &Island,
    properties: &[PropertySnapshot],
    units: &[UnitSnapshot],
    player_id: PlayerId,
    existing_operation: Option<&ExistingCampaignOperation>,
) -> Vec<GridPosition> {
    let owned_property_positions = || {
        properties
            .iter()
            .filter(|snapshot| island.tiles.contains(&snapshot.position))
            .filter(|snapshot| snapshot.property.owner_id == Some(player_id))
            .map(|snapshot| snapshot.position)
            .collect::<Vec<_>>()
    };
    let has_own_capital = properties.iter().any(|snapshot| {
        island.tiles.contains(&snapshot.position)
            && snapshot.property.terrain == Terrain::Capital
            && snapshot.property.owner_id == Some(player_id)
    });
    let has_enemy_capital = properties.iter().any(|snapshot| {
        island.tiles.contains(&snapshot.position)
            && snapshot.property.terrain == Terrain::Capital
            && snapshot
                .property
                .owner_id
                .is_some_and(|owner| owner != player_id)
    });
    if has_own_capital && has_enemy_capital {
        // 同一陸塊の首都戦役は、担当歩兵一体の位置で戦略前線を後退させない。
        // 所有施設群を補給線として測るmainの挙動を維持し、局地CaptureはSquad order、
        // 首都方向の継続目標はCampaignがそれぞれ担当する。
        return owned_property_positions();
    }
    let live_capture_position = |unit: &&UnitSnapshot| {
        unit.faction == player_id
            && unit.stats.can_capture
            && unit.transporting.is_none()
            && island.tiles.contains(&unit.position)
    };
    if let Some(operation) = existing_operation {
        let assigned_entities = operation
            .capture_entities
            .iter()
            .copied()
            .collect::<HashSet<_>>();
        let assigned_positions = units
            .iter()
            .filter(live_capture_position)
            .filter(|unit| assigned_entities.contains(&unit.entity))
            .map(|unit| unit.position)
            .collect::<Vec<_>>();
        if !assigned_positions.is_empty() {
            return assigned_positions;
        }
    }
    let available_positions = units
        .iter()
        .filter(live_capture_position)
        .map(|unit| unit.position)
        .collect::<Vec<_>>();
    if !available_positions.is_empty() {
        return available_positions;
    }
    owned_property_positions()
}

/// 【一時的な診断専用】campaign共有poolから自軍Entityが落ちる理由を特定する監査。
/// `OPENWARS_POOL_AUDIT` 設定時のみ出力する。
pub(crate) fn pool_audit_enabled() -> bool {
    std::env::var_os("OPENWARS_POOL_AUDIT").is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::island_campaign::{IslandCampaignDecision, IslandCampaignState};
    use crate::ai::islands::IslandMap;
    use crate::components::{
        ActionCompleted, Ammo, Faction, Fuel, GridPosition, HasMoved, Health, PlayerId, Property,
        UnitStats,
    };
    use crate::resources::master_data::{MasterDataRegistry, UnitName};
    use crate::resources::{GameRng, GridTopology, Map, MatchState, Terrain, UnitType};
    use bevy_ecs::prelude::{Entity, World};

    const TEST_SEED: u64 = 42;

    #[test]
    fn mirrored_land_fronts_choose_the_nearest_property_instead_of_coordinate_order() {
        let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
        let master_data = MasterDataRegistry::load().expect("master data should load");
        let map = Map::new(11, 1, Terrain::Plains, GridTopology::Square);
        let island = IslandMap::analyze(&map).islands.remove(0);
        let player = PlayerId(1);
        let enemy = PlayerId(2);
        let property = |x, owner| PropertySnapshot {
            position: GridPosition { x, y: 0 },
            property: Property::new(Terrain::City, owner, 200),
        };

        let left_properties = vec![
            property(0, Some(player)),
            property(2, Some(enemy)),
            property(8, Some(enemy)),
        ];
        let left_target = candidate_target_position(
            &map,
            &master_data,
            &island,
            &left_properties,
            &[],
            player,
            IslandCampaignDecision::Assault,
            None,
        );
        assert_eq!(left_target, Some(GridPosition { x: 2, y: 0 }));

        let right_properties = vec![
            property(10, Some(player)),
            property(2, Some(enemy)),
            property(8, Some(enemy)),
        ];
        let right_target = candidate_target_position(
            &map,
            &master_data,
            &island,
            &right_properties,
            &[],
            player,
            IslandCampaignDecision::Assault,
            None,
        );
        assert_eq!(right_target, Some(GridPosition { x: 8, y: 0 }));
        assert_eq!(
            candidate_capture_target_positions(
                &map,
                &master_data,
                &island,
                &right_properties,
                &[],
                player,
                right_target.expect("右側前線"),
                None,
            ),
            vec![GridPosition { x: 8, y: 0 }]
        );
    }

    #[test]
    fn island_without_open_property_targets_only_a_live_ground_enemy() {
        let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
        let master_data = MasterDataRegistry::load().expect("master data should load");
        let player = PlayerId(1);
        let enemy = PlayerId(2);
        let mut map = Map::new(4, 1, Terrain::Sea, GridTopology::Square);
        map.set_terrain(0, 0, Terrain::City).unwrap();
        map.set_terrain(1, 0, Terrain::Shoal).unwrap();
        map.set_terrain(2, 0, Terrain::Plains).unwrap();
        let island = IslandMap::analyze(&map).islands.remove(0);
        let properties = vec![PropertySnapshot {
            position: GridPosition { x: 0, y: 0 },
            property: Property::new(Terrain::City, Some(player), 200),
        }];

        // 未所有施設も地上敵もない島は、座標順で選ばれるShoalへ輸送してはならない。
        assert_eq!(
            candidate_target_position(
                &map,
                &master_data,
                &island,
                &properties,
                &[],
                player,
                IslandCampaignDecision::Secure,
                None,
            ),
            None
        );

        let enemy_position = GridPosition { x: 2, y: 0 };
        let units = vec![UnitSnapshot {
            entity: Entity::from_raw(42),
            faction: enemy,
            position: enemy_position,
            stats: UnitStats {
                movement_type: MovementType::Infantry,
                ..UnitStats::mock()
            },
            health: Health {
                current: 100,
                max: 100,
            },
            fuel: None,
            transporting: None,
            free_cargo_slots: 0,
            loaded_cargo_entities: Vec::new(),
        }];
        assert_eq!(
            candidate_target_position(
                &map,
                &master_data,
                &island,
                &properties,
                &units,
                player,
                IslandCampaignDecision::Assault,
                None,
            ),
            Some(enemy_position)
        );
    }

    #[test]
    fn local_capture_front_uses_assigned_unit_instead_of_distant_owned_property() {
        let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
        let master_data = MasterDataRegistry::load().expect("master data should load");
        let map = Map::new(11, 1, Terrain::Plains, GridTopology::Square);
        let island = IslandMap::analyze(&map).islands.remove(0);
        let player = PlayerId(1);
        let enemy = PlayerId(2);
        let assigned = Entity::from_raw(42);
        let properties = vec![
            PropertySnapshot {
                position: GridPosition { x: 0, y: 0 },
                property: Property::new(Terrain::City, Some(player), 200),
            },
            PropertySnapshot {
                position: GridPosition { x: 2, y: 0 },
                property: Property::new(Terrain::City, Some(enemy), 200),
            },
            PropertySnapshot {
                position: GridPosition { x: 8, y: 0 },
                property: Property::new(Terrain::City, Some(enemy), 200),
            },
        ];
        let units = vec![UnitSnapshot {
            entity: assigned,
            faction: player,
            position: GridPosition { x: 10, y: 0 },
            stats: UnitStats {
                can_capture: true,
                ..UnitStats::mock()
            },
            health: Health {
                current: 100,
                max: 100,
            },
            fuel: None,
            transporting: None,
            free_cargo_slots: 0,
            loaded_cargo_entities: Vec::new(),
        }];
        let operation = ExistingCampaignOperation {
            island_id: island.id,
            target_position: GridPosition { x: 8, y: 0 },
            transport_phase: None,
            is_forming: false,
            transport_entities: Vec::new(),
            capture_entities: vec![assigned],
            combat_entities: Vec::new(),
        };

        assert_eq!(
            candidate_target_position(
                &map,
                &master_data,
                &island,
                &properties,
                &units,
                player,
                IslandCampaignDecision::Secure,
                Some(&operation),
            ),
            Some(GridPosition { x: 8, y: 0 })
        );
        assert_eq!(
            candidate_capture_target_positions(
                &map,
                &master_data,
                &island,
                &properties,
                &units,
                player,
                GridPosition { x: 8, y: 0 },
                Some(&operation),
            ),
            vec![GridPosition { x: 8, y: 0 }]
        );
    }

    #[test]
    fn same_land_capital_front_does_not_retreat_to_the_capturer_position() {
        let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
        let master_data = MasterDataRegistry::load().expect("master data should load");
        let map = Map::new(11, 1, Terrain::Plains, GridTopology::Square);
        let island = IslandMap::analyze(&map).islands.remove(0);
        let player = PlayerId(1);
        let enemy = PlayerId(2);
        let assigned = Entity::from_raw(42);
        let strategic_target = GridPosition { x: 0, y: 0 };
        let properties = vec![
            PropertySnapshot {
                position: strategic_target,
                property: Property::new(Terrain::Capital, Some(enemy), 200),
            },
            PropertySnapshot {
                position: GridPosition { x: 1, y: 0 },
                property: Property::new(Terrain::City, Some(player), 200),
            },
            PropertySnapshot {
                position: GridPosition { x: 8, y: 0 },
                property: Property::new(Terrain::City, None, 200),
            },
            PropertySnapshot {
                position: GridPosition { x: 10, y: 0 },
                property: Property::new(Terrain::Capital, Some(player), 200),
            },
        ];
        let units = vec![UnitSnapshot {
            entity: assigned,
            faction: player,
            position: GridPosition { x: 10, y: 0 },
            stats: UnitStats {
                can_capture: true,
                ..UnitStats::mock()
            },
            health: Health {
                current: 100,
                max: 100,
            },
            fuel: None,
            transporting: None,
            free_cargo_slots: 0,
            loaded_cargo_entities: Vec::new(),
        }];
        let operation = ExistingCampaignOperation {
            island_id: island.id,
            target_position: strategic_target,
            transport_phase: None,
            is_forming: false,
            transport_entities: Vec::new(),
            capture_entities: vec![assigned],
            combat_entities: Vec::new(),
        };

        assert_eq!(
            candidate_target_position(
                &map,
                &master_data,
                &island,
                &properties,
                &units,
                player,
                IslandCampaignDecision::Contest,
                Some(&operation),
            ),
            Some(strategic_target),
            "同一陸塊の戦略目標を、後方の担当歩兵に近い中立拠点へ戻さない"
        );
    }

    fn spawn_test_unit(
        world: &mut World,
        player: PlayerId,
        position: GridPosition,
        stats: UnitStats,
    ) -> Entity {
        let max_fuel = stats.max_fuel.max(1);
        let max_ammo1 = stats.max_ammo1;
        let max_ammo2 = stats.max_ammo2;
        world
            .spawn((
                player,
                Faction(player),
                position,
                stats,
                HasMoved(false),
                ActionCompleted(false),
                Health {
                    current: 100,
                    max: 100,
                },
                Fuel {
                    current: max_fuel,
                    max: max_fuel,
                },
                Ammo {
                    ammo1: max_ammo1,
                    max_ammo1,
                    ammo2: max_ammo2,
                    max_ammo2,
                },
            ))
            .id()
    }

    #[test]
    fn reconstructs_initial_state_for_every_island() {
        let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
        let master_data = MasterDataRegistry::load().expect("master data should load");
        let (mut world, _schedule) =
            crate::setup::initialize_world_from_master_data(&master_data, "map_1")
                .expect("test world should initialize");
        let entities: Vec<Entity> = world.query::<Entity>().iter(&world).collect();
        for entity in entities {
            world.despawn(entity);
        }

        let player = PlayerId(1);
        let enemy = PlayerId(2);
        let own_position = GridPosition { x: 0, y: 1 };
        let enemy_position = GridPosition { x: 2, y: 1 };
        let neutral_position = GridPosition { x: 4, y: 1 };
        let ignored_position = GridPosition { x: 6, y: 1 };
        let mut map = Map::new(7, 3, Terrain::Sea, GridTopology::Square);
        map.set_terrain(own_position.x, own_position.y, Terrain::Airport)
            .unwrap();
        map.set_terrain(enemy_position.x, enemy_position.y, Terrain::Capital)
            .unwrap();
        map.set_terrain(neutral_position.x, neutral_position.y, Terrain::City)
            .unwrap();
        map.set_terrain(ignored_position.x, ignored_position.y, Terrain::Plains)
            .unwrap();
        let island_map = IslandMap::analyze(&map);
        let own_island = island_map.get_island_at(&own_position).unwrap().id;
        let enemy_island = island_map.get_island_at(&enemy_position).unwrap().id;
        let neutral_island = island_map.get_island_at(&neutral_position).unwrap().id;
        let ignored_island = island_map.get_island_at(&ignored_position).unwrap().id;
        world.insert_resource(map);
        world.insert_resource(island_map);
        world.insert_resource(GameRng::new(TEST_SEED));
        world.insert_resource(MatchState::default());

        world.spawn((
            own_position,
            Property::new(Terrain::Airport, Some(player), 100),
        ));
        world.spawn((
            enemy_position,
            Property::new(Terrain::Capital, Some(enemy), 100),
        ));
        world.spawn((neutral_position, Property::new(Terrain::City, None, 100)));

        let infantry = master_data
            .create_unit_stats(&UnitName(UnitType::Infantry.as_str().to_owned()))
            .expect("infantry stats should exist");
        spawn_test_unit(&mut world, player, own_position, infantry.clone());
        spawn_test_unit(&mut world, enemy, enemy_position, infantry);

        let portfolio = analyze_island_campaign(&mut world, player);
        let states: Vec<_> = portfolio
            .islands
            .iter()
            .map(|assessment| (assessment.island_id, assessment.state))
            .collect();

        assert_eq!(
            states,
            vec![
                (own_island, IslandCampaignState::Secured),
                (enemy_island, IslandCampaignState::EnemyHeld),
                (neutral_island, IslandCampaignState::OpenNeutral),
                (ignored_island, IslandCampaignState::Ignored),
            ]
        );
        assert!(portfolio.active_offensives.len() <= 3);
        assert!(portfolio.defenses.is_empty());

        // V4も同じ全島評価をRoadmapの入力に使う。実行候補がないSecured/Ignored島を
        // 生産・作戦駆動の有無で診断から消してはならない。
        let mut settings = crate::ai::ai_version::PlayerAiSettings::default();
        settings.set_version(player, crate::ai::ai_version::AiVersion::V4);
        world.insert_resource(settings);
        let v4_portfolio = analyze_island_campaign(&mut world, player);
        let v4_states: Vec<_> = v4_portfolio
            .islands
            .iter()
            .map(|assessment| (assessment.island_id, assessment.state))
            .collect();
        assert_eq!(v4_states, states);
    }
}
