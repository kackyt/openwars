//! V1固有の行動選択と手番進行。
#![allow(clippy::collapsible_if)]
#![allow(clippy::unnecessary_min_or_max)]
#![allow(clippy::unnecessary_map_or)]
use crate::ai::command::AiCommand;
use crate::ai::context::tactical::*;
use crate::ai::runtime::{
    issue::execute_ai_command, production_queue::AiProductionCooldown, state::AiActionCooldown,
};
use crate::ai::tactics::candidates::air_move_preserves_return_fuel;
use crate::collections::{HashMap, HashSet};
use crate::components::{
    ActionCompleted, Faction, GridPosition, HasMoved, Health, PlayerId, Property, UnitStats,
};
use crate::resources::master_data::MasterDataRegistry;
use crate::resources::{Map, Terrain};
use crate::systems::combat::get_expected_damage;
use crate::systems::movement::{OccupantInfo, calculate_reachable_tiles};
use bevy_ecs::prelude::*;

/// AIの思考エンジン。未行動のユニットに対して最も評価の高いコマンドを決定します。
pub(crate) fn decide_ai_action(
    world: &mut World,
    player_id: PlayerId,
    skip_entities: &crate::collections::HashSet<Entity>,
) -> Option<(Entity, AiCommand)> {
    // 1. 行動可能なユニットを収集
    let mut movable_units = Vec::new();
    let mut unit_positions = HashMap::default();
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

    let mut turn_cache = crate::ai::turn_distance::AiTurnCache::default();
    let unit_costs: HashMap<Entity, u32> = {
        let mut query = world.query::<(Entity, &UnitStats)>();
        query
            .iter(world)
            .map(|(entity, stats)| (entity, stats.cost))
            .collect()
    };

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

        // 戦闘不能判定（HPが低い、または弾薬切れ）
        let is_combat_ineffective = atk_hp < 70 || (stats.max_ammo1 > 0 && atk_ammo.0 == 0);

        let map = world.resource::<Map>().clone();
        let registry = world.resource::<MasterDataRegistry>().clone();
        let tactical_snapshot = AiTacticalSnapshot::from_world(world, &registry, map.topology);

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
        let enemy_units: Vec<(
            GridPosition,
            crate::resources::UnitType,
            u32,
            u32,
            u32,
            u32,
            u32,
        )> = {
            let mut q = world.query::<(
                &GridPosition,
                &Faction,
                &UnitStats,
                &Health,
                Option<&crate::components::CargoCapacity>,
                Option<&crate::components::Transporting>,
            )>();
            q.iter(world)
                // 輸送中の兵は盤外座標の実体なので、独立した追跡対象にはしない。
                .filter(|(_, f, _, h, _, transporting)| {
                    f.0 != player_id && h.current > 0 && transporting.is_none()
                })
                .map(|(p, faction, s, h, cargo, _)| {
                    (
                        *p,
                        s.unit_type,
                        strategic_target_value(
                            s,
                            *p,
                            faction.0,
                            cargo,
                            &unit_costs,
                            &properties,
                            &registry,
                        ),
                        h.current,
                        s.min_range,
                        s.max_range,
                        s.max_movement,
                    )
                })
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
            if !air_move_preserves_return_fuel(
                &map,
                &properties,
                player_id,
                &stats,
                pos,
                current_grid,
                fuel,
            ) {
                continue;
            }
            let is_stationary = current_grid.x == pos.x && current_grid.y == pos.y;

            let actions = tactical_snapshot.action_targets_at(
                unit_entity,
                &stats,
                current_grid,
                !is_stationary,
            );

            // 基本スコア
            let mut base_tile_score = 0;
            if let Some(terrain) = map.get_terrain(current_grid.x, current_grid.y) {
                base_tile_score += registry.get_terrain_defense_bonus(terrain) as i32 * 10;
            }

            // 戦闘不能時の撤退先探索
            if is_combat_ineffective {
                let mut min_recovery_dist: i32 = 999;
                for (p_pos, p_terrain, p_owner) in &properties {
                    if *p_owner == Some(player_id)
                        && registry.can_repair_on_terrain(stats.unit_type, *p_terrain)
                    {
                        let d = (current_grid.x as i32 - p_pos.x as i32).abs()
                            + (current_grid.y as i32 - p_pos.y as i32).abs();
                        if d < min_recovery_dist {
                            min_recovery_dist = d;
                        }
                    }
                }
                // 拠点に近づくほど高スコア
                base_tile_score += (20 - min_recovery_dist).max(0) * 300;
            }

            // 7.3 タクシー帰りロジック: 空の輸送車は生産拠点へ引き返す
            let is_empty_transport = stats.max_cargo > 0
                && world
                    .get::<crate::components::CargoCapacity>(unit_entity)
                    .is_some_and(|c| c.loaded.is_empty());

            if is_empty_transport {
                let mut min_base_dist: i32 = 999;
                for (p_pos, p_terrain, p_owner) in &properties {
                    if *p_owner == Some(player_id)
                        && registry.is_production_facility(p_terrain.as_str())
                    {
                        let d = (current_grid.x as i32 - p_pos.x as i32).abs()
                            + (current_grid.y as i32 - p_pos.y as i32).abs();
                        if d < min_base_dist {
                            min_base_dist = d;
                        }
                    }
                }
                // 拠点に近づくほど高スコア（磁力）
                base_tile_score += (20 - min_base_dist).max(0) * 500;
            }

            // 歩兵の待機移動ロジック: やることがない歩兵は海岸へ向かう
            let is_infantry = stats.unit_type == crate::resources::UnitType::Infantry
                || stats.unit_type == crate::resources::UnitType::Mech;
            if is_infantry
                && !is_combat_ineffective
                && is_unit_stranded(world, &pos, player_id, &properties, &enemy_units)
            {
                let mut min_coast_dist: i32 = 999;

                // 効率化: 全マス走査を避け、現在位置周辺の限定された範囲で海岸を探す
                let check_range = 10;
                let min_x = current_grid.x.saturating_sub(check_range);
                let max_x = (current_grid.x + check_range).min(map.width - 1);
                let min_y = current_grid.y.saturating_sub(check_range);
                let max_y = (current_grid.y + check_range).min(map.height - 1);

                for cy in min_y..=max_y {
                    for cx in min_x..=max_x {
                        if map.get_terrain(cx, cy) == Some(crate::resources::Terrain::Sea) {
                            let d = (current_grid.x as i32 - cx as i32).abs()
                                + (current_grid.y as i32 - cy as i32).abs();
                            if d < min_coast_dist {
                                min_coast_dist = d;
                            }
                        }
                    }
                }

                // 海岸に近いほど加点（距離1を最適とする）
                if min_coast_dist < 99 && min_coast_dist > 0 {
                    base_tile_score += (20 - min_coast_dist).max(0) * 100;
                }
            }

            // 占領価値・拠点接近スコア
            let mut effective_can_capture = stats.can_capture;
            if !effective_can_capture
                && let Some(cargo) = world.get::<crate::components::CargoCapacity>(unit_entity)
            {
                for &cargo_ent in &cargo.loaded {
                    if let Some(c_stats) = world.get::<UnitStats>(cargo_ent)
                        && c_stats.can_capture
                    {
                        effective_can_capture = true;
                        break;
                    }
                }
            }

            if effective_can_capture {
                let mut min_objective_dist: i32 = 999;
                for (p_pos, _p_terrain, p_owner) in &properties {
                    if *p_owner != Some(player_id) {
                        let mut d = (current_grid.x as i32 - p_pos.x as i32).abs()
                            + (current_grid.y as i32 - p_pos.y as i32).abs();
                        if stats.movement_type == crate::resources::MovementType::Ship {
                            let dist_map =
                                crate::ai::turn_distance::calculate_all_turn_distances_cached(
                                    &map,
                                    &registry,
                                    &unit_positions,
                                    (p_pos.x, p_pos.y),
                                    stats.movement_type,
                                    stats.max_movement,
                                    1, // 拠点占領/輸送は隣接(距離1)の海が必要
                                    player_id,
                                    &mut turn_cache,
                                );
                            let t_dist = dist_map.get(&current_grid).copied().unwrap_or(
                                crate::ai::turn_distance::TurnDistance {
                                    turns: u32::MAX,
                                    used_mp: u32::MAX,
                                },
                            );
                            if t_dist.turns != u32::MAX {
                                d = (t_dist.turns * stats.max_movement) as i32;
                            } else {
                                d = 999;
                            }
                        }
                        if d < min_objective_dist {
                            min_objective_dist = d;
                        }
                    }
                }
                // 拠点を狙うスコアを大幅に強化
                base_tile_score += (20 - min_objective_dist).max(0) * 400;
            } else {
                // 最も「損害期待値」の高い敵をメインターゲットとして位置取りを決定する
                let mut best_target_dist: i32 = 999;
                let mut max_potential = -1.0;

                for (e_pos, e_type, e_cost, e_hp, _, _, _) in &enemy_units {
                    let mut effective_dist = (current_grid.x as i32 - e_pos.x as i32).abs()
                        + (current_grid.y as i32 - e_pos.y as i32).abs();

                    // 海軍ユニットが陸上の敵を追跡する場合の補正（または単純なターン距離）
                    if stats.movement_type == crate::resources::MovementType::Ship {
                        let dist_map =
                            crate::ai::turn_distance::calculate_all_turn_distances_cached(
                                &map,
                                &registry,
                                &unit_positions,
                                (e_pos.x, e_pos.y),
                                stats.movement_type,
                                stats.max_movement,
                                stats.max_range, // 敵が射程に入る海マスへのターン距離
                                player_id,
                                &mut turn_cache,
                            );
                        let t_dist = dist_map.get(&current_grid).copied().unwrap_or(
                            crate::ai::turn_distance::TurnDistance {
                                turns: u32::MAX,
                                used_mp: u32::MAX,
                            },
                        );
                        if t_dist.turns != u32::MAX {
                            effective_dist = (t_dist.turns * stats.max_movement) as i32;
                        } else {
                            effective_dist = 999;
                        }
                    }

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
                        best_target_dist = effective_dist;
                    } else if (potential - max_potential).abs() < 0.1
                        && effective_dist < best_target_dist
                    {
                        // 価値が同じなら近い方を優先
                        best_target_dist = effective_dist;
                    }
                }

                // fallback: 敵がいない、または誰も攻撃できない場合は最寄りの敵、または拠点を指す
                if max_potential <= 0.0 {
                    let mut min_dist: i32 = 999;
                    // 1. 敵ユニットを探す
                    for (e_pos, _, _, _, _, _, _) in &enemy_units {
                        let mut d = (current_grid.x as i32 - e_pos.x as i32).abs()
                            + (current_grid.y as i32 - e_pos.y as i32).abs();

                        if stats.movement_type == crate::resources::MovementType::Ship {
                            let dist_map =
                                crate::ai::turn_distance::calculate_all_turn_distances_cached(
                                    &map,
                                    &registry,
                                    &unit_positions,
                                    (e_pos.x, e_pos.y),
                                    stats.movement_type,
                                    stats.max_movement,
                                    stats.max_range, // 敵が射程に入る海マスへのターン距離
                                    player_id,
                                    &mut turn_cache,
                                );
                            let t_dist = dist_map.get(&current_grid).copied().unwrap_or(
                                crate::ai::turn_distance::TurnDistance {
                                    turns: u32::MAX,
                                    used_mp: u32::MAX,
                                },
                            );
                            if t_dist.turns != u32::MAX {
                                d = (t_dist.turns * stats.max_movement) as i32;
                            } else {
                                d = 999;
                            }
                        }
                        if d < min_dist {
                            min_dist = d;
                        }
                    }
                    // 2. 敵がいない場合は、未占領または敵の拠点をターゲットにする
                    if enemy_units.is_empty() {
                        for (p_pos, p_terrain, p_owner) in &properties {
                            if *p_owner != Some(player_id) {
                                let mut d = (current_grid.x as i32 - p_pos.x as i32).abs()
                                    + (current_grid.y as i32 - p_pos.y as i32).abs();
                                if stats.movement_type == crate::resources::MovementType::Ship {
                                    let dist_map = crate::ai::turn_distance::calculate_all_turn_distances_cached(
                                         &map,
                                         &registry,
                                         &unit_positions,
                                         (p_pos.x, p_pos.y),
                                         stats.movement_type,
                                         stats.max_movement,
                                         1, // 拠点に隣接する海マスへのターン距離
                                         player_id,
                                         &mut turn_cache,
                                     );
                                    let t_dist = dist_map.get(&current_grid).copied().unwrap_or(
                                        crate::ai::turn_distance::TurnDistance {
                                            turns: u32::MAX,
                                            used_mp: u32::MAX,
                                        },
                                    );
                                    if t_dist.turns != u32::MAX {
                                        d = (t_dist.turns * stats.max_movement) as i32;
                                    } else {
                                        d = 999;
                                    }
                                }
                                if d < min_dist {
                                    min_dist = d;
                                }
                            } else if is_combat_ineffective
                                && registry.can_repair_on_terrain(stats.unit_type, *p_terrain)
                            {
                                // 自身が修理が必要な場合のみ、自分の拠点もターゲットに含める
                                let mut d = (current_grid.x as i32 - p_pos.x as i32).abs()
                                    + (current_grid.y as i32 - p_pos.y as i32).abs();
                                if stats.movement_type == crate::resources::MovementType::Ship {
                                    let dist_map = crate::ai::turn_distance::calculate_all_turn_distances_cached(
                                         &map,
                                         &registry,
                                         &unit_positions,
                                         (p_pos.x, p_pos.y),
                                         stats.movement_type,
                                         stats.max_movement,
                                         1, // 修理拠点は隣接する海が必要
                                         player_id,
                                         &mut turn_cache,
                                     );
                                    let t_dist = dist_map.get(&current_grid).copied().unwrap_or(
                                        crate::ai::turn_distance::TurnDistance {
                                            turns: u32::MAX,
                                            used_mp: u32::MAX,
                                        },
                                    );
                                    if t_dist.turns != u32::MAX {
                                        d = (t_dist.turns * stats.max_movement) as i32;
                                    } else {
                                        d = 999;
                                    }
                                }
                                if d < min_dist {
                                    min_dist = d;
                                }
                            }
                        }
                    }
                    best_target_dist = min_dist;
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
            if !actions.attackable_targets.is_empty() {
                for target_entity in actions.attackable_targets.iter().copied() {
                    // カミカゼアタック（無謀な攻撃）の回避
                    if crate::ai::pruning::is_suicidal_attack_at(
                        world,
                        unit_entity,
                        target_entity,
                        current_grid,
                        &damage_chart,
                    ) {
                        continue;
                    }

                    // ターゲットの詳細を取得してスコアを加点
                    if let (Some(t_stats), Some(t_health), Some(t_pos), Some(t_faction)) = (
                        world.get::<UnitStats>(target_entity),
                        world.get::<Health>(target_entity),
                        world.get::<GridPosition>(target_entity),
                        world.get::<Faction>(target_entity),
                    ) {
                        // 撃破判定・ダメージ期待値の算出: 攻撃側HP、弾薬、距離、および地形防御ボーナスを考慮
                        let t_terrain = map
                            .get_terrain(t_pos.x, t_pos.y)
                            .unwrap_or(crate::resources::Terrain::Plains);
                        let def_bonus = registry.get_terrain_defense_bonus(t_terrain);
                        let dist = map.distance(current_grid.x, current_grid.y, t_pos.x, t_pos.y);

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
                        // ダメージ量 * 敵本体・搭載兵・占領阻止の戦略価値 / 100
                        // 100%時のダメージ(base_dmg)ではなく、現在のHPや弾薬を考慮した期待ダメージ(expected_actual_damage)を使用する
                        let target_value = strategic_target_value(
                            t_stats,
                            *t_pos,
                            t_faction.0,
                            world.get::<crate::components::CargoCapacity>(target_entity),
                            &unit_costs,
                            &properties,
                            &registry,
                        );
                        let damage_val = expected_actual_damage.saturating_mul(target_value) / 100;
                        attack_score += damage_val as i32;

                        // 戦闘不能時は攻撃を躊躇させる（撃破できない限り）
                        if is_combat_ineffective && expected_actual_damage < t_health.current {
                            attack_score -= 3000;
                        }

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
                let mut score = base_tile_score;

                // 拠点での待機評価
                let mut is_on_recovery_property = false;
                for (p_pos, p_terrain, p_owner) in &properties {
                    if p_pos.x == current_grid.x
                        && p_pos.y == current_grid.y
                        && *p_owner == Some(player_id)
                        && registry.can_repair_on_terrain(stats.unit_type, *p_terrain)
                    {
                        is_on_recovery_property = true;
                        break;
                    }
                }

                if is_on_recovery_property {
                    if is_combat_ineffective {
                        score += 8000; // 戦闘不能なら最優先
                    } else if atk_hp < 100 || atk_ammo.0 < stats.max_ammo1 {
                        score += 1000; // 少しでも消耗していれば拠点に留まる価値あり
                    }
                } else if is_combat_ineffective {
                    // 拠点以外の場所での待機は避ける
                    score -= 5000;
                }

                if score > best_unit_score {
                    best_unit_score = score;
                    best_unit_choice = Some(AiCommand::Wait {
                        target_pos: current_grid,
                    });
                }
            }

            // (F) Merge
            if !actions.mergeable_targets.is_empty() {
                for target_entity in actions.mergeable_targets.iter().copied() {
                    let mut merge_score = 3000;
                    if let (Some(t_health), Some(_t_stats)) = (
                        world.get::<Health>(target_entity),
                        world.get::<UnitStats>(target_entity),
                    ) {
                        if crate::ai::pruning::is_overflow_merge_without_refund(atk_hp, *t_health) {
                            continue;
                        }

                        let total_hp = atk_hp + t_health.current;
                        // 自身または相手のHPが低い場合、合流の価値を高める
                        if is_combat_ineffective || t_health.current < 40 {
                            merge_score += 4000;
                        }
                        // 合流後のHPが無駄にならないなら加点
                        if total_hp <= t_health.max {
                            merge_score += 1000;
                        }

                        let score = base_tile_score + merge_score;
                        if score > best_unit_score {
                            best_unit_score = score;
                            best_unit_choice = Some(AiCommand::Merge {
                                target_pos: current_grid,
                                target_entity,
                            });
                        }
                    }
                }
            }
        }

        #[allow(clippy::collapsible_if)]
        if let Some(choice) = best_unit_choice {
            if best_unit_score > best_overall_score {
                best_overall_score = best_unit_score;
                best_overall_choice = Some((unit_entity, choice));
            }
        }
    }

    best_overall_choice
}

/// 従来型 AI (V1) のメイン実行ループ
pub(crate) fn execute_ai_turn_v1(world: &mut World, active_player: PlayerId) -> Option<String> {
    // 1. ユニット行動を1つ決定・実行
    // AI思考ループの中で、エンジン側のフラグが更新されるのを待たずに
    // 同一フレーム内の重複思考を避けるために、リソースで「指示済みユニット」を管理します。
    let mut skip_entities = crate::collections::HashSet::default();
    if let Some(res) = world.get_resource::<AiActionCooldown>() {
        skip_entities = res.0.clone();
    }

    // 1. ミッションの状態更新とクリーンアップ
    if let Some(mut manager) =
        world.remove_resource::<crate::ai::missions::TransportMissionManager>()
    {
        let mut i = 0;
        while i < manager.missions.len() {
            let mut mission = manager.missions[i];
            let should_remove = crate::ai::missions::update_mission_phase(world, &mut mission);
            if should_remove {
                manager.missions.remove(i);
            } else {
                manager.missions[i] = mission;
                i += 1;
            }
        }
        world.insert_resource(manager);
    }

    // クリーンアップ後の状態を基に、新規ミッションを割り当てる
    crate::ai::planner::assign_transport_missions(world, active_player);

    // ミッションに関与している全Entity（輸送機と歩兵）を収集し、通常の意思決定から完全に除外する
    let mut mission_entities = crate::collections::HashSet::default();
    if let Some(manager) = world.get_resource::<crate::ai::missions::TransportMissionManager>() {
        for m in &manager.missions {
            if world
                .get::<Faction>(m.transport_entity)
                .is_some_and(|f| f.0 == active_player)
            {
                mission_entities.insert(m.transport_entity);
                // Return フェーズでは歩兵はすでに島に展開済みなので、
                // 通常のAI意思決定（占領など）に参加させる
                if m.phase != crate::ai::missions::TransportPhase::Return {
                    mission_entities.insert(m.cargo_entity);
                }
            }
        }
    }

    let mission_cmd_and_entity = if let Some(manager) =
        world.get_resource::<crate::ai::missions::TransportMissionManager>()
    {
        let mut missions = manager.missions.clone();
        // Pickupを優先することで、同じ輸送船に複数のミッションがある場合に先に乗せる
        missions.sort_by_key(|m| match m.phase {
            crate::ai::missions::TransportPhase::Pickup => 0,
            crate::ai::missions::TransportPhase::Drop => 1,
            crate::ai::missions::TransportPhase::Transit => 2,
            crate::ai::missions::TransportPhase::Return => 3,
        });
        missions.into_iter().find_map(|m| {
            if world
                .get::<Faction>(m.transport_entity)
                .is_some_and(|f| f.0 == active_player)
            {
                let cmds = crate::ai::missions::execute_mission_step(world, &m);
                cmds.into_iter()
                    .find(|(entity, _cmd)| !skip_entities.contains(entity))
            } else {
                None
            }
        })
    } else {
        None
    };

    if let Some((entity, cmd)) = mission_cmd_and_entity {
        let cmd_str = format!("{:?}", cmd);
        execute_ai_command(world, entity, cmd);
        if let Some(mut res) = world.get_resource_mut::<AiActionCooldown>() {
            res.0.insert(entity);
        } else {
            let mut set = crate::collections::HashSet::default();
            set.insert(entity);
            world.insert_resource(AiActionCooldown(set));
        }
        return Some(cmd_str);
    }

    // 通常の意思決定を行う際には、ミッション中ユニット（mission_entities）も skip_entities に追加して除外する
    let mut decide_skip_entities = skip_entities.clone();
    decide_skip_entities.extend(mission_entities);

    if let Some((entity, command)) = decide_ai_action(world, active_player, &decide_skip_entities) {
        let cmd_str = format!("{:?}", command);
        execute_ai_command(world, entity, command);

        // リソースを更新して、次回の呼び出しでもこのユニットをスキップするようにする
        if let Some(mut res) = world.get_resource_mut::<AiActionCooldown>() {
            res.0.insert(entity);
        } else {
            let mut set = crate::collections::HashSet::default();
            set.insert(entity);
            world.insert_resource(AiActionCooldown(set));
        }
        return Some(cmd_str);
    }

    // 2. 生産行動
    let prod_commands = super::production::decide_production(world, active_player);

    let cooldown_set = if let Some(res) = world.get_resource::<AiProductionCooldown>() {
        res.0.clone()
    } else {
        HashSet::default()
    };

    // 診断情報を取得（前回のエラーを確認）
    let (last_error, last_event_str) =
        if let Some(diag) = world.get_resource::<crate::resources::ProductionDiagnostic>() {
            (diag.last_error.clone(), diag.last_event.clone())
        } else {
            (None, None)
        };

    for cmd in prod_commands {
        // 冷却中（今ターン既に試行済み）の座標はスキップ
        if cooldown_set.contains(&(cmd.target_x, cmd.target_y)) {
            continue;
        }

        let cmd_str = format!("{:?}", cmd);

        // 直前のエラーがこのコマンドに関連しているかチェック
        if last_error.is_some() && last_event_str.as_deref() == Some(&cmd_str) {
            // 前回と同じコマンドでエラーが発生している場合はスキップ
            // 座標を冷却リストに入れて再試行を防ぐ
            if let Some(mut res) = world.get_resource_mut::<AiProductionCooldown>() {
                res.0.insert((cmd.target_x, cmd.target_y));
            }
            continue;
        }

        // コマンドを発行し、冷却リストに追加
        let mut sent = false;
        {
            if let Some(mut res) = world.get_resource_mut::<AiProductionCooldown>() {
                res.0.insert((cmd.target_x, cmd.target_y));
            } else {
                let mut set = HashSet::default();
                set.insert((cmd.target_x, cmd.target_y));
                world.insert_resource(AiProductionCooldown(set));
            }
        }

        if let Some(mut events) =
            world.get_resource_mut::<Events<crate::events::ProduceUnitCommand>>()
        {
            events.send(cmd);
            sent = true;
        }

        if sent {
            return Some(cmd_str);
        }
    }

    // 3. 全行動完了 -> ターン終了
    if let Some(mut end_events) =
        world.get_resource_mut::<Events<crate::events::NextPhaseCommand>>()
    {
        end_events.send(crate::events::NextPhaseCommand);
    }
    None
}
