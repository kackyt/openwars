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

#[derive(Resource, Default)]
pub struct AiProductionCooldown(pub HashSet<(usize, usize)>);

/// ターン開始時にAIの冷却リストをクリアするシステム。
pub fn clear_ai_cooldowns_system(
    mut events: EventReader<crate::events::GamePhaseChangedEvent>,
    action_cooldown: Option<ResMut<AiActionCooldown>>,
    prod_cooldown: Option<ResMut<AiProductionCooldown>>,
) {
    if events.is_empty() {
        return;
    }
    events.clear();

    if let Some(mut ac) = action_cooldown {
        ac.0.clear();
    }
    if let Some(mut pc) = prod_cooldown {
        pc.0.clear();
    }
}

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
    Drop {
        target_pos: GridPosition,
        cargo_entity: Entity,
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

        // 戦闘不能判定（HPが低い、または弾薬切れ）
        let is_combat_ineffective = atk_hp < 70 || (stats.max_ammo1 > 0 && atk_ammo.0 == 0);

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
        let enemy_units: Vec<(GridPosition, crate::resources::UnitType, u32, u32, u32, u32)> = {
            let mut q = world.query::<(&GridPosition, &Faction, &UnitStats, &Health)>();
            q.iter(world)
                .filter(|(_, f, _, h)| f.0 != player_id && h.current > 0)
                .map(|(p, _, s, h)| (*p, s.unit_type, s.cost, h.current, s.min_range, s.max_range))
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

            // 戦闘不能時の撤退先探索
            if is_combat_ineffective {
                let mut min_recovery_dist = 99;
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
                let mut min_base_dist = 99;
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
            if is_infantry && !is_combat_ineffective && is_unit_stranded(world, &pos, player_id, &properties, &enemy_units) {
                let mut min_coast_dist = 99;

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

                for (e_pos, e_type, e_cost, e_hp, _, _) in &enemy_units {
                    let d = (current_grid.x as i32 - e_pos.x as i32).abs()
                        + (current_grid.y as i32 - e_pos.y as i32).abs();

                    let mut effective_dist = d;
                    // 海軍ユニットが陸上の敵を追跡する場合の補正
                    if stats.movement_type == crate::resources::MovementType::Ship
                        && let Some(e_terrain) = map.get_terrain(e_pos.x, e_pos.y)
                    {
                        let move_cost = registry
                            .get_movement_cost(
                                crate::resources::MovementType::Ship,
                                e_terrain.as_str(),
                            )
                            .unwrap_or(99);
                        if move_cost >= 99 && stats.max_range <= 1 {
                            // 進入不可能な陸地で、かつ直接攻撃ユニットの場合は距離を大幅に水増し
                            effective_dist += 20;
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
                    let mut min_dist = 99;
                    // 1. 敵ユニットを探す
                    for (e_pos, _, _, _, _, _) in &enemy_units {
                        let mut d = (current_grid.x as i32 - e_pos.x as i32).abs()
                            + (current_grid.y as i32 - e_pos.y as i32).abs();

                        if stats.movement_type == crate::resources::MovementType::Ship
                            && let Some(e_terrain) = map.get_terrain(e_pos.x, e_pos.y)
                        {
                            let move_cost = registry
                                .get_movement_cost(
                                    crate::resources::MovementType::Ship,
                                    e_terrain.as_str(),
                                )
                                .unwrap_or(99);
                            if move_cost >= 99 && stats.max_range <= 1 {
                                d += 20;
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
                                let d = (current_grid.x as i32 - p_pos.x as i32).abs()
                                    + (current_grid.y as i32 - p_pos.y as i32).abs();
                                if d < min_dist {
                                    min_dist = d;
                                }
                            } else if is_combat_ineffective
                                && registry.can_repair_on_terrain(stats.unit_type, *p_terrain)
                            {
                                // 自身が修理が必要な場合のみ、自分の拠点もターゲットに含める
                                let d = (current_grid.x as i32 - p_pos.x as i32).abs()
                                    + (current_grid.y as i32 - p_pos.y as i32).abs();
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
                        let dist = (current_grid.x as i64 - t_pos.x as i64).unsigned_abs() as u32
                            + (current_grid.y as i64 - t_pos.y as i64).unsigned_abs() as u32;

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
            if actions.can_merge {
                let targets = crate::systems::merge::get_mergable_targets_at(
                    world,
                    unit_entity,
                    current_grid,
                );
                for target_entity in targets {
                    let mut merge_score = 3000;
                    if let (Some(t_health), Some(_t_stats)) = (
                        world.get::<Health>(target_entity),
                        world.get::<UnitStats>(target_entity),
                    ) {
                        // フルHP同士の合流は無意味なのでスコアを0にする
                        let total_hp = atk_hp + t_health.current;
                        if total_hp > 100 {
                            merge_score = 0;
                        } else {
                            // 自身または相手のHPが低い場合、合流の価値を高める
                            if is_combat_ineffective || t_health.current < 40 {
                                merge_score += 4000;
                            }
                            // 合流後のHPが無駄にならないなら加点
                            if total_hp <= 100 {
                                merge_score += 1000;
                            }
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

            // (D) Load
            if actions.can_load {
                let transports = crate::systems::transport::get_loadable_transports_at(
                    world,
                    unit_entity,
                    current_grid,
                );
                for transport_entity in transports {
                    // 目的地までの距離が遠いほど、搭載する価値が高まる
                    let mut min_objective_dist = 99;
                    for (p_pos, _, p_owner) in &properties {
                        if *p_owner != Some(player_id) {
                            let d = (current_grid.x as i32 - p_pos.x as i32).abs()
                                + (current_grid.y as i32 - p_pos.y as i32).abs();
                            if d < min_objective_dist {
                                min_objective_dist = d;
                            }
                        }
                    }

                    let mut load_score = 2000;
                    if min_objective_dist > 5 {
                        load_score += 3000; // 遠い場合は積極的に乗る
                    }
                    if stats.can_capture && min_objective_dist > 8 {
                        load_score += 2000; // 占領可能な歩兵は特に遠い場合に優先
                    }

                    let score = base_tile_score + load_score;
                    #[allow(clippy::collapsible_if)]
                    if score > best_unit_score {
                        best_unit_score = score;
                        best_unit_choice = Some(AiCommand::Load {
                            transport_entity,
                            target_pos: current_grid,
                        });
                    }
                }
            }

            // (E) Drop
            if actions.can_drop {
                #[allow(clippy::collapsible_if)]
                if let Ok(cargo) = world
                    .query::<&crate::components::CargoCapacity>()
                    .get(world, unit_entity)
                {
                    let cargo_entities = cargo.loaded.clone();
                    for cargo_entity in cargo_entities {
                        // 未行動のユニットのみ降ろす
                        if let Some(action) =
                            world.get::<crate::components::ActionCompleted>(cargo_entity)
                        {
                            #[allow(clippy::collapsible_if)]
                            if !action.0 {
                                // 降車可能なマスを探索
                                if let Some(cargo_unit_type) =
                                    world.get::<UnitStats>(cargo_entity).map(|s| s.unit_type)
                                {
                                    let drop_tiles = crate::systems::transport::get_droppable_tiles(
                                        world,
                                        unit_entity,
                                        cargo_entity,
                                    );
                                    for drop_tile in drop_tiles {
                                        let drop_pos = GridPosition {
                                            x: drop_tile.0,
                                            y: drop_tile.1,
                                        };

                                        // 降車先の価値を評価
                                        let mut drop_score: i32 = 5000; // 基本的に降ろすのは良いこと

                                        // 降車先が拠点ならボーナス
                                        for (p_pos, _, p_owner) in &properties {
                                            if p_pos.x == drop_pos.x && p_pos.y == drop_pos.y {
                                                if *p_owner != Some(player_id) {
                                                    drop_score += 3000; // 敵拠点の占領準備
                                                }
                                                break;
                                            }
                                        }

                                        // 敵との距離と危険度を評価
                                        let mut min_enemy_dist = 99;
                                        let mut max_threat = 0;
                                        for (e_pos, e_unit_type, _, _, e_min_range, e_max_range) in
                                            &enemy_units
                                        {
                                            let d = (drop_pos.x as i32 - e_pos.x as i32).abs()
                                                + (drop_pos.y as i32 - e_pos.y as i32).abs();
                                            if d < min_enemy_dist {
                                                min_enemy_dist = d;
                                            }
                                            // 敵の攻撃範囲（射程内）なら脅威を計算
                                            // 間接攻撃ユニットの死角を考慮するため、最小射程もチェックする
                                            if d >= *e_min_range as i32 && d <= *e_max_range as i32
                                            {
                                                if let Some(dmg) = damage_chart
                                                    .get_base_damage(*e_unit_type, cargo_unit_type)
                                                {
                                                    if dmg > max_threat {
                                                        max_threat = dmg;
                                                    }
                                                }
                                            }
                                        }

                                        // 脅威度に応じた動的なペナルティ
                                        // ダメージ期待値が50%を超えるような無謀な降車は避ける
                                        if max_threat > 50 {
                                            drop_score = drop_score.saturating_sub(4000);
                                        } else if max_threat > 20 {
                                            drop_score = drop_score.saturating_sub(1500);
                                        } else if max_threat > 0 {
                                            drop_score = drop_score.saturating_sub(500);
                                        }

                                        // 敵が近く、かつ安全ならボーナス（次ターン攻撃用）
                                        if max_threat == 0 && (1..=3).contains(&min_enemy_dist) {
                                            drop_score += 2000;
                                        }

                                        let score = base_tile_score + drop_score;
                                        #[allow(clippy::collapsible_if)]
                                        if score > best_unit_score {
                                            best_unit_score = score;
                                            best_unit_choice = Some(AiCommand::Drop {
                                                target_pos: drop_pos,
                                                cargo_entity,
                                            });
                                        }
                                    }
                                }
                            }
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
        AiCommand::Drop {
            target_pos,
            cargo_entity,
        } => {
            if let Some(mut evs) =
                world.get_resource_mut::<Events<crate::events::UnloadUnitCommand>>()
            {
                evs.send(crate::events::UnloadUnitCommand {
                    transport_entity: unit_entity,
                    cargo_entity,
                    target_x: target_pos.x,
                    target_y: target_pos.y,
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
    // ミッションの割り当てを試行する
    crate::ai::planner::assign_transport_missions(world, active_player);

    let mut skip_entities = std::collections::HashSet::new();
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

    let mission_cmd_and_entity = if let Some(manager) =
        world.get_resource::<crate::ai::missions::TransportMissionManager>()
    {
        let missions = manager.missions.clone();
        missions.into_iter().find_map(|m| {
            if !skip_entities.contains(&m.transport_entity)
                && world
                    .get::<Faction>(m.transport_entity)
                    .is_some_and(|f| f.0 == active_player)
            {
                crate::ai::missions::execute_mission_step(world, &m)
                    .map(|cmd| (m.transport_entity, cmd))
            } else {
                None
            }
        })
    } else {
        None
    };

    if let Some((entity, cmd)) = mission_cmd_and_entity {
        execute_ai_command(world, entity, cmd);
        if let Some(mut res) = world.get_resource_mut::<AiActionCooldown>() {
            res.0.insert(entity);
        } else {
            let mut set = std::collections::HashSet::new();
            set.insert(entity);
            world.insert_resource(AiActionCooldown(set));
        }
        return true;
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

    // 2. 生産行動
    let prod_commands = super::production::decide_production(world, active_player);

    let cooldown_set = if let Some(res) = world.get_resource::<AiProductionCooldown>() {
        res.0.clone()
    } else {
        HashSet::new()
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

        // 直前のエラーがこのコマンドに関連しているかチェック
        let cmd_debug = format!("{:?}", cmd);
        if last_error.is_some() && last_event_str.as_deref() == Some(&cmd_debug) {
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
                let mut set = HashSet::new();
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
            return true;
        }
    }

    // 3. 全行動完了 -> ターン終了
    if let Some(mut end_events) =
        world.get_resource_mut::<Events<crate::events::NextPhaseCommand>>()
    {
        end_events.send(crate::events::NextPhaseCommand);
    }
    false
}

fn is_unit_stranded(
    world: &World,
    pos: &GridPosition,
    player_id: PlayerId,
    properties: &[(GridPosition, crate::resources::Terrain, Option<PlayerId>)],
    enemy_units: &[(GridPosition, crate::resources::UnitType, u32, u32, u32, u32)],
) -> bool {
    if let Some(island_map) = world.get_resource::<crate::ai::islands::IslandMap>()
        && let Some(my_island) = island_map.get_island_at(pos)
    {
        let mut local_targets = false;
        for (p_pos, _, p_owner) in properties {
            if *p_owner != Some(player_id) && my_island.tiles.contains(p_pos) {
                local_targets = true;
                break;
            }
        }

        let mut local_enemies = false;
        for (e_pos, _, _, _, _, _) in enemy_units {
            if my_island.tiles.contains(e_pos) {
                local_enemies = true;
                break;
            }
        }

        if !local_targets && !local_enemies {
            return true;
        }
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

    #[test]
    fn test_decide_ai_action_load() {
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

        world.spawn((
            GridPosition { x: 9, y: 9 },
            Property {
                terrain: Terrain::City,
                owner_id: Some(p2),
                capture_points: 20,
                max_capture_points: 20,
            },
        ));

        let _inf = world
            .spawn((
                p1,
                Faction(p1),
                HasMoved(false),
                ActionCompleted(false),
                GridPosition { x: 1, y: 1 },
                UnitStats {
                    unit_type: UnitType::Infantry,
                    max_movement: 3,
                    can_capture: true,
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

        let _transport = world
            .spawn((
                p1,
                Faction(p1),
                HasMoved(false),
                ActionCompleted(false),
                GridPosition { x: 1, y: 1 },
                UnitStats {
                    unit_type: UnitType::TransportHelicopter,
                    max_cargo: 2,
                    loadable_unit_types: vec![UnitType::Infantry],
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
                crate::components::CargoCapacity {
                    max: 2,
                    loaded: vec![],
                },
            ))
            .id();

        let skips = std::collections::HashSet::new();
        let action = decide_ai_action(&mut world, p1, &skips);

        assert!(action.is_some());
        let (_ent, cmd) = action.unwrap();
        match cmd {
            AiCommand::Load { .. } => {}
            other => panic!("Expected Load command, got {:?}", other),
        }
    }

    #[test]
    fn test_decide_ai_action_drop() {
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

        world.spawn((
            GridPosition { x: 1, y: 2 },
            Property {
                terrain: Terrain::City,
                owner_id: Some(p2),
                capture_points: 20,
                max_capture_points: 20,
            },
        ));

        let inf = world
            .spawn((
                p1,
                Faction(p1),
                HasMoved(true),
                ActionCompleted(false),
                GridPosition { x: 999, y: 999 },
                UnitStats {
                    unit_type: UnitType::Infantry,
                    movement_type: crate::resources::MovementType::Infantry,
                    can_capture: true,
                    ..UnitStats::mock()
                },
                Health {
                    current: 100,
                    max: 100,
                },
                crate::components::Transporting(Entity::from_raw(0)),
            ))
            .id();

        let transport = world
            .spawn((
                p1,
                Faction(p1),
                HasMoved(false),
                ActionCompleted(false),
                GridPosition { x: 1, y: 1 },
                UnitStats {
                    unit_type: UnitType::TransportHelicopter,
                    max_cargo: 2,
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
                crate::components::CargoCapacity {
                    max: 2,
                    loaded: vec![inf],
                },
            ))
            .id();

        world
            .entity_mut(inf)
            .insert(crate::components::Transporting(transport));

        let skips = std::collections::HashSet::new();
        let action = decide_ai_action(&mut world, p1, &skips);

        assert!(action.is_some());
        let (_ent, cmd) = action.unwrap();
        match cmd {
            AiCommand::Drop {
                target_pos,
                cargo_entity,
            } => {
                assert_eq!(cargo_entity, inf);
                assert_eq!(target_pos.x, 1);
                assert_eq!(target_pos.y, 2);
            }
            other => panic!("Expected Drop command, got {:?}", other),
        }
    }

    #[test]
    fn test_decide_ai_action_retreat_low_hp() {
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

        let p1 = PlayerId(1);
        // 都市を(1,1)に設置
        world.spawn((
            GridPosition { x: 1, y: 1 },
            Property::new(Terrain::City, Some(p1), 200),
        ));

        // 低HP(30)の戦車を(1,0)に配置
        world.spawn((
            p1,
            Faction(p1),
            HasMoved(false),
            ActionCompleted(false),
            GridPosition { x: 1, y: 0 },
            UnitStats {
                unit_type: UnitType::Tank,
                cost: 7000,
                max_movement: 3,
                movement_type: crate::resources::MovementType::Tank,
                max_fuel: 99,
                ..UnitStats::mock()
            },
            Health {
                current: 30,
                max: 100,
            },
            crate::components::Fuel {
                current: 99,
                max: 99,
            },
        ));

        let skips = std::collections::HashSet::new();
        let action = decide_ai_action(&mut world, p1, &skips);

        assert!(action.is_some());
        if let Some((_, AiCommand::Wait { target_pos })) = action {
            // (1,1)の都市へ移動して待機することを確認
            assert_eq!(target_pos.x, 1);
            assert_eq!(target_pos.y, 1);
        } else {
            panic!("Expected Wait at (1,1), got {:?}", action);
        }
    }

    #[test]
    fn test_decide_ai_action_merge() {
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

        let p1 = PlayerId(1);

        // 低HP(50)の歩兵Aを(0,0)に配置
        let unit_a = world
            .spawn((
                p1,
                Faction(p1),
                HasMoved(false),
                ActionCompleted(false),
                GridPosition { x: 0, y: 0 },
                UnitStats {
                    unit_type: UnitType::Infantry,
                    cost: 1000,
                    max_movement: 3,
                    movement_type: crate::resources::MovementType::Infantry,
                    ..UnitStats::mock()
                },
                Health {
                    current: 50,
                    max: 100,
                },
                crate::components::Fuel {
                    current: 99,
                    max: 99,
                },
            ))
            .id();

        // 低HP(40)の歩兵Bを(1,0)に配置
        let unit_b = world
            .spawn((
                p1,
                Faction(p1),
                HasMoved(false),
                ActionCompleted(false),
                GridPosition { x: 1, y: 0 },
                UnitStats {
                    unit_type: UnitType::Infantry,
                    cost: 1000,
                    max_movement: 3,
                    movement_type: crate::resources::MovementType::Infantry,
                    ..UnitStats::mock()
                },
                Health {
                    current: 40,
                    max: 100,
                },
                crate::components::Fuel {
                    current: 99,
                    max: 99,
                },
            ))
            .id();

        let skips = std::collections::HashSet::new();
        let action = decide_ai_action(&mut world, p1, &skips);

        assert!(action.is_some());
        // 歩兵Aが歩兵Bの位置(1,0)へ移動してMergeすることを確認
        if let Some((
            entity,
            AiCommand::Merge {
                target_pos,
                target_entity,
            },
        )) = action
        {
            assert_eq!(entity, unit_a);
            assert_eq!(target_pos.x, 1);
            assert_eq!(target_pos.y, 0);
            assert_eq!(target_entity, unit_b);
        } else {
            panic!("Expected Merge command, got {:?}", action);
        }
    }

    #[test]
    fn test_decide_ai_action_retreat_no_ammo() {
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

        let p1 = PlayerId(1);
        // 都市を(1,1)に設置
        world.spawn((
            GridPosition { x: 1, y: 1 },
            Property::new(Terrain::City, Some(p1), 200),
        ));

        // 弾薬切れ(0)の戦車を(1,0)に配置
        world.spawn((
            p1,
            Faction(p1),
            HasMoved(false),
            ActionCompleted(false),
            GridPosition { x: 1, y: 0 },
            UnitStats {
                unit_type: UnitType::Tank,
                cost: 7000,
                max_movement: 3,
                movement_type: crate::resources::MovementType::Tank,
                max_fuel: 99,
                max_ammo1: 5, // 主武装あり
                ..UnitStats::mock()
            },
            Health {
                current: 100, // HPは満タン
                max: 100,
            },
            crate::components::Ammo {
                ammo1: 0, // 弾薬切れ
                max_ammo1: 5,
                ammo2: 99,
                max_ammo2: 99,
            },
            crate::components::Fuel {
                current: 99,
                max: 99,
            },
        ));

        let skips = std::collections::HashSet::new();
        let action = decide_ai_action(&mut world, p1, &skips);

        assert!(action.is_some());
        if let Some((_, AiCommand::Wait { target_pos })) = action {
            // (1,1)の都市へ移動して待機することを確認
            assert_eq!(target_pos.x, 1);
            assert_eq!(target_pos.y, 1);
        } else {
            panic!("Expected Wait at (1,1) due to no ammo, got {:?}", action);
        }
    }

    #[test]
    fn test_ai_action_taxi_back() {
        let master_data = MasterDataRegistry::load().unwrap();
        let (mut world, _) =
            crate::setup::initialize_world_from_master_data(&master_data, "map_1").unwrap();
        let p1 = PlayerId(1);

        // 1. 全ユニットをクリア
        let entities: Vec<Entity> = world.query::<Entity>().iter(&world).collect();
        for e in entities {
            world.despawn(e);
        }

        // 2. 首都（生産拠点）を設置 (x=0, y=0)
        let capital_pos = GridPosition { x: 0, y: 0 };
        world.spawn((capital_pos, Property::new(Terrain::Capital, Some(p1), 100)));

        // 3. 空の輸送ヘリを「前線（遠く）」に設置 (x=8, y=0)
        let heli_pos = GridPosition { x: 8, y: 0 };
        let heli_entity = world
            .spawn((
                heli_pos,
                p1,
                Faction(p1),
                HasMoved(false),
                ActionCompleted(false),
                UnitStats {
                    unit_type: UnitType::TransportHelicopter,
                    max_movement: 6,
                    movement_type: crate::resources::MovementType::Air,
                    max_cargo: 1,
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
                crate::components::CargoCapacity {
                    loaded: vec![],
                    max: 1,
                },
            ))
            .id();

        // 4. AIに行動を決定させる
        let skips = std::collections::HashSet::new();
        let action = decide_ai_action(&mut world, p1, &skips);

        // 5. 検証: 輸送ヘリが首都（x=0）の方向に移動しようとしていること
        assert!(action.is_some());
        if let Some((entity, AiCommand::Wait { target_pos })) = action {
            assert_eq!(entity, heli_entity);
            assert!(
                target_pos.x < heli_pos.x,
                "Empty transport should move back towards capital (x=0). Target: {:?}, Current: {:?}",
                target_pos,
                heli_pos
            );
        } else {
            panic!("Expected Wait command for taxi-back, got {:?}", action);
        }
    }
}
