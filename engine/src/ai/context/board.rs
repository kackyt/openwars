//! 1回の戦略分析・部隊再編で共有する、player視点の盤面snapshot（読み取り専用）。
//!
//! 戦略分析とSquad編成は、同じ盤面からunit・物件・地図を別々に集め直していた。
//! [`PlayerBoard`] はそれらを1回だけWorldから写し取り、両者へ同じ事実を渡す。
//!
//! 占有モデルは戦略分析とSquad編成で共通の [`PlayerBoard::occupancy`] 1つだけを使う。
//! 搭載中のunitは地図上に居ないので除き、輸送unitの空き枠は実際の積載から求める。
//!
//! unitと物件はWorldのクエリ列挙順のまま保持する。占有や所有のmapはこの順に挿入するため、
//! 個別にWorldを走査していたときと同じ内容・同じ反復順になる。

use crate::ai::islands::IslandMap;
use crate::collections::HashMap;
use crate::components::{
    ActionCompleted, Ammo, CargoCapacity, Faction, Fuel, GridPosition, Health, PlayerId, Property,
    Transporting, UnitStats,
};
use crate::resources::master_data::MasterDataRegistry;
use crate::resources::{Map, Terrain};
use crate::systems::movement::OccupantInfo;
use bevy_ecs::prelude::*;

/// 輸送枠（最大積載数と現在の積載数）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CargoSlots {
    pub max: u32,
    pub loaded: u32,
}

/// 盤面上の1unitの事実。
#[derive(Debug, Clone)]
pub(crate) struct BoardUnit {
    pub entity: Entity,
    pub owner: PlayerId,
    pub position: GridPosition,
    pub stats: UnitStats,
    /// 輸送unitに搭載中か。
    pub transported: bool,
    pub cargo: Option<CargoSlots>,
    pub health: Option<u32>,
    /// 弾薬（主・副）。
    pub ammo: Option<(u32, u32)>,
    pub fuel: Option<u32>,
    pub action_completed: Option<bool>,
}

impl BoardUnit {
    /// 地図上に居るか（搭載中・マップ外の退避座標を除く）。
    pub fn is_deployed(&self) -> bool {
        self.position.x < 9999 && !self.transported
    }
}

/// 盤面上の1物件の事実。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BoardProperty {
    pub position: GridPosition,
    pub owner: Option<PlayerId>,
    /// 物件自身の地形種別（`Property::terrain`）。
    pub terrain: Terrain,
}

/// player視点の盤面snapshot。
pub(crate) struct PlayerBoard {
    pub player: PlayerId,
    pub map: Map,
    pub registry: MasterDataRegistry,
    pub island_map: IslandMap,
    /// 全unit（勢力を問わない。Worldのクエリ列挙順）。
    pub units: Vec<BoardUnit>,
    /// 全物件（Worldのクエリ列挙順）。
    pub properties: Vec<BoardProperty>,
}

impl PlayerBoard {
    /// Worldから盤面を1回だけ写し取る。Worldは書き換えない。
    pub fn observe(world: &mut World, player: PlayerId) -> Self {
        let map = world.resource::<Map>().clone();
        let registry = world
            .get_resource::<MasterDataRegistry>()
            .cloned()
            .unwrap_or_else(|| MasterDataRegistry::load().unwrap_or_default());
        let island_map = world
            .get_resource::<IslandMap>()
            .cloned()
            .unwrap_or_else(|| IslandMap::analyze(&map));

        let mut unit_query = world.query::<(
            Entity,
            &Faction,
            &GridPosition,
            &UnitStats,
            Option<&Transporting>,
            Option<&CargoCapacity>,
            Option<&Health>,
            Option<&Ammo>,
            Option<&Fuel>,
            Option<&ActionCompleted>,
        )>();
        let units = unit_query
            .iter(world)
            .map(
                |(
                    entity,
                    faction,
                    position,
                    stats,
                    transporting,
                    cargo,
                    health,
                    ammo,
                    fuel,
                    done,
                )| {
                    BoardUnit {
                        entity,
                        owner: faction.0,
                        position: *position,
                        stats: stats.clone(),
                        transported: transporting.is_some(),
                        cargo: cargo.map(|capacity| CargoSlots {
                            max: capacity.max,
                            loaded: capacity.loaded.len() as u32,
                        }),
                        health: health.map(|health| health.current),
                        ammo: ammo.map(|ammo| (ammo.ammo1, ammo.ammo2)),
                        fuel: fuel.map(|fuel| fuel.current),
                        action_completed: done.map(|completed| completed.0),
                    }
                },
            )
            .collect();

        let mut property_query = world.query::<(&GridPosition, &Property)>();
        let properties = property_query
            .iter(world)
            .map(|(position, property)| BoardProperty {
                position: *position,
                owner: property.owner_id,
                terrain: property.terrain,
            })
            .collect();

        Self {
            player,
            map,
            registry,
            island_map,
            units,
            properties,
        }
    }

    /// 盤面の占有。搭載中のunitを除き、輸送unitの空き枠は実際の積載から求める。
    ///
    /// 戦略分析（首都脅威・物件脅威・対空）と編成のターン距離が同じ占有を読む。
    /// 搭載中のunitは座標が残っていても地図上の進路を塞がない。
    pub fn occupancy(&self) -> HashMap<(usize, usize), OccupantInfo> {
        let mut occupancy = HashMap::default();
        for unit in self.units.iter().filter(|unit| !unit.transported) {
            let free_slots = unit
                .cargo
                .map(|cargo| cargo.max.saturating_sub(cargo.loaded))
                .unwrap_or(0);
            occupancy.insert(
                (unit.position.x, unit.position.y),
                occupant_info(unit, free_slots),
            );
        }
        occupancy
    }

    /// 物件の座標と所有者（クエリ列挙順に挿入）。
    pub fn property_ownership(&self) -> HashMap<GridPosition, Option<PlayerId>> {
        let mut ownership = HashMap::default();
        for property in &self.properties {
            ownership.insert(property.position, property.owner);
        }
        ownership
    }
}

fn occupant_info(unit: &BoardUnit, free_slots: u32) -> OccupantInfo {
    OccupantInfo {
        player_id: unit.owner,
        is_transport: unit.stats.max_cargo > 0,
        unit_type: unit.stats.unit_type,
        loadable_types: unit.stats.loadable_unit_types.clone(),
        free_slots,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resources::{GridTopology, UnitType};

    fn board_world() -> (World, Entity, Entity) {
        let mut world = World::new();
        world.insert_resource(Map::new(4, 1, Terrain::Plains, GridTopology::Square));
        world.insert_resource(MasterDataRegistry::load().unwrap());
        let mut transport_stats = UnitStats::mock();
        transport_stats.unit_type = UnitType::Lander;
        transport_stats.max_cargo = 2;
        let transport = world
            .spawn((
                Faction(PlayerId(1)),
                GridPosition { x: 1, y: 0 },
                transport_stats,
                CargoCapacity {
                    max: 2,
                    loaded: Vec::new(),
                },
            ))
            .id();
        let cargo = world
            .spawn((
                Faction(PlayerId(1)),
                GridPosition { x: 1, y: 0 },
                UnitStats::mock(),
                Transporting(transport),
            ))
            .id();
        world
            .get_mut::<CargoCapacity>(transport)
            .unwrap()
            .loaded
            .push(cargo);
        (world, transport, cargo)
    }

    #[test]
    fn occupancy_excludes_cargo_and_counts_actual_free_slots() {
        let (mut world, _, cargo) = board_world();
        let board = PlayerBoard::observe(&mut world, PlayerId(1));

        // 同じ座標に搭載中unitが居ても、列挙順に関係なく輸送unitが占有者になる。
        let occupancy = board.occupancy();
        let occupant = &occupancy[&(1, 0)];
        assert!(occupant.is_transport);
        assert_eq!(occupant.free_slots, 1);
        assert_eq!(occupancy.len(), 1);
        let cargo_unit = board
            .units
            .iter()
            .find(|unit| unit.entity == cargo)
            .unwrap();
        assert!(!cargo_unit.is_deployed());
    }

    #[test]
    fn occupancy_ignores_a_loaded_unit_left_on_another_tile() {
        let (mut world, _, cargo) = board_world();
        *world.get_mut::<GridPosition>(cargo).unwrap() = GridPosition { x: 3, y: 0 };
        let board = PlayerBoard::observe(&mut world, PlayerId(1));

        assert!(!board.occupancy().contains_key(&(3, 0)));
    }
}
