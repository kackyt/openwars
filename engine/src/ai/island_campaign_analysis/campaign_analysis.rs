//! 盤面観測・地域評価・兵站準備・資源配分を順に組み立てる。
#[cfg(test)]
use super::analyze_island_campaign;
use super::resource_pool::collect_campaign_resource_pool;
use super::target_selection::{candidate_capture_target_positions, candidate_target_position};
use super::{
    CampaignResourcePool, CampaignSustainmentCoverage, CampaignSustainmentTargets,
    CampaignTransportBlueprint, Entity, ExistingCampaignOperation, GridPosition, HashMap, HashSet,
    Island, IslandCampaignCandidate, IslandCampaignDecision, IslandCampaignFacts,
    IslandCampaignPortfolio, IslandCampaignRequirement, IslandId, IslandMap, Map,
    MasterDataRegistry, MatchState, MovementType, PlayerId, Players, PropertySnapshot,
    SquadManager, Terrain, TransportPhase, UnitSnapshot, UnitType, World,
    adapt_assault_requirement_to_production_route, allocate_campaign_portfolio_with_assessments,
    assess_island, campaign_unit_snapshots, campaign_unit_type_rank,
    collect_assigned_transport_phases, collect_existing_operations, collect_faction_snapshots,
    collect_island_campaign_facts, collect_property_snapshots, collect_unit_snapshots,
    gate_capital_assault_launch, operation_has_live_capability, prepare_capital_assault_wave,
    producible_campaign_transports, promote_logistics_prerequisite, requirement_for_assessment,
    sustainment_site_counts, sustainment_targets, unavailable_campaign_entities,
};

use crate::ai::island_campaign::IslandCampaignAssessment;

/// 既存の実行任務と呼出側予約を除き、同じsnapshotから作戦候補を評価する。
pub(crate) fn analyze_island_campaign_excluding(
    world: &mut World,
    player_id: PlayerId,
    reserved_entities: &HashSet<Entity>,
) -> IslandCampaignPortfolio {
    let snapshot = CampaignAnalysisSnapshot::collect(world, player_id, reserved_entities);
    let uses_operation_driven_production =
        crate::ai::resolve_player_ai_version(world, player_id).uses_operation_driven_production();
    let evaluated_islands = snapshot.evaluate_islands();
    let assessments = evaluated_islands
        .iter()
        .map(|(assessment, _)| assessment.clone())
        .collect();
    let mut candidates: Vec<_> = evaluated_islands
        .into_iter()
        .filter_map(|(_, candidate)| candidate)
        .collect();
    let capital_launch_gate =
        snapshot.prepare_logistics(world, &mut candidates, uses_operation_driven_production);
    let mut portfolio =
        allocate_campaign_portfolio_with_assessments(candidates, snapshot.pool, assessments);
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

/// この解析中だけ共有する観測値。兵站台帳を更新しても再収集しない。
struct CampaignAnalysisSnapshot {
    player_id: PlayerId,
    facts: Vec<IslandCampaignFacts>,
    map: Map,
    island_map: IslandMap,
    registry: MasterDataRegistry,
    properties: Vec<PropertySnapshot>,
    friendly_income_per_turn: u32,
    enemy_income_per_turn: u32,
    home_position: Option<GridPosition>,
    home_island: Option<IslandId>,
    enemy_capital: Option<(IslandId, GridPosition)>,
    forward_sustainment_coverage: CampaignSustainmentCoverage,
    producible_transports: Vec<CampaignTransportBlueprint>,
    units: Vec<UnitSnapshot>,
    operations_by_island: HashMap<IslandId, ExistingCampaignOperation>,
    available_funds: u32,
    pool: CampaignResourcePool,
}
impl CampaignAnalysisSnapshot {
    fn collect(
        world: &mut World,
        player_id: PlayerId,
        reserved_entities: &HashSet<Entity>,
    ) -> Self {
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
        let forward_sustainment_coverage =
            observe_forward_coverage(&properties, &island_map, player_id, home_island);
        let producible_transports =
            producible_campaign_transports(&map, &registry, &island_map, &properties, player_id);
        let factions = collect_faction_snapshots(world);
        let units = campaign_unit_snapshots(&manager, collect_unit_snapshots(world), &factions);
        let assigned_transport_phases =
            collect_assigned_transport_phases(&manager, &units, player_id);
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
        Self {
            player_id,
            facts,
            map,
            island_map,
            registry,
            properties,
            friendly_income_per_turn,
            enemy_income_per_turn,
            home_position,
            home_island,
            enemy_capital,
            forward_sustainment_coverage,
            producible_transports,
            units,
            operations_by_island,
            available_funds,
            pool,
        }
    }

    /// 独立な島評価を並列化し、IslandId順の結果を保持する。
    fn evaluate_islands(&self) -> Vec<(IslandCampaignAssessment, Option<IslandCampaignCandidate>)> {
        let _map = &self.map;
        let island_map = &self.island_map;
        let facts = &self.facts;
        let facts_by_island: HashMap<_, _> = facts
            .iter()
            .map(|island_facts| (island_facts.island_id, island_facts))
            .collect();
        let mut islands = island_map.islands.clone();
        islands.sort_by_key(|island| island.id.0);
        crate::ai::deterministic_parallel::map_ordered(islands, |island| {
            let island_facts = facts_by_island.get(&island.id).copied()?;
            self.evaluate_island(island, island_facts)
        })
        .into_iter()
        .flatten()
        .collect()
    }

    /// 島評価を前線目標と実行可能な作戦候補へ投影する。
    fn evaluate_island(
        &self,
        island: Island,
        island_facts: &IslandCampaignFacts,
    ) -> Option<(IslandCampaignAssessment, Option<IslandCampaignCandidate>)> {
        let player_id = self.player_id;
        let map = &self.map;
        let registry = &self.registry;
        let properties = &self.properties;
        let units = &self.units;
        let operations_by_island = &self.operations_by_island;
        let producible_transports = &self.producible_transports;
        let home_island = self.home_island;
        let mut assessment = assess_island(island_facts);
        // 陸塊が1つだけでも島嶼mapと同じ継続campaignとして扱う。違いは
        // `reachable` と輸送routeから輸送工程が必要になるかどうかだけである。
        let mut requirement = requirement_for_assessment(island_facts, &mut assessment);
        let existing_operation = operations_by_island.get(&island.id).cloned();
        let assault_transport_types = self.adapt_assault_requirement(
            &mut assessment,
            &mut requirement,
            existing_operation.as_ref(),
        );
        let priority_enemy_types = self.priority_enemy_types(&island);
        let target_position = candidate_target_position(
            map,
            registry,
            &island,
            properties,
            units,
            player_id,
            assessment.decision,
            existing_operation.as_ref(),
        );
        let Some(target_position) = target_position else {
            return Some((assessment, None));
        };
        let capture_target_positions = candidate_capture_target_positions(
            map,
            registry,
            &island,
            properties,
            units,
            player_id,
            target_position,
            existing_operation.as_ref(),
        );
        self.limit_capture_front(
            &island,
            &capture_target_positions,
            &mut requirement,
            &mut assessment,
        );
        let (ground_sustainment_sites, air_sustainment_sites, sea_sustainment_sites) =
            sustainment_site_counts(&island, properties);
        let sustainment_targets = if Some(island.id) == home_island {
            CampaignSustainmentTargets::default()
        } else {
            sustainment_targets(&island, properties, player_id)
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
    }

    /// 実行中の輸送波は実積載と価格を保持し、新規便だけ生産経路へ適合させる。
    fn adapt_assault_requirement(
        &self,
        assessment: &mut IslandCampaignAssessment,
        requirement: &mut IslandCampaignRequirement,
        existing_operation: Option<&ExistingCampaignOperation>,
    ) -> Vec<UnitType> {
        let _map = &self.map;
        let units = &self.units;
        let producible_transports = &self.producible_transports;
        let assault_wave_is_live = existing_operation.as_ref().is_some_and(|operation| {
            matches!(
                operation.transport_phase,
                Some(TransportPhase::Pickup | TransportPhase::Transit | TransportPhase::Drop)
            )
        });

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
                assessment,
                requirement,
                producible_transports,
            )
        }
    }

    /// 地上揚陸波が実際に交戦する敵種だけを安定順で集める。
    fn priority_enemy_types(&self, island: &Island) -> Vec<UnitType> {
        let player_id = self.player_id;
        let _map = &self.map;
        let units = &self.units;
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
        priority_enemy_types
    }

    /// 既存橋頭堡があれば今手番の前線物件だけへ占領要求を限定する。
    fn limit_capture_front(
        &self,
        island: &Island,
        capture_target_positions: &[GridPosition],
        requirement: &mut IslandCampaignRequirement,
        assessment: &mut IslandCampaignAssessment,
    ) {
        let player_id = self.player_id;
        let properties = &self.properties;
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
    }

    /// 候補評価後にだけ継続兵站計画と首都出撃gateを更新する。
    fn prepare_logistics(
        &self,
        world: &mut World,
        candidates: &mut [IslandCampaignCandidate],
        uses_operation_driven_production: bool,
    ) -> Option<(IslandId, bool, bool)> {
        let player_id = self.player_id;
        let map = &self.map;
        let _registry = &self.registry;
        let home_position = self.home_position;
        let enemy_capital = self.enemy_capital;
        let friendly_income_per_turn = self.friendly_income_per_turn;
        let enemy_income_per_turn = self.enemy_income_per_turn;
        let available_funds = self.available_funds;
        let forward_sustainment_coverage = self.forward_sustainment_coverage;
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
                candidates,
                map,
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
                    .is_some_and(|registry| {
                        registry.operation_launch_ready(player_id, capital_island)
                    });
                capital_launch_gate = Some((capital_island, logistics_ready, combat_plan_ready));
            }
            world.insert_resource(logistics_registry);
        } else {
            promote_logistics_prerequisite(
                candidates,
                map,
                friendly_income_per_turn,
                enemy_income_per_turn,
                forward_sustainment_coverage,
                home_position,
            );
        }
        capital_launch_gate
    }
}

/// 所有する前進物件から地上・航空・海上の補給範囲を観測する。
fn observe_forward_coverage(
    properties: &[PropertySnapshot],
    island_map: &IslandMap,
    player_id: PlayerId,
    home_island: Option<IslandId>,
) -> CampaignSustainmentCoverage {
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
    forward_sustainment_coverage
}

/// 診断専用の共有pool監査。判断には戻さない。
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
