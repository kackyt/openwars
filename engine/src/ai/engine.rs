use crate::components::{
    ActionCompleted, Faction, GridPosition, HasMoved, Health, PlayerId, Property, UnitStats,
};
use crate::events::{AttackUnitCommand, CapturePropertyCommand, MoveUnitCommand, WaitUnitCommand};
use crate::resources::master_data::MasterDataRegistry;
use crate::resources::{Map, Terrain};
use crate::systems::combat::get_expected_damage;
use crate::systems::movement::{OccupantInfo, calculate_reachable_tiles};
use bevy_ecs::prelude::*;
use std::collections::HashMap;
use std::collections::HashSet;

#[derive(Resource, Default)]
pub struct AiActionCooldown(pub HashSet<Entity>);

#[derive(Debug, Clone)]
pub enum AiCommand {
    Attack {
        target_pos: GridPosition,
        target_entity: Entity,
    },
    Capture {
        target_pos: GridPosition,
    },
    Wait {
        target_pos: GridPosition,
    },
    Merge {
        target_pos: GridPosition,
        target_entity: Entity,
    },
    Load {
        target_pos: GridPosition,
        transport_entity: Entity,
    },
    Supply {
        target_pos: GridPosition,
        target_entity: Entity,
    },
}

/// AIの思考エンジン。未行動のユニットに対して最も評価の高いコマンドを決定します。
pub fn decide_ai_action(
    world: &mut World,
    player_id: PlayerId,
    skip_entities: &std::collections::HashSet<Entity>,
) -> Option<(Entity, AiCommand)> {
    // 1. 行動可能なユニットを収集
    let mut movable_units = Vec::new();
    let mut unit_positions = HashMap::new();
    {
        let mut query = world.query::<(
            Entity,
            &GridPosition,
            &Faction,
            &HasMoved,
            &ActionCompleted,
            &UnitStats,
            Option<&crate::components::CargoCapacity>,
            Option<&crate::components::Transporting>,
        )>();
        for (
            entity,
            pos,
            faction,
            has_moved,
            action_completed,
            stats,
            cargo_opt,
            transporting_opt,
        ) in query.iter(world)
        {
            // 輸送中のユニットはマップ上に実体がないためスキップ
            if transporting_opt.is_some() {
                continue;
            }

            // movable_units への登録判定（行動候補）
            if !skip_entities.contains(&entity)
                && faction.0 == player_id
                && !has_moved.0
                && !action_completed.0
            {
                movable_units.push(entity);
            }

            // 占有情報の登録（輸送中以外は常に全ユニット対象）
            let free_slots = cargo_opt
                .map(|c| c.max.saturating_sub(c.loaded.len() as u32))
                .unwrap_or(0);
            unit_positions.insert(
                (pos.x, pos.y),
                OccupantInfo {
                    player_id: faction.0,
                    is_transport: stats.max_cargo > 0,
                    unit_type: stats.unit_type,
                    loadable_types: stats.loadable_unit_types.clone(),
                    free_slots,
                },
            );
        }
    }

    if movable_units.is_empty() {
        return None;
    }

    // 2. 行動可能なユニットを順に評価
    let mut best_overall_score = i32::MIN;
    let mut best_overall_choice: Option<(Entity, AiCommand)> = None;

    for unit_entity in movable_units {
        let (stats, pos, fuel, atk_hp, atk_ammo) = {
            let stats = world.get::<UnitStats>(unit_entity).cloned();
            let pos = world.get::<GridPosition>(unit_entity).cloned();
            let fuel = world
                .get::<crate::components::Fuel>(unit_entity)
                .map(|f| f.current);
            let health = world.get::<Health>(unit_entity).map(|h| h.current);
            let ammo = world
                .get::<crate::components::Ammo>(unit_entity)
                .map(|a| (a.ammo1, a.ammo2))
                .unwrap_or((99, 99));

            // この時点では transported 判定は不要（movable_units収集時に除外済み）
            if stats.is_none() || pos.is_none() || fuel.is_none() || health.is_none() {
                continue;
            }
            (
                stats.unwrap(),
                pos.unwrap(),
                fuel.unwrap(),
                health.unwrap(),
                ammo,
            )
        };

        let map = world.resource::<Map>().clone();
        let registry = world.resource::<MasterDataRegistry>().clone();

        // 3. 到達可能タイルを算出
        let reachable = calculate_reachable_tiles(
            &map,
            &unit_positions,
            (pos.x, pos.y),
            stats.movement_type,
            stats.max_movement,
            fuel,
            player_id,
            stats.unit_type,
            &registry,
        );

        // 4. 共通リソースの取得（接近スコア計算用）
        let properties: Vec<(GridPosition, Terrain, Option<PlayerId>)> = {
            let mut q = world.query::<(&GridPosition, &Property)>();
            q.iter(world)
                .map(|(p, prop)| (*p, prop.terrain, prop.owner_id))
                .collect()
        };

        // 全敵ユニット情報を収集（ターゲット評価用）
        let enemy_units: Vec<(GridPosition, crate::resources::UnitType, u32, u32)> = {
            let mut q = world.query::<(&GridPosition, &Faction, &UnitStats, &Health)>();
            q.iter(world)
                .filter(|(_, f, _, h)| f.0 != player_id && h.current > 0)
                .map(|(p, _, s, h)| (*p, s.unit_type, s.cost, h.current))
                .collect()
        };

        let damage_chart = world.resource::<crate::resources::DamageChart>().clone();

        let mut best_unit_score = i32::MIN;
        let mut best_unit_choice: Option<AiCommand> = None;

        // 5. 各到達可能タイルにおいて、実行可能なアクションを判定
        for target_tile in reachable {
            let current_grid = GridPosition {
                x: target_tile.0,
                y: target_tile.1,
            };
            let is_stationary = current_grid.x == pos.x && current_grid.y == pos.y;

            let actions = crate::systems::action::get_available_actions_at(
                world,
                unit_entity,
                current_grid,
                !is_stationary,
            );

            // 基本スコア
            let mut base_tile_score = 0;
            if let Some(terrain) = map.get_terrain(current_grid.x, current_grid.y) {
                base_tile_score += registry.get_terrain_defense_bonus(terrain) as i32 * 10;
            }

            // 占領価値・拠点接近スコア
            if stats.can_capture {
                let mut min_objective_dist = 99;
                for (p_pos, _p_terrain, p_owner) in &properties {
                    if *p_owner != Some(player_id) {
                        let d = (current_grid.x as i32 - p_pos.x as i32).abs()
                            + (current_grid.y as i32 - p_pos.y as i32).abs();
                        if d < min_objective_dist {
                            min_objective_dist = d;
                        }
                    }
                }
                // 拠点を狙うスコアを大幅に強化
                base_tile_score += (20 - min_objective_dist).max(0) * 400;
            } else {
                // 最も「損害期待値」の高い敵をメインターゲットとして位置取りを決定する
                let mut best_target_dist = 99;
                let mut max_potential = -1.0;

                for (e_pos, e_type, e_cost, e_hp) in &enemy_units {
                    let d = (current_grid.x as i32 - e_pos.x as i32).abs()
                        + (current_grid.y as i32 - e_pos.y as i32).abs();

                    // ダメージ期待値を概算（相性とコストとHPを考慮）
                    let base_dmg = damage_chart
                        .get_base_damage(stats.unit_type, *e_type)
                        .or_else(|| {
                            damage_chart.get_base_damage_secondary(stats.unit_type, *e_type)
                        })
                        .unwrap_or(0);

                    // 価値 = ダメージ期待値 * ユニットコスト
                    // ※HPが低い敵ほど仕留めやすいため評価を少し上げる
                    let potential =
                        base_dmg as f32 * (*e_cost as f32 / 100.0) * (2.0 - *e_hp as f32 / 100.0);

                    if potential > max_potential {
                        max_potential = potential;
                        best_target_dist = d;
                    } else if (potential - max_potential).abs() < 0.1 && d < best_target_dist {
                        // 価値が同じなら近い方を優先
                        best_target_dist = d;
                    }
                }

                // fallback: 敵がいない、または誰も攻撃できない場合は最寄りの敵を目指す
                if max_potential <= 0.0 {
                    for (e_pos, _, _, _) in &enemy_units {
                        let d = (current_grid.x as i32 - e_pos.x as i32).abs()
                            + (current_grid.y as i32 - e_pos.y as i32).abs();
                        if d < best_target_dist {
                            best_target_dist = d;
                        }
                    }
                }

                if stats.min_range > 1 {
                    // 間接攻撃ユニット：最大射程付近を維持したい
                    let target_dist = stats.max_range as i32;
                    let dist_diff = (best_target_dist - target_dist).abs();
                    base_tile_score += (20 - dist_diff).max(0) * 100;

                    // 最小射程未満（隣接など）は攻撃不能になるため強く避ける
                    if best_target_dist < stats.min_range as i32 {
                        base_tile_score -= 2000;
                    }
                } else {
                    // 直接攻撃ユニット：隣接を目指す
                    base_tile_score += (20 - best_target_dist).max(0) * 100;
                }
            }

            // (A) Capture
            if actions.can_capture {
                let score = base_tile_score + 10000;
                if score > best_unit_score {
                    best_unit_score = score;
                    best_unit_choice = Some(AiCommand::Capture {
                        target_pos: current_grid,
                    });
                }
            }

            // (B) Attack
            if actions.can_attack {
                let targets = crate::systems::combat::get_attackable_targets_at(
                    world,
                    unit_entity,
                    current_grid,
                    is_stationary,
                );
                for target_entity in targets {
                    // カミカゼアタック（無謀な攻撃）の回避
                    if crate::ai::pruning::is_suicidal_attack(
                        world,
                        unit_entity,
                        target_entity,
                        &damage_chart,
                    ) {
                        continue;
                    }

                    // ターゲットの詳細を取得してスコアを加点
                    if let (Some(t_stats), Some(t_health), Some(t_pos)) = (
                        world.get::<UnitStats>(target_entity),
                        world.get::<Health>(target_entity),
                        world.get::<GridPosition>(target_entity),
                    ) {
                        // 撃破判定・ダメージ期待値の算出: 攻撃側HP、弾薬、距離、および地形防御ボーナスを考慮
                        let t_terrain = map
                            .get_terrain(t_pos.x, t_pos.y)
                            .unwrap_or(crate::resources::Terrain::Plains);
                        let def_bonus = registry.get_terrain_defense_bonus(t_terrain);
                        let dist = (pos.x as i64 - t_pos.x as i64).unsigned_abs() as u32
                            + (pos.y as i64 - t_pos.y as i64).unsigned_abs() as u32;

                        // ターゲットへのダメージ予測
                        let expected_actual_damage = get_expected_damage(
                            &stats,
                            atk_hp,
                            atk_ammo,
                            t_stats,
                            def_bonus,
                            dist,
                            &registry,
                            &damage_chart,
                            false,
                        );

                        // 期待ダメージが0の場合は攻撃候補から外す（Waitを上回る誤挙動を防止）
                        if expected_actual_damage == 0 {
                            continue;
                        }

                        let mut attack_score = 2000;

                        // 与えるダメージ量に応じた加点 (0 ~ 10000程度)
                        // ダメージ量 * 敵のコスト / 100
                        // 100%時のダメージ(base_dmg)ではなく、現在のHPや弾薬を考慮した期待ダメージ(expected_actual_damage)を使用する
                        let damage_val = (expected_actual_damage * t_stats.cost) / 100;
                        attack_score += damage_val as i32;

                        // 撃破できる場合はボーナス
                        if expected_actual_damage >= t_health.current {
                            attack_score += 5000;
                        }

                        let score = base_tile_score + attack_score;
                        if score > best_unit_score {
                            best_unit_score = score;
                            best_unit_choice = Some(AiCommand::Attack {
                                target_pos: current_grid,
                                target_entity,
                            });
                        }
                    }
                }
            }

            // (C) Wait
            if actions.can_wait {
                let score = base_tile_score;
                if score > best_unit_score {
                    best_unit_score = score;
                    best_unit_choice = Some(AiCommand::Wait {
                        target_pos: current_grid,
                    });
                }
            }
        }

        if let Some(choice) = best_unit_choice
            && best_unit_score > best_overall_score
        {
            best_overall_score = best_unit_score;
            best_overall_choice = Some((unit_entity, choice));
        }
    }

    best_overall_choice
}

pub fn execute_ai_command(world: &mut World, unit_entity: Entity, command: AiCommand) {
    match command {
        AiCommand::Attack {
            target_pos,
            target_entity,
        } => {
            if let Some(mut evs) = world.get_resource_mut::<Events<MoveUnitCommand>>() {
                evs.send(MoveUnitCommand {
                    unit_entity,
                    target_x: target_pos.x,
                    target_y: target_pos.y,
                });
            }
            if let Some(mut evs) = world.get_resource_mut::<Events<AttackUnitCommand>>() {
                evs.send(AttackUnitCommand {
                    attacker_entity: unit_entity,
                    defender_entity: target_entity,
                });
            }
        }
        AiCommand::Capture { target_pos } => {
            if let Some(mut evs) = world.get_resource_mut::<Events<MoveUnitCommand>>() {
                evs.send(MoveUnitCommand {
                    unit_entity,
                    target_x: target_pos.x,
                    target_y: target_pos.y,
                });
            }
            if let Some(mut evs) = world.get_resource_mut::<Events<CapturePropertyCommand>>() {
                evs.send(CapturePropertyCommand { unit_entity });
            }
        }
        AiCommand::Wait { target_pos } => {
            if let Some(mut evs) = world.get_resource_mut::<Events<MoveUnitCommand>>() {
                evs.send(MoveUnitCommand {
                    unit_entity,
                    target_x: target_pos.x,
                    target_y: target_pos.y,
                });
            }
            if let Some(mut evs) = world.get_resource_mut::<Events<WaitUnitCommand>>() {
                evs.send(WaitUnitCommand { unit_entity });
            }
        }
        AiCommand::Merge {
            target_pos,
            target_entity,
        } => {
            if let Some(mut evs) = world.get_resource_mut::<Events<MoveUnitCommand>>() {
                evs.send(MoveUnitCommand {
                    unit_entity,
                    target_x: target_pos.x,
                    target_y: target_pos.y,
                });
            }
            if let Some(mut evs) =
                world.get_resource_mut::<Events<crate::events::MergeUnitCommand>>()
            {
                evs.send(crate::events::MergeUnitCommand {
                    source_entity: unit_entity,
                    target_entity,
                });
            }
        }
        AiCommand::Load {
            target_pos,
            transport_entity,
        } => {
            if let Some(mut evs) = world.get_resource_mut::<Events<MoveUnitCommand>>() {
                evs.send(MoveUnitCommand {
                    unit_entity,
                    target_x: target_pos.x,
                    target_y: target_pos.y,
                });
            }
            if let Some(mut evs) =
                world.get_resource_mut::<Events<crate::events::LoadUnitCommand>>()
            {
                evs.send(crate::events::LoadUnitCommand {
                    unit_entity,
                    transport_entity,
                });
            }
        }
        AiCommand::Supply {
            target_pos,
            target_entity,
        } => {
            if let Some(mut evs) = world.get_resource_mut::<Events<MoveUnitCommand>>() {
                evs.send(MoveUnitCommand {
                    unit_entity,
                    target_x: target_pos.x,
                    target_y: target_pos.y,
                });
            }
            if let Some(mut evs) =
                world.get_resource_mut::<Events<crate::events::SupplyUnitCommand>>()
            {
                evs.send(crate::events::SupplyUnitCommand {
                    supplier_entity: unit_entity,
                    target_entity,
                });
            }
        }
    }
}

/// 一度の呼び出しで、該当勢力のAI行動（生産、または1ユニットの行動）を1ステップ実行し、イベントを発行します。
/// 行動可能ユニットがなくなったらターン終了コマンドを発行します。
/// 何らかの行動を実行した場合は true、ターンが終了した場合は false を返します。
/// AIのメイン実行エントリーポイント。
pub fn execute_ai_turn(world: &mut World, active_player: PlayerId) -> bool {
    // 1. ユニット行動を1つ決定・実行
    // AI思考ループの中で、エンジン側のフラグが更新されるのを待たずに
    // 同一フレーム内の重複思考を避けるために、リソースで「指示済みユニット」を管理します。
    let mut skip_entities = std::collections::HashSet::new();
    if let Some(res) = world.get_resource::<AiActionCooldown>() {
        skip_entities = res.0.clone();
    }

    if let Some((entity, command)) = decide_ai_action(world, active_player, &skip_entities) {
        execute_ai_command(world, entity, command);

        // リソースを更新して、次回の呼び出しでもこのユニットをスキップするようにする
        if let Some(mut res) = world.get_resource_mut::<AiActionCooldown>() {
            res.0.insert(entity);
        } else {
            let mut set = std::collections::HashSet::new();
            set.insert(entity);
            world.insert_resource(AiActionCooldown(set));
        }
        return true;
    }

    // 全ユニットの検討が終わったら冷却リストをクリアする（生産や次ターン移行の準備）
    if let Some(mut res) = world.get_resource_mut::<AiActionCooldown>() {
        res.0.clear();
    }

    // 2. 生産行動
    let prod_commands = super::production::decide_production(world, active_player);
    if let Some(cmd) = prod_commands.into_iter().next() {
        if let Some(mut events) =
            world.get_resource_mut::<Events<crate::events::ProduceUnitCommand>>()
        {
            events.send(cmd);
        }
        return true;
    }

    // 3. 全行動完了 -> ターン終了
    if let Some(mut end_events) =
        world.get_resource_mut::<Events<crate::events::NextPhaseCommand>>()
    {
        end_events.send(crate::events::NextPhaseCommand);
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::{Faction, Health, PlayerId, Property, UnitStats};
    use crate::resources::{DamageChart, UnitType};

    #[test]
    fn test_decide_ai_action_no_units() {
        let mut world = World::new();
        let skips = std::collections::HashSet::new();
        assert!(decide_ai_action(&mut world, PlayerId(1), &skips).is_none());
    }

    #[test]
    fn test_decide_ai_action_wait() {
        let mut world = World::new();
        world.insert_resource(DamageChart::new());
        world.insert_resource(Map {
            width: 5,
            height: 5,
            tiles: vec![Terrain::Plains; 25],
            topology: crate::resources::GridTopology::Square,
        });
        crate::resources::master_data::MasterDataRegistry::load()
            .map(|m| world.insert_resource(m))
            .unwrap();

        world.spawn((
            PlayerId(1),
            Faction(PlayerId(1)),
            HasMoved(false),
            ActionCompleted(false),
            GridPosition { x: 0, y: 0 },
            UnitStats {
                unit_type: UnitType::Tank,
                cost: 1000,
                max_movement: 3,
                movement_type: crate::resources::MovementType::Tank,
                max_fuel: 99,
                ..UnitStats::mock()
            },
            Health {
                current: 100,
                max: 100,
            },
            crate::components::Fuel {
                current: 99,
                max: 99,
            },
        ));

        // Since there is no enemy to attack and no property to capture, it should return Wait.
        let skips = std::collections::HashSet::new();
        let action = decide_ai_action(&mut world, PlayerId(1), &skips);
        assert!(action.is_some());
        if let Some((_, AiCommand::Wait { .. })) = action {
        } else {
            panic!("Expected Wait command, got {:?}", action);
        }
    }

    #[test]
    fn test_decide_ai_action_attack() {
        let mut world = World::new();
        let mut dc = DamageChart::new();
        dc.insert_damage(UnitType::Tank, UnitType::Infantry, 90);
        dc.insert_damage(UnitType::Infantry, UnitType::Tank, 1); // Ensure not suicidal
        world.insert_resource(dc);
        world.insert_resource(Map {
            width: 10,
            height: 10,
            tiles: vec![Terrain::Plains; 100],
            topology: crate::resources::GridTopology::Square,
        });
        crate::resources::master_data::MasterDataRegistry::load()
            .map(|m| world.insert_resource(m))
            .unwrap();

        let p1 = PlayerId(1);
        let p2 = PlayerId(2);

        let attacker = world
            .spawn((
                p1,
                Faction(p1),
                HasMoved(false),
                ActionCompleted(false),
                GridPosition { x: 1, y: 1 },
                UnitStats {
                    ammo1_cost: 0,
                    ammo2_cost: 0,
                    unit_type: UnitType::Tank,
                    cost: 7000,
                    min_range: 1,
                    max_range: 1,
                    max_ammo1: 10,
                    max_ammo2: 10,
                    movement_type: crate::resources::MovementType::Tank,
                    max_movement: 6,
                    max_fuel: 99,
                    daily_fuel_consumption: 0,
                    can_capture: false,
                    can_supply: false,
                    max_cargo: 0,
                    loadable_unit_types: vec![],
                },
                Health {
                    current: 100,
                    max: 100,
                },
                crate::components::Ammo {
                    ammo1: 10,
                    max_ammo1: 10,
                    ammo2: 10,
                    max_ammo2: 10,
                },
                crate::components::Fuel {
                    current: 99,
                    max: 99,
                },
            ))
            .id();

        world.spawn((
            p2,
            Faction(p2),
            GridPosition { x: 1, y: 2 }, // adjacent
            UnitStats {
                unit_type: UnitType::Infantry,
                cost: 1000,
                ..UnitStats::mock()
            },
            Health {
                current: 100,
                max: 100,
            },
        ));

        let skips = std::collections::HashSet::new();
        let action = decide_ai_action(&mut world, p1, &skips);
        assert!(action.is_some());
        if let Some((entity, AiCommand::Attack { target_entity, .. })) = action {
            assert_eq!(entity, attacker);
            // target_entity is the spawned defender
            let defender_faction = world.get::<Faction>(target_entity).unwrap();
            assert_eq!(defender_faction.0, p2);
        } else {
            panic!("Expected Attack command, got {:?}", action);
        }
    }

    #[test]
    fn test_decide_ai_action_capture() {
        let mut world = World::new();
        world.insert_resource(DamageChart::new());
        world.insert_resource(Map {
            width: 10,
            height: 10,
            tiles: vec![Terrain::Plains; 100],
            topology: crate::resources::GridTopology::Square,
        });
        crate::resources::master_data::MasterDataRegistry::load()
            .map(|m| world.insert_resource(m))
            .unwrap();

        let p1 = PlayerId(1);

        let unit = world
            .spawn((
                p1,
                Faction(p1),
                HasMoved(false),
                ActionCompleted(false),
                GridPosition { x: 1, y: 1 },
                UnitStats {
                    unit_type: UnitType::Infantry,
                    cost: 1000,
                    can_capture: true,
                    max_movement: 3,
                    movement_type: crate::resources::MovementType::Infantry,
                    max_fuel: 99,
                    ..UnitStats::mock()
                },
                Health {
                    current: 100,
                    max: 100,
                },
                crate::components::Fuel {
                    current: 99,
                    max: 99,
                },
            ))
            .id();

        // Neutral property on the same tile
        world.spawn((
            GridPosition { x: 1, y: 1 },
            Property::new(Terrain::City, None, 200),
        ));

        let p1 = PlayerId(1);
        let skips = std::collections::HashSet::new();
        let action = decide_ai_action(&mut world, p1, &skips);
        assert!(action.is_some());
        if let Some((entity, AiCommand::Capture { .. })) = action {
            assert_eq!(entity, unit);
        } else {
            panic!("Expected Capture command, got {:?}", action);
        }
    }

    #[test]
    fn test_decide_ai_action_indirect_range() {
        let mut world = World::new();
        let mut dc = DamageChart::new();
        // Artillery vs Tank
        dc.insert_damage(UnitType::Artillery, UnitType::Tank, 50);
        world.insert_resource(dc);
        world.insert_resource(Map {
            width: 10,
            height: 10,
            tiles: vec![Terrain::Plains; 100],
            topology: crate::resources::GridTopology::Square,
        });
        crate::resources::master_data::MasterDataRegistry::load()
            .map(|m| world.insert_resource(m))
            .unwrap();

        let p1 = PlayerId(1);
        let p2 = PlayerId(2);

        // Artillery at (0,0), can move 5 tiles.
        // Max range 3, Min range 2.
        world.spawn((
            p1,
            Faction(p1),
            HasMoved(false),
            ActionCompleted(false),
            GridPosition { x: 0, y: 0 },
            UnitStats {
                unit_type: UnitType::Artillery,
                cost: 6000,
                max_movement: 5,
                movement_type: crate::resources::MovementType::Artillery,
                min_range: 2,
                max_range: 3,
                max_fuel: 99,
                ..UnitStats::mock()
            },
            Health {
                current: 100,
                max: 100,
            },
            crate::components::Fuel {
                current: 99,
                max: 99,
            },
            crate::components::Ammo {
                ammo1: 10,
                max_ammo1: 10,
                ammo2: 0,
                max_ammo2: 0,
            },
        ));

        // Tank at (7,0). Distance is 7.
        // Artillery can move to (4,0) [dist 3], (5,0) [dist 2].
        // It should prefer (4,0) because it's max_range (3).
        world.spawn((
            p2,
            Faction(p2),
            GridPosition { x: 7, y: 0 },
            UnitStats {
                unit_type: UnitType::Tank,
                cost: 7000,
                ..UnitStats::mock()
            },
            Health {
                current: 100,
                max: 100,
            },
        ));

        let skips = std::collections::HashSet::new();
        let action = decide_ai_action(&mut world, p1, &skips);

        assert!(action.is_some());
        if let Some((_, AiCommand::Wait { target_pos, .. })) = action {
            // Should be at distance 3 from (7,0) -> x=4, y=0
            assert_eq!(target_pos.x, 4);
            assert_eq!(target_pos.y, 0);
        } else {
            panic!("Expected Wait command at distance 3, got {:?}", action);
        }
    }

    #[test]
    fn test_decide_ai_action_indirect_escape() {
        let mut world = World::new();
        world.insert_resource(DamageChart::new());
        world.insert_resource(Map {
            width: 10,
            height: 10,
            tiles: vec![Terrain::Plains; 100],
            topology: crate::resources::GridTopology::Square,
        });
        crate::resources::master_data::MasterDataRegistry::load()
            .map(|m| world.insert_resource(m))
            .unwrap();

        let p1 = PlayerId(1);
        let p2 = PlayerId(2);

        // Artillery at (4,0), adjacent to Tank at (5,0).
        // Cannot attack from (4,0) because min_range is 2.
        // Should move away to at least distance 2.
        world.spawn((
            p1,
            Faction(p1),
            HasMoved(false),
            ActionCompleted(false),
            GridPosition { x: 4, y: 0 },
            UnitStats {
                unit_type: UnitType::Artillery,
                cost: 6000,
                max_movement: 5,
                movement_type: crate::resources::MovementType::Artillery,
                min_range: 2,
                max_range: 3,
                max_fuel: 99,
                ..UnitStats::mock()
            },
            Health {
                current: 100,
                max: 100,
            },
            crate::components::Fuel {
                current: 99,
                max: 99,
            },
            crate::components::Ammo {
                ammo1: 10,
                max_ammo1: 10,
                ammo2: 0,
                max_ammo2: 0,
            },
        ));

        world.spawn((
            p2,
            Faction(p2),
            GridPosition { x: 5, y: 0 },
            UnitStats {
                unit_type: UnitType::Tank,
                cost: 7000,
                ..UnitStats::mock()
            },
            Health {
                current: 100,
                max: 100,
            },
        ));

        let skips = std::collections::HashSet::new();
        let action = decide_ai_action(&mut world, p1, &skips);

        let (_, cmd) = action.expect("some action must be chosen");
        let target_pos = match cmd {
            AiCommand::Wait { target_pos } => target_pos,
            other => panic!("Expected Wait command, got {:?}", other),
        };

        // Distance to (5,0) should be >= 2. (4,0) is dist 1.
        let dist = (target_pos.x as i32 - 5).abs() + (target_pos.y as i32).abs();
        assert!(
            dist >= 2,
            "Artillery should move away from adjacency, got pos {:?} (dist {})",
            target_pos,
            dist
        );
    }

    #[test]
    fn test_decide_ai_action_avoid_kamikaze() {
        let mut world = World::new();
        let mut dc = DamageChart::new();
        // Infantry vs Tank: 1% damage
        dc.insert_damage(UnitType::Infantry, UnitType::Tank, 1);
        // Tank vs Infantry: 90% damage
        dc.insert_damage(UnitType::Tank, UnitType::Infantry, 90);
        world.insert_resource(dc);
        world.insert_resource(Map {
            width: 10,
            height: 10,
            tiles: vec![Terrain::Plains; 100],
            topology: crate::resources::GridTopology::Square,
        });
        crate::resources::master_data::MasterDataRegistry::load()
            .map(|m| world.insert_resource(m))
            .unwrap();

        let p1 = PlayerId(1);
        let p2 = PlayerId(2);

        // Infantry (P1) at (1,1)
        world.spawn((
            p1,
            Faction(p1),
            HasMoved(false),
            ActionCompleted(false),
            GridPosition { x: 1, y: 1 },
            UnitStats {
                unit_type: UnitType::Infantry,
                cost: 1000,
                min_range: 1,
                max_range: 1,
                max_movement: 3,
                movement_type: crate::resources::MovementType::Infantry,
                max_fuel: 99,
                ..UnitStats::mock()
            },
            Health {
                current: 100,
                max: 100,
            },
            crate::components::Fuel {
                current: 99,
                max: 99,
            },
            crate::components::Ammo {
                ammo1: 10,
                max_ammo1: 10,
                ammo2: 10,
                max_ammo2: 10,
            },
        ));

        // Tank (P2) at (1,2)
        world.spawn((
            p2,
            Faction(p2),
            GridPosition { x: 1, y: 2 },
            UnitStats {
                unit_type: UnitType::Tank,
                cost: 7000,
                ..UnitStats::mock()
            },
            Health {
                current: 100,
                max: 100,
            },
        ));

        let skips = std::collections::HashSet::new();
        let action = decide_ai_action(&mut world, p1, &skips);

        assert!(action.is_some());
        if let Some((_, AiCommand::Attack { .. })) = action {
            panic!("AI should not perform a suicidal attack (Infantry vs Tank)");
        }
    }
}
