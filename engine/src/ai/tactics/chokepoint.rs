//! 橋とその近辺（隘路）の通行を空ける。
//!
//! 橋は川・海を渡る唯一の通路になりやすい。間接火力は移動した手番に撃てないため、
//! 撃ち続けようとすると橋やその手前に居座り、後続の直接部隊（戦車・歩兵）が渡れなくなる。
//! 非戦闘の支援車（補給輸送車など）も同様に通路を塞ぐ。
//!
//! そこで、橋とその近辺では間接火力・非戦闘の支援車を「止めない」。区域内では待機せず、
//! 区域内のマスから射撃もしない。区域を通過するための移動（移動後に区域内で手番を終える
//! ことを含む）は許すので、長い橋でも数手番かけて渡れる。直接部隊には制限を掛けない。

use crate::collections::HashSet;
use crate::components::{GridPosition, UnitStats};
use crate::resources::{Map, Terrain};

/// 橋の端から何マスまでを「橋の近辺」とみなすか。
pub(crate) const BRIDGE_APPROACH_RADIUS: u32 = 2;

/// 橋のマスと、その近辺（橋から `BRIDGE_APPROACH_RADIUS` マス以内）の陸のマス。
pub(crate) fn bridge_approach_cells(map: &Map) -> HashSet<GridPosition> {
    let bridges = (0..map.height)
        .flat_map(|y| (0..map.width).map(move |x| (x, y)))
        .filter(|&(x, y)| map.get_terrain(x, y) == Some(Terrain::Bridge))
        .collect::<Vec<_>>();
    let mut cells = HashSet::default();
    if bridges.is_empty() {
        return cells;
    }
    for y in 0..map.height {
        for x in 0..map.width {
            if bridges
                .iter()
                .any(|&(bx, by)| map.distance(x, y, bx, by) <= BRIDGE_APPROACH_RADIUS)
                && !matches!(map.get_terrain(x, y), Some(Terrain::Sea | Terrain::Shoal))
            {
                cells.insert(GridPosition { x, y });
            }
        }
    }
    cells
}

/// 橋とその近辺で止まらせない兵種か。間接火力と、武器も占領能力も持たない地上の支援車。
/// 輸送艦・航空機は橋を塞がないので対象にしない。
pub(crate) fn keeps_bridge_approach_clear(stats: &UnitStats) -> bool {
    let ground = !matches!(
        stats.movement_type,
        crate::resources::MovementType::Air | crate::resources::MovementType::Ship
    );
    ground && (stats.min_range > 1 || (stats.max_range == 0 && !stats.can_capture))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resources::GridTopology;

    fn river_map() -> Map {
        // x=5 の列が川、(5,2) だけが橋。
        let mut map = Map::new(11, 5, Terrain::Plains, GridTopology::Square);
        for y in 0..5 {
            map.set_terrain(5, y, Terrain::River).unwrap();
        }
        map.set_terrain(5, 2, Terrain::Bridge).unwrap();
        map
    }

    #[test]
    fn approach_covers_the_bridge_and_its_mouths() {
        let cells = bridge_approach_cells(&river_map());
        let at = |x, y| GridPosition { x, y };
        assert!(cells.contains(&at(5, 2)), "橋そのもの");
        assert!(
            cells.contains(&at(4, 2)) && cells.contains(&at(3, 2)),
            "手前の口"
        );
        assert!(
            cells.contains(&at(6, 2)) && cells.contains(&at(7, 2)),
            "向こう岸の口"
        );
        assert!(
            !cells.contains(&at(2, 2)) && !cells.contains(&at(8, 2)),
            "離れた所は対象外"
        );
    }

    fn spawn(
        world: &mut bevy_ecs::world::World,
        player: u32,
        kind: crate::resources::UnitType,
        at: GridPosition,
    ) -> bevy_ecs::entity::Entity {
        use crate::components::{ActionCompleted, Ammo, Faction, Fuel, HasMoved, Health, PlayerId};
        let stats = world
            .resource::<crate::resources::master_data::MasterDataRegistry>()
            .create_unit_stats(&crate::resources::master_data::UnitName(
                kind.as_str().to_owned(),
            ))
            .unwrap();
        world
            .spawn((
                Faction(PlayerId(player)),
                at,
                Health {
                    current: 100,
                    max: 100,
                },
                Ammo {
                    ammo1: stats.max_ammo1,
                    max_ammo1: stats.max_ammo1,
                    ammo2: stats.max_ammo2,
                    max_ammo2: stats.max_ammo2,
                },
                Fuel {
                    current: stats.max_fuel,
                    max: stats.max_fuel,
                },
                HasMoved(false),
                ActionCompleted(false),
                stats,
            ))
            .id()
    }

    fn bridge_world() -> bevy_ecs::world::World {
        let (mut world, _) = crate::setup::create_world();
        let map = river_map();
        world.insert_resource(crate::ai::islands::IslandMap::analyze(&map));
        world.insert_resource(map);
        world.insert_resource(crate::resources::master_data::MasterDataRegistry::load().unwrap());
        let mut chart = crate::resources::DamageChart::new();
        chart.insert_damage(
            crate::resources::UnitType::Artillery,
            crate::resources::UnitType::Infantry,
            90,
        );
        chart.insert_damage(
            crate::resources::UnitType::Tank,
            crate::resources::UnitType::Infantry,
            75,
        );
        world.insert_resource(chart);
        let mut settings = crate::ai::PlayerAiSettings::new();
        settings.set_version(crate::components::PlayerId(1), crate::ai::AiVersion::V4);
        world.insert_resource(settings);
        world
    }

    /// 橋の口にいる砲台は、射程内に敵がいても居座って撃たず、区域の外か先へ動く。
    #[test]
    fn indirect_fire_neither_stops_nor_fires_at_the_bridge_mouth() {
        use crate::ai::command::AiCommand;
        let mut world = bridge_world();
        let at = |x, y| GridPosition { x, y };
        let gun = spawn(
            &mut world,
            1,
            crate::resources::UnitType::Artillery,
            at(4, 2),
        );
        spawn(
            &mut world,
            2,
            crate::resources::UnitType::Infantry,
            at(7, 2),
        );
        let decision = crate::ai::tactics::v2::decide_action(
            &mut world,
            crate::components::PlayerId(1),
            &Default::default(),
        );
        match decision {
            Some((actor, AiCommand::Wait { target_pos })) => {
                assert_eq!(actor, gun);
                assert_ne!(target_pos, at(4, 2), "橋の口で待機しない");
            }
            Some((_, AiCommand::Attack { .. })) => panic!("橋の口から撃たない: {decision:?}"),
            other => panic!("砲台が動く: {other:?}"),
        }
    }

    /// 直接部隊は橋の口でも従来どおり攻撃できる。
    #[test]
    fn direct_units_may_fight_at_the_bridge_mouth() {
        use crate::ai::command::AiCommand;
        let mut world = bridge_world();
        let at = |x, y| GridPosition { x, y };
        let tank = spawn(&mut world, 1, crate::resources::UnitType::Tank, at(4, 2));
        spawn(
            &mut world,
            2,
            crate::resources::UnitType::Infantry,
            at(5, 2),
        );
        let decision = crate::ai::tactics::v2::decide_action(
            &mut world,
            crate::components::PlayerId(1),
            &Default::default(),
        );
        assert!(
            matches!(decision, Some((actor, AiCommand::Attack { .. })) if actor == tank),
            "{decision:?}"
        );
    }

    #[test]
    fn map_without_bridges_has_no_approach() {
        let map = Map::new(5, 5, Terrain::Plains, GridTopology::Square);
        assert!(bridge_approach_cells(&map).is_empty());
    }

    #[test]
    fn only_indirect_and_unarmed_support_keep_the_bridge_clear() {
        let master = crate::resources::master_data::MasterDataRegistry::load().unwrap();
        let stats = |name: crate::resources::UnitType| {
            master
                .create_unit_stats(&crate::resources::master_data::UnitName(
                    name.as_str().to_owned(),
                ))
                .unwrap()
        };
        use crate::resources::UnitType::*;
        for unit in [Artillery, HeavySpGun, Rockets, SupplyTruck] {
            assert!(keeps_bridge_approach_clear(&stats(unit)), "{unit:?}");
        }
        for unit in [Infantry, Mech, Tank, MdTank, Recon] {
            assert!(!keeps_bridge_approach_clear(&stats(unit)), "{unit:?}");
        }
    }
}
