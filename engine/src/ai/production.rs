use crate::components::{GridPosition, PlayerId, Property};
use crate::events::ProduceUnitCommand;
use crate::resources::{Players, Terrain, UnitRegistry, UnitType};
use bevy_ecs::prelude::*;

/// 単純な生産AI。
/// 指定プレイヤーの空いている工場すべてに対して、歩兵の生産を試みます。
pub fn decide_production(world: &mut World, player_id: PlayerId) -> Vec<ProduceUnitCommand> {
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

    let mut available_funds = current_funds;

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
        let mut unit_query = world.query::<&GridPosition>();
        for pos in unit_query.iter(world) {
            occupied_positions.insert(*pos);
        }
    }

    for pos in factory_positions {
        // 資金不足なら終了
        if available_funds < infantry_cost {
            break;
        }

        // 工場の上にユニットがいなければ生産コマンドを追加
        if !occupied_positions.contains(&pos) {
            commands.push(ProduceUnitCommand {
                target_x: pos.x,
                target_y: pos.y,
                unit_type: UnitType::Infantry,
                player_id,
            });
            available_funds -= infantry_cost;
            occupied_positions.insert(pos);
        }
    }

    commands
}
