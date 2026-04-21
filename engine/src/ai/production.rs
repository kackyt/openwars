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

    // プレイヤーの工場を取得
    let mut factory_positions = Vec::new();
    {
        let mut query = world.query::<(&GridPosition, &Property)>();
        for (pos, prop) in query.iter(world) {
            if prop.owner_id == Some(player_id) && prop.terrain == Terrain::Factory {
                factory_positions.push(*pos);
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

    for pos in factory_positions {
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
        // 1. P1 owned, empty -> Should produce
        world.spawn((
            GridPosition { x: 0, y: 0 },
            Property::new(Terrain::Factory, Some(p1), 200),
        ));

        // 2. P1 owned, empty -> Should produce (has 2500 funds)
        world.spawn((
            GridPosition { x: 1, y: 0 },
            Property::new(Terrain::Factory, Some(p1), 200),
        ));

        // 3. P1 owned, empty -> Should NOT produce (funds 2500 -> 1500 -> 500, needs 1000)
        world.spawn((
            GridPosition { x: 2, y: 0 },
            Property::new(Terrain::Factory, Some(p1), 200),
        ));

        // 4. P1 owned, occupied -> Should NOT produce
        world.spawn((
            GridPosition { x: 3, y: 0 },
            Property::new(Terrain::Factory, Some(p1), 200),
        ));
        world.spawn((
            GridPosition { x: 3, y: 0 },
            Faction(p1),
            crate::components::UnitStats {
                ..crate::components::UnitStats::mock()
            },
        )); // Unit on top!

        // 5. Unowned factory -> Should NOT produce
        world.spawn((
            GridPosition { x: 4, y: 0 },
            Property::new(Terrain::Factory, None, 200),
        ));

        // 6. P2 owned factory -> Should NOT produce
        world.spawn((
            GridPosition { x: 5, y: 0 },
            Property::new(Terrain::Factory, Some(p2), 200),
        ));

        // Execute decide_production
        let commands = decide_production(&mut world, p1);

        // We expect exactly 2 commands for (0,0) and (1,0)
        assert_eq!(commands.len(), 2);
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

        // Factory with a "Wait" unit (ActionCompleted=true)
        world.spawn((
            GridPosition { x: 0, y: 0 },
            Property::new(Terrain::Factory, Some(p1), 200),
        ));
        world.spawn((
            GridPosition { x: 0, y: 0 },
            Faction(p1),
            crate::components::UnitStats {
                ..crate::components::UnitStats::mock()
            },
            crate::components::ActionCompleted(true), // Waiting unit
        ));

        // Factory empty
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
