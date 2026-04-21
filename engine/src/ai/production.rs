use crate::components::{GridPosition, PlayerId, Property, Transporting, UnitStats};
use crate::events::ProduceUnitCommand;
use crate::resources::master_data::MasterDataRegistry;
use crate::resources::{Players, Terrain, UnitRegistry, UnitType};
use bevy_ecs::prelude::*;

/// 単純な生産AI。
/// 指定プレイヤーの空いている工場すべてに対して、歩兵の生産を試みます。
pub fn decide_production(world: &mut World, player_id: PlayerId) -> Vec<ProduceUnitCommand> {
    use crate::systems::production::can_produce_at;
    let mut commands = Vec::new();

    // 現在の資金を取得
    let current_funds = if let Some(players) = world.get_resource::<Players>() {
        players
            .0
            .iter()
            .find(|p| p.id == player_id)
            .map(|p| p.funds)
            .unwrap_or(0)
    } else {
        return commands;
    };

    // 補充・修理用に一定額(1000G)を温存するように計算
    let mut available_funds = current_funds.saturating_sub(1000);

    // 歩兵のコストを取得
    let infantry_cost = if let Some(registry) = world.get_resource::<UnitRegistry>() {
        if let Some(stats) = registry.get_stats(UnitType::Infantry) {
            stats.cost
        } else {
            return commands;
        }
    } else {
        return commands;
    };

    // 生産拠点を取得（マスターデータに基づく）
    let mut production_positions = Vec::new();
    {
        let master_data = world.resource::<MasterDataRegistry>().clone();
        let mut query = world.query::<(&GridPosition, &Property)>();
        for (pos, prop) in query.iter(world) {
            if prop.owner_id == Some(player_id)
                && master_data.is_production_facility(prop.terrain.as_str())
            {
                production_positions.push(*pos);
            }
        }
    }

    // ユニットがいる位置を取得（重なり判定用）
    let mut occupied_positions = std::collections::HashSet::new();
    {
        let mut unit_query =
            world.query_filtered::<&GridPosition, (With<UnitStats>, Without<Transporting>)>();
        for pos in unit_query.iter(world) {
            occupied_positions.insert(*pos);
        }
    }

    for pos in production_positions {
        if available_funds < infantry_cost {
            break;
        }
        // システム層のバリデーションを呼び出し
        let master_data = world.resource::<MasterDataRegistry>().clone();
        if can_produce_at(
            world,
            player_id,
            pos.x,
            pos.y,
            UnitType::Infantry,
            &master_data,
        )
        .is_ok()
        {
            commands.push(ProduceUnitCommand {
                target_x: pos.x,
                target_y: pos.y,
                unit_type: UnitType::Infantry,
                player_id,
            });
            available_funds -= infantry_cost;
        }
    }

    commands
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::Faction;
    use crate::resources::Player;
    use std::collections::HashMap;

    #[test]
    fn test_decide_production() {
        let mut world = World::new();
        let p1 = PlayerId(1);
        let p2 = PlayerId(2);

        // Setup Funds
        world.insert_resource(Players(vec![
            Player {
                id: p1,
                name: "P1".to_string(),
                funds: 3500,
            }, // Can afford 2 Infantry (cost 1000) with 1000 buffer
            Player {
                id: p2,
                name: "P2".to_string(),
                funds: 1000,
            },
        ]));

        // Setup Registry
        let mut registry = HashMap::new();
        registry.insert(
            UnitType::Infantry,
            crate::components::UnitStats {
                unit_type: UnitType::Infantry,
                cost: 1000,
                ..crate::components::UnitStats::mock()
            },
        );
        world.insert_resource(UnitRegistry(registry));

        // Setup factories
        // 0. Setup Capital (needed for can_produce_at scope)
        world.spawn((
            GridPosition { x: 0, y: 0 },
            Property::new(Terrain::Capital, Some(p1), 100),
        ));

        // Setup Registry
        let registry = MasterDataRegistry::load().unwrap();
        world.insert_resource(registry);

        // 1. P1 owned, empty -> Should produce (has 2500 funds)
        world.spawn((
            GridPosition { x: 1, y: 0 },
            Property::new(Terrain::Factory, Some(p1), 200),
        ));

        // 2. P1 owned, empty -> Should produce (has 2500 funds)
        world.spawn((
            GridPosition { x: 2, y: 0 },
            Property::new(Terrain::Factory, Some(p1), 200),
        ));

        // 3. P1 owned, empty -> Should NOT produce (funds 2500 -> 1500 -> 500, needs 1000)
        world.spawn((
            GridPosition { x: 3, y: 0 },
            Property::new(Terrain::Factory, Some(p1), 200),
        ));

        // 4. P1 owned, occupied -> Should NOT produce
        world.spawn((
            GridPosition { x: 4, y: 0 },
            Property::new(Terrain::Factory, Some(p1), 200),
        ));
        world.spawn((
            GridPosition { x: 4, y: 0 },
            Faction(p1),
            crate::components::UnitStats {
                ..crate::components::UnitStats::mock()
            },
        )); // Unit on top!

        // 5. Unowned factory -> Should NOT produce
        world.spawn((
            GridPosition { x: 5, y: 0 },
            Property::new(Terrain::Factory, None, 200),
        ));

        // 6. P2 owned factory -> Should NOT produce
        world.spawn((
            GridPosition { x: 6, y: 0 },
            Property::new(Terrain::Factory, Some(p2), 200),
        ));

        // Execute decide_production
        let commands = decide_production(&mut world, p1);

        // We expect 3 commands for (0,0) (Capital), (1,0) (Factory) and (2,0) (Factory)
        // (3,0) is skipped because of funds (needs 1000 buffer + 3*1000 = 4000, but has 3500)
        assert_eq!(commands.len(), 2); // Wait, if funds 3500, available 2500. So 2 units.
        // The order might depend on query order, but usually (0,0) comes first.
        assert_eq!(commands[0].target_x, 0); 
        assert_eq!(commands[1].target_x, 1);
    }

    #[test]
    fn test_ai_production_skips_occupied_factory() {
        let mut world = World::new();
        let p1 = PlayerId(1);

        // Setup Funds
        world.insert_resource(Players(vec![Player {
            id: p1,
            name: "P1".to_string(),
            funds: 5000,
        }]));

        // Setup Registry
        let mut registry = HashMap::new();
        registry.insert(
            UnitType::Infantry,
            crate::components::UnitStats {
                unit_type: UnitType::Infantry,
                cost: 1000,
                ..crate::components::UnitStats::mock()
            },
        );
        world.insert_resource(UnitRegistry(registry));

        // Setup MasterDataRegistry
        world.insert_resource(MasterDataRegistry::load().unwrap());

        // Setup Capital
        world.spawn((
            GridPosition { x: 0, y: 0 },
            Property::new(Terrain::Capital, Some(p1), 100),
        ));

        // Case: Factory at (0,0) (Capital position) with a unit
        // Factory at (0,0)
        world.spawn((
            GridPosition { x: 0, y: 0 },
            Property::new(Terrain::Factory, Some(p1), 200),
        ));
        // Factory at (0,0) occupied by a "Wait" unit
        world.spawn((
            GridPosition { x: 0, y: 0 },
            Faction(p1),
            crate::components::UnitStats {
                ..crate::components::UnitStats::mock()
            },
            crate::components::ActionCompleted(true),
        ));

        // Factory empty at (1,0)
        world.spawn((
            GridPosition { x: 1, y: 0 },
            Property::new(Terrain::Factory, Some(p1), 200),
        ));

        let commands = decide_production(&mut world, p1);

        // Should only produce on (1,0), not on (0,0)
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].target_x, 1);
    }
}
