//! 海上戦域のテスト用の盤面・World。
use crate::ai::context::board::{BoardProperty, PlayerBoard};
use crate::ai::islands::IslandMap;
use crate::components::{
    ActionCompleted, Ammo, CargoCapacity, Faction, Fuel, GridPosition, HasMoved, Health, PlayerId,
    Transporting,
};
use crate::resources::master_data::{MasterDataRegistry, UnitName};
use crate::resources::{DamageChart, GridTopology, Map, Terrain, UnitType};
use bevy_ecs::prelude::*;

/// 文字から地形へ。`.`平地 `~`海 `,`浅瀬 `H`首都 `P`港 `C`都市 `F`工場 `A`空港。
fn terrain_of(symbol: char) -> Terrain {
    match symbol {
        '~' => Terrain::Sea,
        ',' => Terrain::Shoal,
        'H' => Terrain::Capital,
        'P' => Terrain::Port,
        'C' => Terrain::City,
        'F' => Terrain::Factory,
        'A' => Terrain::Airport,
        _ => Terrain::Plains,
    }
}

/// 文字列の地図（正方格子）を作る。
pub(crate) fn map_from_rows(rows: &[&str]) -> Map {
    let height = rows.len();
    let width = rows[0].len();
    let mut map = Map::new(width, height, Terrain::Plains, GridTopology::Square);
    for (y, row) in rows.iter().enumerate() {
        for (x, symbol) in row.chars().enumerate() {
            map.set_terrain(x, y, terrain_of(symbol)).unwrap();
        }
    }
    map
}

/// 文字列の地図から盤面を作る。物件は (x, y, 所有者; 0は中立)。
pub(crate) fn board_from_rows(rows: &[&str], properties: &[(usize, usize, u32)]) -> PlayerBoard {
    let map = map_from_rows(rows);
    let properties = properties
        .iter()
        .map(|(x, y, owner)| BoardProperty {
            position: GridPosition { x: *x, y: *y },
            owner: (*owner > 0).then_some(PlayerId(*owner)),
            terrain: map.get_terrain(*x, *y).unwrap(),
        })
        .collect();
    PlayerBoard {
        player: PlayerId(1),
        island_map: IslandMap::analyze(&map),
        map,
        registry: MasterDataRegistry::load().unwrap(),
        units: Vec::new(),
        properties,
    }
}

/// 文字列の地図を持つWorldを作る。
pub(crate) fn naval_world(rows: &[&str]) -> World {
    let mut world = World::new();
    world.insert_resource(map_from_rows(rows));
    world.insert_resource(MasterDataRegistry::load().unwrap());
    world.insert_resource(DamageChart::new());
    world
}

/// 物件を置く。
pub(crate) fn property(world: &mut World, owner: u32, x: usize, y: usize) -> Entity {
    let terrain = world.resource::<Map>().get_terrain(x, y).unwrap();
    world
        .spawn((
            GridPosition { x, y },
            crate::components::Property {
                terrain,
                owner_id: (owner > 0).then_some(PlayerId(owner)),
                capture_points: 200,
                max_capture_points: 200,
            },
        ))
        .id()
}

/// 未行動のunitを置く。
pub(crate) fn spawn(
    world: &mut World,
    owner: u32,
    unit_type: UnitType,
    x: usize,
    y: usize,
) -> Entity {
    let stats = world
        .resource::<MasterDataRegistry>()
        .create_unit_stats(&UnitName(unit_type.as_str().to_owned()))
        .unwrap();
    let entity = world
        .spawn((
            GridPosition { x, y },
            Faction(PlayerId(owner)),
            Health {
                current: 100,
                max: 100,
            },
            Fuel {
                current: stats.max_fuel,
                max: stats.max_fuel,
            },
            Ammo {
                ammo1: stats.max_ammo1,
                max_ammo1: stats.max_ammo1,
                ammo2: stats.max_ammo2,
                max_ammo2: stats.max_ammo2,
            },
            HasMoved(false),
            ActionCompleted(false),
        ))
        .id();
    if stats.max_cargo > 0 {
        world.entity_mut(entity).insert(CargoCapacity {
            max: stats.max_cargo,
            loaded: Vec::new(),
        });
    }
    world.entity_mut(entity).insert(stats);
    entity
}

/// 輸送部隊へ指定種別の部隊を搭載した状態にする。
pub(crate) fn load(
    world: &mut World,
    transport: Entity,
    unit_type: UnitType,
    owner: u32,
) -> Entity {
    let position = *world.get::<GridPosition>(transport).unwrap();
    let cargo = spawn(world, owner, unit_type, position.x, position.y);
    world.entity_mut(cargo).insert(Transporting(transport));
    world
        .get_mut::<CargoCapacity>(transport)
        .unwrap()
        .loaded
        .push(cargo);
    cargo
}
