use super::*;
use crate::components::{GridPosition, Health};
use crate::resources::master_data::UnitName;
use crate::resources::{GridTopology, Terrain};

// Worldの再収集を挟まず、両経路へ同じ観測値を与えて候補の全fieldを固定する。
struct Fixture {
    registry: MasterDataRegistry,
    map: Map,
    islands: IslandMap,
    units: Vec<UnitSnapshot>,
    properties: Vec<PropertySnapshot>,
    operations: Vec<ExistingCampaignOperation>,
    produced: HashMap<Entity, IslandId>,
    roadmap: HashMap<Entity, IslandId>,
    phases: HashMap<Entity, AssignedTransportState>,
    unavailable: HashSet<Entity>,
}

impl Fixture {
    fn new() -> Self {
        let map = Map::new(3, 1, Terrain::Plains, GridTopology::Square);
        Self {
            islands: IslandMap::analyze(&map),
            map,
            registry: MasterDataRegistry::load().expect("マスターデータを読み込む"),
            units: Vec::new(),
            properties: Vec::new(),
            operations: Vec::new(),
            produced: HashMap::default(),
            roadmap: HashMap::default(),
            phases: HashMap::default(),
            unavailable: HashSet::default(),
        }
    }

    fn unit(&self, id: u32, unit_type: UnitType) -> UnitSnapshot {
        UnitSnapshot {
            entity: Entity::from_raw(id),
            faction: PlayerId(1),
            position: GridPosition { x: 0, y: 0 },
            stats: self
                .registry
                .create_unit_stats(&UnitName(unit_type.as_str().to_owned()))
                .expect("ユニット諸元を取得する"),
            health: Health {
                current: 100,
                max: 100,
            },
            fuel: Some(99),
            transporting: None,
            free_cargo_slots: 0,
            loaded_cargo_entities: Vec::new(),
        }
    }

    fn expected(&self, unit: &UnitSnapshot) -> CampaignUnitCandidate {
        CampaignUnitCandidate {
            entity: unit.entity,
            unit_type: unit.stats.unit_type,
            cost: unit.stats.cost,
            can_capture: unit.stats.can_capture,
            can_secure_local_property: false,
            available_cargo_slots: unit.free_cargo_slots,
            loaded_cargo_entities: Vec::new(),
            loadable_unit_types: unit.stats.loadable_unit_types.clone(),
            is_transporting: unit.transporting.is_some(),
            reachable_positions: vec![
                GridPosition { x: 0, y: 0 },
                GridPosition { x: 1, y: 0 },
                GridPosition { x: 2, y: 0 },
            ],
            island_id: self
                .islands
                .get_island_at(&unit.position)
                .map(|island| island.id),
            assigned_island: None,
        }
    }

    fn assert_pool(&self, expected: Vec<CampaignUnitCandidate>) {
        let expected = CampaignResourcePool {
            available_funds: 12_345,
            units: expected,
        };
        let standard = super::super::collect_campaign_resource_pool(
            PlayerId(1),
            &self.map,
            &self.registry,
            &self.islands,
            &self.properties,
            &self.units,
            &self.operations,
            &self.produced,
            &self.roadmap,
            &self.phases,
            &self.unavailable,
            12_345,
        );
        let small_map = collect_campaign_resource_pool(
            PlayerId(1),
            &self.map,
            &self.registry,
            &self.islands,
            &self.properties,
            &self.units,
            &self.operations,
            &self.produced,
            &self.roadmap,
            &self.phases,
            &self.unavailable,
            12_345,
            7,
        );
        assert_eq!(standard, expected, "通常経路のpool全体");
        assert_eq!(small_map, expected, "小規模経路のpool全体");
    }
}

#[test]
fn resource_pool_preserves_assignment_precedence_and_all_sort_keys() {
    let mut fixture = Fixture::new();
    let mut first = fixture.unit(1, UnitType::Infantry);
    first.stats.cost = 100;
    let mut second = fixture.unit(2, UnitType::Mech);
    second.stats.cost = 100;
    let mut third = fixture.unit(3, UnitType::Infantry);
    third.stats.cost = 101;
    let fourth = fixture.unit(4, UnitType::Infantry);
    let fifth = fixture.unit(5, UnitType::Infantry);
    let sixth = fixture.unit(6, UnitType::Infantry);
    let seventh = fixture.unit(7, UnitType::Infantry);
    // 費用→兵種→Entityの順を、入力順とは異なる候補列で確認する。
    fixture.units = vec![
        third.clone(),
        second.clone(),
        seventh.clone(),
        first.clone(),
        fifth.clone(),
        fourth.clone(),
        sixth.clone(),
    ];
    fixture.produced.insert(fourth.entity, IslandId(8));
    fixture.produced.insert(fifth.entity, IslandId(8));
    fixture.produced.insert(sixth.entity, IslandId(8));
    fixture.roadmap.insert(fourth.entity, IslandId(2));
    fixture.operations.push(ExistingCampaignOperation {
        island_id: IslandId(3),
        target_position: first.position,
        transport_phase: None,
        is_forming: false,
        transport_entities: Vec::new(),
        capture_entities: vec![fourth.entity, fifth.entity],
        combat_entities: Vec::new(),
    });
    // live能力のない古い作戦は、生産意図を上書きしない。
    fixture.operations.push(ExistingCampaignOperation {
        island_id: IslandId(1),
        target_position: first.position,
        transport_phase: None,
        is_forming: false,
        transport_entities: vec![sixth.entity],
        capture_entities: Vec::new(),
        combat_entities: Vec::new(),
    });
    let mut expected = vec![
        fixture.expected(&first),
        fixture.expected(&second),
        fixture.expected(&third),
        fixture.expected(&seventh),
        fixture.expected(&fourth),
        fixture.expected(&fifth),
        fixture.expected(&sixth),
    ];
    expected[4].assigned_island = Some(IslandId(2));
    expected[5].assigned_island = Some(IslandId(3));
    expected[6].assigned_island = Some(IslandId(8));
    fixture.assert_pool(expected);
}

#[test]
fn resource_pool_keeps_entity_and_island_tie_breaks() {
    let mut fixture = Fixture::new();
    let first = fixture.unit(10, UnitType::Infantry);
    let second = fixture.unit(11, UnitType::Infantry);
    fixture.units = vec![second.clone(), first.clone()];
    fixture.assert_pool(vec![fixture.expected(&first), fixture.expected(&second)]);

    fixture.map = Map::new(3, 1, Terrain::Sea, GridTopology::Square);
    fixture.map.set_terrain(0, 0, Terrain::Plains).unwrap();
    fixture.map.set_terrain(2, 0, Terrain::Plains).unwrap();
    fixture.islands = IslandMap::analyze(&fixture.map);
    let mut second = second;
    second.position = GridPosition { x: 2, y: 0 };
    fixture.units = vec![second.clone(), first.clone()];
    let mut expected = vec![fixture.expected(&first), fixture.expected(&second)];
    expected[0].reachable_positions = vec![first.position];
    expected[1].reachable_positions = vec![second.position];
    expected.sort_by_key(|unit| unit.island_id.map(|island| island.0));
    fixture.assert_pool(expected);
}

#[test]
fn resource_pool_transport_phase_availability_is_characterized() {
    // 空cargoでallが真になること、空Returnだけが再利用可能なことも固定する。
    let cases = [
        (None, false, true),
        (None, true, false),
        (
            Some(AssignedTransportState::Forming(Some(IslandId(1)), true)),
            false,
            true,
        ),
        (
            Some(AssignedTransportState::Forming(None, true)),
            false,
            false,
        ),
        (
            Some(AssignedTransportState::Forming(Some(IslandId(2)), true)),
            false,
            false,
        ),
        (
            Some(AssignedTransportState::Forming(Some(IslandId(1)), false)),
            false,
            false,
        ),
        (
            Some(AssignedTransportState::Phase(
                TransportPhase::Transit,
                Some(IslandId(1)),
                true,
            )),
            true,
            true,
        ),
        (
            Some(AssignedTransportState::Phase(
                TransportPhase::Return,
                Some(IslandId(1)),
                false,
            )),
            false,
            true,
        ),
        (
            Some(AssignedTransportState::Phase(
                TransportPhase::Return,
                Some(IslandId(1)),
                true,
            )),
            true,
            false,
        ),
        (
            Some(AssignedTransportState::Phase(
                TransportPhase::Drop,
                None,
                true,
            )),
            false,
            false,
        ),
        (Some(AssignedTransportState::Other), false, false),
    ];
    for (state, loaded, available) in cases {
        let mut fixture = Fixture::new();
        let mut transport = fixture.unit(1, UnitType::TransportHelicopter);
        transport.free_cargo_slots = 1;
        let mut cargo = fixture.unit(2, UnitType::Infantry);
        cargo.transporting = Some(transport.entity);
        fixture.produced.insert(transport.entity, IslandId(1));
        fixture.produced.insert(cargo.entity, IslandId(1));
        if loaded {
            transport.loaded_cargo_entities.push(cargo.entity);
        }
        if let Some(state) = state {
            fixture.phases.insert(transport.entity, state);
        }
        // 積荷は索引用snapshotには含めるが候補には出さない。
        fixture.unavailable.insert(cargo.entity);
        fixture.units = vec![transport.clone(), cargo];
        let expected = if available {
            let mut expected = fixture.expected(&transport);
            expected.assigned_island = Some(IslandId(1));
            expected.available_cargo_slots = if loaded { 2 } else { 1 };
            expected.loaded_cargo_entities = transport.loaded_cargo_entities;
            vec![expected]
        } else {
            Vec::new()
        };
        fixture.assert_pool(expected);
    }
}

#[test]
fn resource_pool_rejects_inconsistent_loaded_cargo() {
    for invalid in 0..3 {
        let mut fixture = Fixture::new();
        let mut transport = fixture.unit(1, UnitType::TransportHelicopter);
        transport.free_cargo_slots = 1;
        let mut cargo = fixture.unit(2, UnitType::Infantry);
        cargo.transporting = Some(transport.entity);
        transport.loaded_cargo_entities.push(cargo.entity);
        fixture.produced.insert(transport.entity, IslandId(1));
        fixture.produced.insert(
            cargo.entity,
            if invalid == 0 {
                IslandId(2)
            } else {
                IslandId(1)
            },
        );
        if invalid == 1 {
            cargo.transporting = Some(Entity::from_raw(99));
        }
        fixture.phases.insert(
            transport.entity,
            AssignedTransportState::Forming(Some(IslandId(1)), true),
        );
        fixture.unavailable.insert(cargo.entity);
        fixture.units = vec![transport];
        if invalid != 2 {
            fixture.units.push(cargo);
        }
        fixture.assert_pool(Vec::new());
    }
}

#[test]
fn resource_pool_excludes_unavailable_fuel_empty_capacity_and_unassigned_cargo() {
    let mut fixture = Fixture::new();
    let mut fuel_zero = fixture.unit(1, UnitType::TransportHelicopter);
    fuel_zero.free_cargo_slots = 1;
    fuel_zero.fuel = Some(0);
    let capacity_zero = fixture.unit(2, UnitType::Lander);
    let mut cargo = fixture.unit(3, UnitType::Infantry);
    cargo.transporting = Some(fuel_zero.entity);
    let reserved = fixture.unit(4, UnitType::Infantry);
    let mut enemy = fixture.unit(5, UnitType::Infantry);
    enemy.faction = PlayerId(2);
    let mut foot = fixture.unit(6, UnitType::Infantry);
    foot.fuel = Some(0);
    fixture.unavailable.insert(reserved.entity);
    fixture.units = vec![
        fuel_zero,
        capacity_zero,
        cargo,
        reserved,
        enemy,
        foot.clone(),
    ];
    fixture.assert_pool(vec![fixture.expected(&foot)]);
}

#[test]
fn resource_pool_loaded_capacity_saturates_and_preserves_cargo_order() {
    let mut fixture = Fixture::new();
    let mut transport = fixture.unit(1, UnitType::TransportHelicopter);
    transport.free_cargo_slots = u32::MAX;
    let mut first = fixture.unit(2, UnitType::Infantry);
    let mut second = fixture.unit(3, UnitType::Infantry);
    first.transporting = Some(transport.entity);
    second.transporting = Some(transport.entity);
    transport.loaded_cargo_entities = vec![second.entity, first.entity];
    for entity in [transport.entity, first.entity, second.entity] {
        fixture.produced.insert(entity, IslandId(1));
    }
    fixture.phases.insert(
        transport.entity,
        AssignedTransportState::Forming(Some(IslandId(1)), true),
    );
    fixture.unavailable.extend([first.entity, second.entity]);
    fixture.units = vec![transport.clone(), first, second];
    let mut expected = fixture.expected(&transport);
    expected.available_cargo_slots = u32::MAX;
    expected.loaded_cargo_entities = transport.loaded_cargo_entities;
    expected.assigned_island = Some(IslandId(1));
    fixture.assert_pool(vec![expected]);
}

#[test]
fn resource_pool_local_capture_requires_reachable_unowned_property_and_unloaded_unit() {
    let mut fixture = Fixture::new();
    let infantry = fixture.unit(1, UnitType::Infantry);
    let mut loaded = fixture.unit(2, UnitType::Infantry);
    loaded.transporting = Some(Entity::from_raw(99));
    fixture.produced.insert(loaded.entity, IslandId(1));
    fixture.units = vec![infantry.clone(), loaded.clone()];
    fixture.properties.push(PropertySnapshot {
        position: GridPosition { x: 2, y: 0 },
        property: super::super::Property::new(Terrain::City, None, 100),
    });
    let mut expected = vec![fixture.expected(&infantry), fixture.expected(&loaded)];
    expected[0].can_secure_local_property = true;
    expected[1].assigned_island = Some(IslandId(1));
    fixture.assert_pool(expected.clone());
    fixture.properties[0].property.owner_id = Some(PlayerId(1));
    expected[0].can_secure_local_property = false;
    fixture.assert_pool(expected);

    // 同じ島でも車両には山を越せない。占領能力だけ付けた合成諸元で到達性を分離検証する。
    fixture.map.set_terrain(1, 0, Terrain::Mountain).unwrap();
    fixture.islands = IslandMap::analyze(&fixture.map);
    fixture.properties[0].property.owner_id = None;
    let mut vehicle = fixture.unit(3, UnitType::Tank);
    vehicle.stats.can_capture = true;
    fixture.units = vec![vehicle.clone()];
    let mut expected = fixture.expected(&vehicle);
    expected.reachable_positions = vec![vehicle.position];
    fixture.assert_pool(vec![expected]);
}

#[test]
fn resource_pool_orders_positions_by_row_then_column_and_keeps_empty_funds() {
    let mut fixture = Fixture::new();
    fixture.assert_pool(Vec::new());
    fixture.map = Map::new(2, 2, Terrain::Plains, GridTopology::Square);
    fixture.islands = IslandMap::analyze(&fixture.map);
    let unit = fixture.unit(1, UnitType::Infantry);
    fixture.units.push(unit.clone());
    let mut expected = fixture.expected(&unit);
    expected.reachable_positions = vec![
        GridPosition { x: 0, y: 0 },
        GridPosition { x: 1, y: 0 },
        GridPosition { x: 0, y: 1 },
        GridPosition { x: 1, y: 1 },
    ];
    fixture.assert_pool(vec![expected]);
}
