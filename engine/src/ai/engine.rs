#![allow(clippy::collapsible_if)]
#![allow(clippy::unnecessary_min_or_max)]
#![allow(clippy::unnecessary_map_or)]

use crate::ai::turn_distance::{TurnDistanceCache, calculate_turn_distance};
use crate::components::{
    ActionCompleted, Faction, GridPosition, HasMoved, Health, PlayerId, Property, UnitStats,
};
use crate::events::{AttackUnitCommand, CapturePropertyCommand, MoveUnitCommand, WaitUnitCommand};
use crate::resources::master_data::MasterDataRegistry;
use crate::resources::{Map, Terrain};
use crate::systems::combat::get_expected_damage;
use crate::systems::movement::{OccupantInfo, calculate_reachable_tiles};
use bevy_ecs::prelude::*;
use std::collections::{HashMap, HashSet, VecDeque};

#[derive(Resource, Default)]
pub struct AiActionCooldown(pub HashSet<Entity>);

fn transport_has_other_actionable_cargo(
    world: &World,
    transport: Entity,
    current_cargo: Entity,
) -> bool {
    world
        .get::<crate::components::CargoCapacity>(transport)
        .is_some_and(|capacity| {
            capacity.loaded.iter().any(|loaded| {
                *loaded != current_cargo
                    && world
                        .get::<crate::components::ActionCompleted>(*loaded)
                        .is_some_and(|action| !action.0)
            })
        })
}

#[derive(Resource, Default)]
pub struct AiProductionCooldown(pub HashSet<(usize, usize)>);

/// V3が同一ターンの部隊計画と島嶼キャンペーン分析を再実行しないための一時cache。
/// 次フェーズでResourceごと削除し、キャンペーンの永続状態としては扱わない。
#[derive(Resource, Default)]
pub struct AiTurnStrategyCache {
    player_id: Option<PlayerId>,
    squads_planned: bool,
    campaign_portfolio: Option<crate::ai::island_campaign::IslandCampaignPortfolio>,
    campaign_production_planned: bool,
    campaign_production_commands: VecDeque<crate::events::ProduceUnitCommand>,
    campaign_production_blocks_generic: bool,
}

impl AiTurnStrategyCache {
    pub(crate) fn set_campaign_portfolio(
        &mut self,
        player_id: PlayerId,
        portfolio: crate::ai::island_campaign::IslandCampaignPortfolio,
    ) {
        if self.player_id != Some(player_id) {
            self.clear();
            self.player_id = Some(player_id);
        }
        self.campaign_portfolio = Some(portfolio);
    }

    pub(crate) fn campaign_portfolio(
        &self,
        player_id: PlayerId,
    ) -> Option<&crate::ai::island_campaign::IslandCampaignPortfolio> {
        (self.player_id == Some(player_id))
            .then_some(self.campaign_portfolio.as_ref())
            .flatten()
    }

    pub(crate) fn mark_squads_planned(&mut self, player_id: PlayerId) {
        if self.player_id != Some(player_id) {
            self.clear();
            self.player_id = Some(player_id);
        }
        self.squads_planned = true;
    }

    fn squads_planned(&self, player_id: PlayerId) -> bool {
        self.player_id == Some(player_id) && self.squads_planned
    }

    pub(crate) fn set_campaign_production_plan(
        &mut self,
        player_id: PlayerId,
        commands: Vec<crate::events::ProduceUnitCommand>,
        completed_all_rows: bool,
    ) {
        if self.player_id != Some(player_id) {
            self.clear();
            self.player_id = Some(player_id);
        }
        self.campaign_production_planned = true;
        self.campaign_production_commands = VecDeque::from(commands);
        self.campaign_production_blocks_generic = !completed_all_rows;
    }

    pub(crate) fn campaign_production_planned(&self, player_id: PlayerId) -> bool {
        self.player_id == Some(player_id) && self.campaign_production_planned
    }

    pub(crate) fn take_campaign_production_command(
        &mut self,
        player_id: PlayerId,
    ) -> Option<crate::events::ProduceUnitCommand> {
        (self.player_id == Some(player_id))
            .then(|| self.campaign_production_commands.pop_front())
            .flatten()
    }

    pub(crate) fn campaign_production_blocks_generic(&self, player_id: PlayerId) -> bool {
        self.player_id == Some(player_id) && self.campaign_production_blocks_generic
    }

    fn clear(&mut self) {
        self.player_id = None;
        self.squads_planned = false;
        self.campaign_portfolio = None;
        self.campaign_production_planned = false;
        self.campaign_production_commands.clear();
        self.campaign_production_blocks_generic = false;
    }
}

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
        transport_target_pos: GridPosition,
        cargo_drop_pos: GridPosition,
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

    let mut turn_cache = crate::ai::turn_distance::AiTurnCache::default();

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
        let enemy_units: Vec<(
            GridPosition,
            crate::resources::UnitType,
            u32,
            u32,
            u32,
            u32,
            u32,
        )> = {
            let mut q = world.query::<(&GridPosition, &Faction, &UnitStats, &Health)>();
            q.iter(world)
                .filter(|(_, f, _, h)| f.0 != player_id && h.current > 0)
                .map(|(p, _, s, h)| {
                    (
                        *p,
                        s.unit_type,
                        s.cost,
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
            transport_target_pos,
            cargo_drop_pos,
            cargo_entity,
        } => {
            if let Some(mut evs) = world.get_resource_mut::<Events<MoveUnitCommand>>() {
                evs.send(MoveUnitCommand {
                    unit_entity,
                    target_x: transport_target_pos.x,
                    target_y: transport_target_pos.y,
                });
            }
            if let Some(mut evs) =
                world.get_resource_mut::<Events<crate::events::UnloadUnitCommand>>()
            {
                evs.send(crate::events::UnloadUnitCommand {
                    transport_entity: unit_entity,
                    cargo_entity,
                    target_x: cargo_drop_pos.x,
                    target_y: cargo_drop_pos.y,
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
/// 何らかの行動を実行した場合はその行動内容（文字列）を `Some` で返し、ターンが終了した場合は `None` を返します。
/// AIのメイン実行エントリーポイント。
pub fn execute_ai_turn(world: &mut World, active_player: PlayerId) -> Option<String> {
    let ai_version = crate::ai::resolve_player_ai_version(world, active_player);

    match ai_version {
        crate::ai::ai_version::AiVersion::V1 => execute_ai_turn_v1(world, active_player),
        // V3/V4 は V2 と同じ部隊編成・ビーム探索パイプラインを共有し、
        // タイル評価 (decide_ai_action_v2) と盤面評価の中でバージョン別の強化を行う
        // （V4 の差分は生産判断のみで、行動決定パイプラインは V3 と同一）
        crate::ai::ai_version::AiVersion::V2
        | crate::ai::ai_version::AiVersion::V3
        | crate::ai::ai_version::AiVersion::V4 => execute_ai_turn_v2(world, active_player),
    }
}

/// 従来型 AI (V1) のメイン実行ループ
pub fn execute_ai_turn_v1(world: &mut World, active_player: PlayerId) -> Option<String> {
    // 1. ユニット行動を1つ決定・実行
    // AI思考ループの中で、エンジン側のフラグが更新されるのを待たずに
    // 同一フレーム内の重複思考を避けるために、リソースで「指示済みユニット」を管理します。
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

    // クリーンアップ後の状態を基に、新規ミッションを割り当てる
    crate::ai::planner::assign_transport_missions(world, active_player);

    // ミッションに関与している全Entity（輸送機と歩兵）を収集し、通常の意思決定から完全に除外する
    let mut mission_entities = std::collections::HashSet::new();
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
            let mut set = std::collections::HashSet::new();
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
            let mut set = std::collections::HashSet::new();
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

/// 新しいAI (V2) のメイン実行ループ。
/// 最初のステップで部隊再編成とビーム探索をキャッシュし、毎ステップ1アクションずつ実行します。
pub fn execute_ai_turn_v2(world: &mut World, active_player: PlayerId) -> Option<String> {
    let mut skip_entities = std::collections::HashSet::new();
    if let Some(res) = world.get_resource::<AiActionCooldown>() {
        skip_entities = res.0.clone();
    }

    let uses_v3 = crate::ai::resolve_player_ai_version(world, active_player).uses_v3_tactics();
    let should_plan_squads = skip_entities.is_empty()
        && (!uses_v3
            || !world
                .get_resource::<AiTurnStrategyCache>()
                .is_some_and(|cache| cache.squads_planned(active_player)));

    // V3は行動可能ユニットがなくても同一ターンの再計画を避け、V1/V2は従来条件を維持する。
    if should_plan_squads {
        crate::ai::squad::plan_squads(world, active_player);
        crate::ai::beam_search::run_squad_beam_search(world, active_player);
    } else {
        // 降車が実際に発生した場合だけ再編成し、通常行動ごとの全盤面走査を避ける。
        let needs_transport_reconcile = world
            .get_resource::<crate::ai::squad::SquadManager>()
            .is_some_and(|manager| {
                manager.squads.iter().any(|squad| {
                    let delivered_cargo = matches!(
                        squad.phase,
                        crate::ai::squad::MissionPhase::Transport(
                            crate::ai::squad::TransportPhase::Transit
                                | crate::ai::squad::TransportPhase::Drop
                        )
                    ) && squad.cargo_entities.iter().any(|cargo| {
                        world
                            .get::<crate::components::Transporting>(*cargo)
                            .is_none()
                            && world.get::<GridPosition>(*cargo).is_some()
                    });
                    let pickup_completed = matches!(
                        squad.phase,
                        crate::ai::squad::MissionPhase::Transport(
                            crate::ai::squad::TransportPhase::Pickup
                        )
                    ) && !squad.cargo_entities.is_empty()
                        && squad.cargo_entities.iter().all(|cargo| {
                            squad.transport_entity.is_some_and(|transport| {
                                world
                                    .get::<crate::components::Transporting>(*cargo)
                                    .is_some_and(|transporting| transporting.0 == transport)
                            })
                        });
                    delivered_cargo || pickup_completed
                })
            });
        if needs_transport_reconcile {
            crate::ai::squad::update_squads(world, active_player);
        }
    }

    // 1. 輸送部隊の優先実行
    let mut transport_action = None;
    if let Some(mut manager) = world.remove_resource::<crate::ai::squad::SquadManager>() {
        for squad in &mut manager.squads {
            if squad.mission_type == crate::ai::squad::MissionType::Transport
                && squad.owner_id == Some(active_player)
                && crate::ai::squad::squad_is_mutable_by_player(world, squad, active_player)
            {
                let is_transport_cooldown = squad
                    .transport_entity
                    .is_none_or(|entity| skip_entities.contains(&entity));
                let are_all_cargo_on_cooldown = !squad.cargo_entities.is_empty()
                    && squad
                        .cargo_entities
                        .iter()
                        .all(|entity| skip_entities.contains(entity));

                // 輸送役と指名カーゴは独立して行動できるため、いずれかが行動可能なら継続する。
                if is_transport_cooldown && are_all_cargo_on_cooldown {
                    continue;
                }

                let step_res =
                    crate::ai::squad::execute_transport_squad_step(world, squad, &skip_entities);
                if let Some((entity, cmd)) = step_res {
                    if !skip_entities.contains(&entity) {
                        transport_action = Some((entity, cmd));
                        break;
                    }
                }
            }
        }
        world.insert_resource(manager);
    }

    if let Some((entity, cmd)) = transport_action {
        // Drop は輸送役だけでなく、降車したcargoもそのターンの行動を消費する。
        // 両者を記録し、実際には行動済みのcargoを遊兵として数えないようにする。
        let affected_cargo = match &cmd {
            AiCommand::Drop { cargo_entity, .. } => Some(*cargo_entity),
            _ => None,
        };
        // Unload systemは未行動cargoが残る間、輸送役を行動完了にしない。
        // AI側も同じ契約に合わせ、最後のcargoを降ろすまで輸送役をcooldownしない。
        let transport_can_continue_drop = match &cmd {
            AiCommand::Drop { cargo_entity, .. } => {
                transport_has_other_actionable_cargo(world, entity, *cargo_entity)
            }
            _ => false,
        };
        let cmd_str = format!("{:?}", cmd);
        execute_ai_command(world, entity, cmd);
        if let Some(mut res) = world.get_resource_mut::<AiActionCooldown>() {
            if !transport_can_continue_drop {
                res.0.insert(entity);
            }
            if let Some(cargo_entity) = affected_cargo {
                res.0.insert(cargo_entity);
            }
        } else {
            let mut set = std::collections::HashSet::new();
            if !transport_can_continue_drop {
                set.insert(entity);
            }
            if let Some(cargo_entity) = affected_cargo {
                set.insert(cargo_entity);
            }
            world.insert_resource(AiActionCooldown(set));
        }
        return Some(cmd_str);
    }

    // 通常の意思決定を行う際には、輸送中のEntity（輸送機と歩兵）を通常AIのスキップ対象に追加する
    let mut decide_skip_entities = skip_entities.clone();
    if let Some(manager) = world.get_resource::<crate::ai::squad::SquadManager>() {
        for squad in &manager.squads {
            if squad.mission_type == crate::ai::squad::MissionType::Transport {
                if let Some(transport_entity) = squad.transport_entity {
                    decide_skip_entities.insert(transport_entity);
                }
                decide_skip_entities.extend(squad.cargo_entities.iter().copied());
            }
        }
    }

    // 2. 通常部隊・SoloFallback ユニットの行動決定 (V2意思決定)
    if let Some((entity, command)) =
        decide_ai_action_v2(world, active_player, &decide_skip_entities)
    {
        let cmd_str = format!("{:?}", command);
        execute_ai_command(world, entity, command);

        if let Some(mut res) = world.get_resource_mut::<AiActionCooldown>() {
            res.0.insert(entity);
        } else {
            let mut set = std::collections::HashSet::new();
            set.insert(entity);
            world.insert_resource(AiActionCooldown(set));
        }
        return Some(cmd_str);
    }

    // 3. 生産行動
    // 生産内容は盤面・資金の変化ごとに従来通り再計画し、V3の島分析だけを同一ターンで共有する。
    let prod_commands = super::production::decide_production(world, active_player);

    let cooldown_set = if let Some(res) = world.get_resource::<AiProductionCooldown>() {
        res.0.clone()
    } else {
        HashSet::new()
    };

    let (last_error, last_event_str) =
        if let Some(diag) = world.get_resource::<crate::resources::ProductionDiagnostic>() {
            (diag.last_error.clone(), diag.last_event.clone())
        } else {
            (None, None)
        };

    for cmd in prod_commands {
        if cooldown_set.contains(&(cmd.target_x, cmd.target_y)) {
            continue;
        }

        let cmd_str = format!("{:?}", cmd);

        if last_error.is_some() && last_event_str.as_deref() == Some(&cmd_str) {
            if let Some(mut res) = world.get_resource_mut::<AiProductionCooldown>() {
                res.0.insert((cmd.target_x, cmd.target_y));
            }
            continue;
        }

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
            return Some(cmd_str);
        }
    }

    // 4. 全行動完了 -> ターン終了
    // ターン終了直前は「このターン結局何が動かなかったか」が確定する唯一の点。
    // AiActionCooldown はターン境界で破棄されるため、ここで遊兵を計上しておく。
    let acted_entities = world
        .get_resource::<AiActionCooldown>()
        .map(|res| res.0.clone())
        .unwrap_or_default();
    let idle_audit = crate::ai::idle_audit::audit_idle_units(world, active_player, &acted_entities);
    if let Some(mut diagnostics) =
        world.get_resource_mut::<crate::ai::idle_audit::IdleAuditDiagnostics>()
    {
        diagnostics.record(idle_audit);
    } else {
        let mut diagnostics = crate::ai::idle_audit::IdleAuditDiagnostics::default();
        diagnostics.record(idle_audit);
        world.insert_resource(diagnostics);
    }

    if let Some(mut end_events) =
        world.get_resource_mut::<Events<crate::events::NextPhaseCommand>>()
    {
        end_events.send(crate::events::NextPhaseCommand);
    }
    None
}

/// #45 (V3): 待ち伏せポジションのスコア。射程内で待機して先制攻撃を狙う位置
const AMBUSH_IN_RANGE_BONUS: i32 = 4000;
/// #45 (V3): 敵の進行を1〜2ターン待ち受けられる位置のスコア
const AMBUSH_NEAR_RANGE_BONUS: i32 = 2000;
/// #45 (V3): 最小射程より内側 (攻撃不能な近距離) へ前進するペナルティ
const AMBUSH_TOO_CLOSE_PENALTY: i32 = 3000;
/// #45 (V3): 待ち受けゾーンとみなす最大射程からのマージン (敵の接近を想定)
const AMBUSH_APPROACH_MARGIN: u32 = 2;

/// 通常スコアとは別軸で緊急行動を比較する優先度。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum ActionPriority {
    Normal,
    EmergencyAdvance,
    EmergencyRouteBlock,
    EmergencySiteOccupation,
    EmergencyNeutralization,
}

fn emergency_position_priority(
    mission: Option<&crate::ai::emergency::EmergencyMission>,
    origin: GridPosition,
    candidate: GridPosition,
    map: &Map,
) -> ActionPriority {
    let Some(mission) = mission else {
        return ActionPriority::Normal;
    };
    if candidate == mission.target_position {
        return match mission.response {
            crate::ai::emergency::EmergencyResponse::EliminateThreat => {
                ActionPriority::EmergencyAdvance
            }
            crate::ai::emergency::EmergencyResponse::OccupySite => {
                ActionPriority::EmergencySiteOccupation
            }
            crate::ai::emergency::EmergencyResponse::BlockRoute => {
                ActionPriority::EmergencyRouteBlock
            }
        };
    }

    let original_distance = map.distance(
        origin.x,
        origin.y,
        mission.target_position.x,
        mission.target_position.y,
    );
    let candidate_distance = map.distance(
        candidate.x,
        candidate.y,
        mission.target_position.x,
        mission.target_position.y,
    );
    if candidate_distance < original_distance {
        ActionPriority::EmergencyAdvance
    } else {
        ActionPriority::Normal
    }
}

/// 新しいAI (V2/V3) 用の行動意思決定エンジン。
/// 各ユニットの所属部隊の割り当て目標（squad.target）に向かう接近スコアをベースに行動を決定します。
/// V3 の場合は #44 (低HP時の地形防御優先)・#45 (間接攻撃の待ち伏せ)・
/// #50 (間接砲火への露出ペナルティ) の戦術評価が追加されます。
pub fn decide_ai_action_v2(
    world: &mut World,
    player_id: PlayerId,
    skip_entities: &std::collections::HashSet<Entity>,
) -> Option<(Entity, AiCommand)> {
    // V3 の戦術評価 (#44/#45/#50) を有効にするかどうか
    let is_v3 = crate::ai::resolve_player_ai_version(world, player_id).uses_v3_tactics();

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
            if transporting_opt.is_some() {
                continue;
            }

            if !skip_entities.contains(&entity)
                && faction.0 == player_id
                && !has_moved.0
                && !action_completed.0
            {
                movable_units.push(entity);
            }

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

    // 2. SquadManager から各ユニットの所属部隊と目標を取得
    let manager = world
        .get_resource::<crate::ai::squad::SquadManager>()
        .cloned()
        .unwrap_or_default();
    let mut unit_squad_targets = HashMap::new();
    let mut solo_fallbacks = HashSet::new();

    for squad in &manager.squads {
        for &member in &squad.members {
            if let Some(target) = squad.target {
                unit_squad_targets.insert(member, target);
            }
        }
    }
    for &solo in &manager.solo_fallbacks {
        solo_fallbacks.insert(solo);
    }

    let map = world.resource::<Map>().clone();
    let registry = world.resource::<MasterDataRegistry>().clone();
    let properties: Vec<(GridPosition, Terrain, Option<PlayerId>)> = {
        let mut q = world.query::<(&GridPosition, &Property)>();
        q.iter(world)
            .map(|(p, prop)| (*p, prop.terrain, prop.owner_id))
            .collect()
    };
    let enemy_units: Vec<(
        GridPosition,
        crate::resources::UnitType,
        u32,
        u32,
        u32,
        u32,
        u32,
    )> = {
        let mut q = world.query::<(&GridPosition, &Faction, &UnitStats, &Health)>();
        q.iter(world)
            .filter(|(_, f, _, h)| f.0 != player_id && h.current > 0)
            .map(|(p, _, s, h)| {
                (
                    *p,
                    s.unit_type,
                    s.cost,
                    h.current,
                    s.min_range,
                    s.max_range,
                    s.max_movement,
                )
            })
            .collect()
    };
    let damage_chart = world.resource::<crate::resources::DamageChart>().clone();
    let emergency_plan = world
        .get_resource::<crate::ai::emergency::EmergencyMissionPlan>()
        .cloned()
        .unwrap_or_default();

    let mut turn_cache = TurnDistanceCache::default();
    let mut best_overall_rank = (ActionPriority::Normal, i32::MIN);
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

        let is_combat_ineffective = atk_hp < 70 || (stats.max_ammo1 > 0 && atk_ammo.0 == 0);
        let planned_emergency = emergency_plan.mission_for_entity(unit_entity);
        let emergency_mission = planned_emergency
            .filter(|mission| world.get_entity(mission.threat.threat_entity).is_ok());

        // #44 (V3): 敵の脅威がこのユニットの近傍にあるか (森・山への退避を
        // 意味のある局面に限定するためのゲート)。敵の攻撃到達圏 (移動+射程) を
        // 少し余裕をもって見た半径で判定する
        const THREAT_PROXIMITY_RADIUS: u32 = 8;
        let enemy_threat_nearby = enemy_units.iter().any(|(e_pos, _, _, _, _, _, _)| {
            (e_pos.x.abs_diff(pos.x) + e_pos.y.abs_diff(pos.y)) as u32 <= THREAT_PROXIMITY_RADIUS
        });

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

        let squad_target = if planned_emergency.is_some() && emergency_mission.is_none() {
            // 同一ターン中に対象が消失した場合は、古い迎撃座標のSquad加点を無効化する。
            None
        } else {
            unit_squad_targets.get(&unit_entity).copied()
        };
        let initial_is_solo = solo_fallbacks.contains(&unit_entity) || squad_target.is_none();

        // 評価ロジック（is_solo: initial_is_solo を直接使う）
        let is_solo = initial_is_solo;
        let mut best_unit_rank = (ActionPriority::Normal, i32::MIN);
        let mut best_unit_choice: Option<AiCommand> = None;

        for target_tile in &reachable {
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

            let mut base_tile_score = 0;
            let tile_def_bonus = map
                .get_terrain(current_grid.x, current_grid.y)
                .map(|t| registry.get_terrain_defense_bonus(t))
                .unwrap_or(0);
            base_tile_score += tile_def_bonus as i32 * 10;

            // #44 (V3): HP が低下しているほど防御地形 (森・山) への評価を引き上げ、
            // 生存率を高める位置取りを優先させる。
            // ただし敵の脅威が近くにある場合に限る (安全な後方でダメージを負った
            // ユニットが森を求めて無意味に引きこもり、前線合流が遅れるのを防ぐ)
            if is_v3 && atk_hp < 70 && enemy_threat_nearby {
                base_tile_score += tile_def_bonus as i32 * (100 - atk_hp as i32) * 2;
            }

            // #50 (V3): 敵攻撃ユニットの脅威圏 (脅威マップ) に入るタイルには
            // 期待被弾価値に応じた露出ペナルティを課す (間接=現在射程、
            // 直接=移動+攻撃到達圏)。撃破 (+5000) や占領 (+10000) など
            // リターンの大きい行動は行動側の加点によって自然に相殺される
            if is_v3 {
                base_tile_score -= crate::ai::threat::exposure_penalty(
                    &map,
                    (current_grid.x, current_grid.y),
                    stats.unit_type,
                    stats.cost,
                    atk_hp,
                    stats.min_range,
                    tile_def_bonus,
                    &enemy_units,
                    &damage_chart,
                );
            }

            // #45 (V3): 間接攻撃ユニットの待ち伏せポジショニング。
            // 射程内 (先制攻撃圏) や敵の接近を待ち受けられる位置での待機を加点し、
            // 最小射程より内側への不要な前進を減点する
            if is_v3 && stats.min_range > 1 && !is_combat_ineffective && !enemy_units.is_empty() {
                let mut nearest_enemy_dist = u32::MAX;
                for (e_pos, _, _, _, _, _, _) in &enemy_units {
                    let d = map.distance(e_pos.x, e_pos.y, current_grid.x, current_grid.y);
                    if d < nearest_enemy_dist {
                        nearest_enemy_dist = d;
                    }
                }
                if nearest_enemy_dist < stats.min_range {
                    base_tile_score -= AMBUSH_TOO_CLOSE_PENALTY;
                } else if nearest_enemy_dist <= stats.max_range {
                    base_tile_score += AMBUSH_IN_RANGE_BONUS;
                } else if nearest_enemy_dist <= stats.max_range + AMBUSH_APPROACH_MARGIN {
                    base_tile_score += AMBUSH_NEAR_RANGE_BONUS;
                }
            }

            // 1. 部隊目標への接近ボーナス
            if !is_solo {
                if let Some(target) = squad_target {
                    let turn_dist = calculate_turn_distance(
                        &map,
                        &registry,
                        &unit_positions,
                        (current_grid.x, current_grid.y),
                        (target.x, target.y),
                        stats.movement_type,
                        stats.max_movement,
                        stats.max_range,
                        player_id,
                        &mut turn_cache,
                    );
                    let m_dist = (current_grid.x as i32 - target.x as i32).abs()
                        + (current_grid.y as i32 - target.y as i32).abs();
                    let p_dist = m_dist as f32 / stats.max_movement as f32;
                    base_tile_score += (100 - turn_dist.turns as i32).max(0) * 1000;
                    base_tile_score += ((100.0 - p_dist).max(0.0) * 2000.0) as i32;
                }
            }

            // 2. SoloFallback / 孤立・戦闘不能のインセンティブ
            if is_solo {
                if is_combat_ineffective {
                    let mut min_score: Option<(crate::ai::turn_distance::TurnDistance, i32)> = None;
                    for (p_pos, p_terrain, p_owner) in &properties {
                        if *p_owner == Some(player_id)
                            && registry.can_repair_on_terrain(stats.unit_type, *p_terrain)
                        {
                            let d = calculate_turn_distance(
                                &map,
                                &registry,
                                &unit_positions,
                                (current_grid.x, current_grid.y),
                                (p_pos.x, p_pos.y),
                                stats.movement_type,
                                stats.max_movement,
                                0,
                                player_id,
                                &mut turn_cache,
                            );
                            let m = (current_grid.x as i32 - p_pos.x as i32).abs()
                                + (current_grid.y as i32 - p_pos.y as i32).abs();
                            let score = (d, m);
                            if min_score.map_or(true, |min| score < min) {
                                min_score = Some(score);
                            }
                        }
                    }
                    if let Some((d, m)) = min_score {
                        if d.turns < 99 {
                            let p = m as f32 / stats.max_movement as f32;
                            base_tile_score += (100 - d.turns as i32).max(0) * 1000;
                            base_tile_score += ((100.0 - p).max(0.0) * 2000.0) as i32;
                        }
                    }
                } else if !stats.can_capture {
                    // 健全な SoloFallback: 敵ユニットに接近する
                    let mut min_score: Option<(crate::ai::turn_distance::TurnDistance, i32)> = None;
                    for (e_pos, _, _, _, _, _, _) in &enemy_units {
                        let d = calculate_turn_distance(
                            &map,
                            &registry,
                            &unit_positions,
                            (current_grid.x, current_grid.y),
                            (e_pos.x, e_pos.y),
                            stats.movement_type,
                            stats.max_movement,
                            stats.max_range,
                            player_id,
                            &mut turn_cache,
                        );
                        let m = (current_grid.x as i32 - e_pos.x as i32).abs()
                            + (current_grid.y as i32 - e_pos.y as i32).abs();
                        let score = (d, m);
                        if min_score.map_or(true, |min| score < min) {
                            min_score = Some(score);
                        }
                    }
                    if let Some((d, m)) = min_score {
                        if d.turns < 99 {
                            let p = m as f32 / stats.max_movement as f32;
                            base_tile_score += (100 - d.turns as i32).max(0) * 1000;
                            base_tile_score += ((100.0 - p).max(0.0) * 2000.0) as i32;
                        }
                    }
                }
            }

            // (A) タクシー帰りロジック
            let is_empty_transport = stats.max_cargo > 0
                && world
                    .get::<crate::components::CargoCapacity>(unit_entity)
                    .is_some_and(|c| c.loaded.is_empty());

            if is_empty_transport {
                let mut min_score: Option<(crate::ai::turn_distance::TurnDistance, i32)> = None;
                for (p_pos, p_terrain, p_owner) in &properties {
                    if *p_owner == Some(player_id)
                        && registry.is_production_facility(p_terrain.as_str())
                    {
                        let d = calculate_turn_distance(
                            &map,
                            &registry,
                            &unit_positions,
                            (current_grid.x, current_grid.y),
                            (p_pos.x, p_pos.y),
                            stats.movement_type,
                            stats.max_movement,
                            0,
                            player_id,
                            &mut turn_cache,
                        );
                        let m = (current_grid.x as i32 - p_pos.x as i32).abs()
                            + (current_grid.y as i32 - p_pos.y as i32).abs();
                        let score = (d, m);
                        if min_score.map_or(true, |min| score < min) {
                            min_score = Some(score);
                        }
                    }
                }
                if let Some((d, m)) = min_score {
                    if d.turns < 99 {
                        let p = m as f32 / stats.max_movement as f32;
                        base_tile_score += (100 - d.turns as i32).max(0) * 1000;
                        base_tile_score += ((100.0 - p).max(0.0) * 2000.0) as i32;
                    }
                }
            }

            // (B) 歩兵の待機移動ロジック
            // 注: 座礁した戦闘車両にも海岸移動を適用する実験を行ったが、
            // 全ユニットが海岸に密集して海峡越しに交戦誤判定 (is_engaged は
            // 海を無視したマンハッタン距離) を誘発し、フェーズが Contested に
            // 固定されて拡張が停止する退行が観測されたため、歩兵限定に戻した
            let is_infantry = stats.unit_type == crate::resources::UnitType::Infantry
                || stats.unit_type == crate::resources::UnitType::Mech;
            if is_infantry
                && !is_combat_ineffective
                && is_unit_stranded(world, &pos, player_id, &properties, &enemy_units)
            {
                let mut min_coast_dist = u32::MAX;
                let check_range = 10;
                let min_x = current_grid.x.saturating_sub(check_range);
                let max_x = (current_grid.x + check_range).min(map.width - 1);
                let min_y = current_grid.y.saturating_sub(check_range);
                let max_y = (current_grid.y + check_range).min(map.height - 1);

                for cy in min_y..=max_y {
                    for cx in min_x..=max_x {
                        if map.get_terrain(cx, cy) == Some(crate::resources::Terrain::Sea) {
                            let d = calculate_turn_distance(
                                &map,
                                &registry,
                                &unit_positions,
                                (current_grid.x, current_grid.y),
                                (cx, cy),
                                stats.movement_type,
                                stats.max_movement,
                                0,
                                player_id,
                                &mut turn_cache,
                            );
                            if d.turns < min_coast_dist {
                                min_coast_dist = d.turns;
                            }
                        }
                    }
                }
                if min_coast_dist < 99 && min_coast_dist > 0 {
                    base_tile_score += (100 - min_coast_dist as i32).max(0) * 100;
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

            // #53 (V3): 部隊に所属する占領ユニットは部隊目標への接近のみに従う。
            // 汎用の「最寄り非所有拠点への引力」は部隊目標と同じ重みを持つため、
            // これを併用すると常に最寄りの前線都市へ引き戻され、
            // 後方の敵生産施設を目標とする部隊が機能しなくなる
            if effective_can_capture && (!is_v3 || is_solo) {
                let mut min_score: Option<(crate::ai::turn_distance::TurnDistance, i32)> = None;
                for (p_pos, _p_terrain, p_owner) in &properties {
                    if *p_owner != Some(player_id) {
                        let d = calculate_turn_distance(
                            &map,
                            &registry,
                            &unit_positions,
                            (current_grid.x, current_grid.y),
                            (p_pos.x, p_pos.y),
                            stats.movement_type,
                            stats.max_movement,
                            stats.max_range,
                            player_id,
                            &mut turn_cache,
                        );
                        let m = (current_grid.x as i32 - p_pos.x as i32).abs()
                            + (current_grid.y as i32 - p_pos.y as i32).abs();
                        let score = (d, m);
                        if min_score.map_or(true, |min| score < min) {
                            min_score = Some(score);
                        }
                    }
                }
                if let Some((d, m)) = min_score {
                    if d.turns < 99 {
                        let p = m as f32 / stats.max_movement as f32;
                        base_tile_score += (100 - d.turns as i32).max(0) * 1000;
                        base_tile_score += ((100.0 - p).max(0.0) * 2000.0) as i32;
                    }
                }
            } else if is_solo {
                // Fallback: 敵に近づく
                let mut best_target_dist: i32 = 999;
                let mut best_target_pos = None;
                let mut max_potential = -1.0;

                for (e_pos, e_type, e_cost, e_hp, _, _, _) in &enemy_units {
                    let mut effective_dist = calculate_turn_distance(
                        &map,
                        &registry,
                        &unit_positions,
                        (current_grid.x, current_grid.y),
                        (e_pos.x, e_pos.y),
                        stats.movement_type,
                        stats.max_movement,
                        stats.max_range,
                        player_id,
                        &mut turn_cache,
                    );

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
                            effective_dist.turns += 20;
                        }
                    }

                    let base_dmg = damage_chart
                        .get_base_damage(stats.unit_type, *e_type)
                        .or_else(|| {
                            damage_chart.get_base_damage_secondary(stats.unit_type, *e_type)
                        })
                        .unwrap_or(0);

                    let potential =
                        base_dmg as f32 * (*e_cost as f32 / 100.0) * (2.0 - *e_hp as f32 / 100.0);

                    if potential > max_potential {
                        max_potential = potential;
                        best_target_dist = effective_dist.turns as i32;
                        best_target_pos = Some(*e_pos);
                    } else if (potential - max_potential).abs() < 0.1
                        && (effective_dist.turns as i32) < best_target_dist
                    {
                        best_target_dist = effective_dist.turns as i32;
                        best_target_pos = Some(*e_pos);
                    }
                }

                if max_potential <= 0.0 {
                    let mut min_score: Option<(crate::ai::turn_distance::TurnDistance, i32)> = None;
                    for (e_pos, _, _, _, _, _, _) in &enemy_units {
                        let mut d = calculate_turn_distance(
                            &map,
                            &registry,
                            &unit_positions,
                            (current_grid.x, current_grid.y),
                            (e_pos.x, e_pos.y),
                            stats.movement_type,
                            stats.max_movement,
                            stats.max_range,
                            player_id,
                            &mut turn_cache,
                        );

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
                                d.turns += 20;
                            }
                        }

                        let m = (current_grid.x as i32 - e_pos.x as i32).abs()
                            + (current_grid.y as i32 - e_pos.y as i32).abs();
                        let score = (d, m);

                        if min_score.map_or(true, |min| score < min) {
                            min_score = Some(score);
                            best_target_pos = Some(*e_pos);
                        }
                    }
                    if min_score.is_none() || min_score.unwrap().0.turns >= 99 {
                        for (p_pos, _, p_owner) in &properties {
                            if *p_owner != Some(player_id) {
                                let d = calculate_turn_distance(
                                    &map,
                                    &registry,
                                    &unit_positions,
                                    (current_grid.x, current_grid.y),
                                    (p_pos.x, p_pos.y),
                                    stats.movement_type,
                                    stats.max_movement,
                                    0,
                                    player_id,
                                    &mut turn_cache,
                                );
                                let m = (current_grid.x as i32 - p_pos.x as i32).abs()
                                    + (current_grid.y as i32 - p_pos.y as i32).abs();
                                let score = (d, m);

                                if min_score.map_or(true, |min| score < min) {
                                    min_score = Some(score);
                                    best_target_pos = Some(*p_pos);
                                }
                            }
                        }
                    }
                    if let Some((d, m)) = min_score {
                        best_target_dist = d.turns as i32;
                        if d.turns < 99 {
                            let p = m as f32 / stats.max_movement as f32;
                            base_tile_score += (100 - d.turns as i32).max(0) * 1000;
                            base_tile_score += ((100.0 - p).max(0.0) * 2000.0) as i32;
                        }
                    }
                }

                if stats.min_range > 1 {
                    if let Some(t_pos) = best_target_pos {
                        let m_dist = (current_grid.x as i32 - t_pos.x as i32).abs()
                            + (current_grid.y as i32 - t_pos.y as i32).abs();
                        if m_dist >= stats.min_range as i32 && m_dist <= stats.max_range as i32 {
                            // 射程内に入った！絶好のポジション
                            base_tile_score += 10000;
                        } else if m_dist < stats.min_range as i32 {
                            // 近すぎる！ペナルティ
                            base_tile_score -= 2000;
                        } else {
                            // まだ遠い。ターン距離が短いほど良い
                            base_tile_score += (100 - best_target_dist).max(0) * 100;
                        }
                    } else {
                        base_tile_score += (100 - best_target_dist).max(0) * 100;
                    }
                } else {
                    base_tile_score += (100 - best_target_dist).max(0) * 100;
                }
            }

            // (A) Capture
            if actions.can_capture {
                let score = base_tile_score + 10000;
                let rank = (
                    emergency_position_priority(emergency_mission, pos, current_grid, &map),
                    score,
                );
                if rank > best_unit_rank {
                    best_unit_rank = rank;
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
                        // 撃破判定・ダメージ期待値の算出
                        let t_terrain = map
                            .get_terrain(t_pos.x, t_pos.y)
                            .unwrap_or(crate::resources::Terrain::Plains);
                        let def_bonus = registry.get_terrain_defense_bonus(t_terrain);
                        let dist = (current_grid.x as i64 - t_pos.x as i64).unsigned_abs() as u32
                            + (current_grid.y as i64 - t_pos.y as i64).unsigned_abs() as u32;

                        let expected_actual_damage = crate::systems::combat::get_expected_damage(
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

                        // 期待ダメージが0の場合は攻撃候補から外す
                        if expected_actual_damage == 0 {
                            continue;
                        }

                        let mut attack_score = 2000;
                        let damage_val = (expected_actual_damage * t_stats.cost) / 100;
                        attack_score += damage_val as i32;

                        if is_combat_ineffective && expected_actual_damage < t_health.current {
                            attack_score -= 3000;
                        }

                        if expected_actual_damage >= t_health.current {
                            attack_score += 5000;
                        }

                        let score = base_tile_score + attack_score;
                        let priority = if emergency_mission
                            .is_some_and(|mission| mission.threat.threat_entity == target_entity)
                        {
                            ActionPriority::EmergencyNeutralization
                        } else {
                            ActionPriority::Normal
                        };
                        let rank = (priority, score);
                        if rank > best_unit_rank {
                            best_unit_rank = rank;
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
                        score += 8000;
                    } else if atk_hp < 100 || atk_ammo.0 < stats.max_ammo1 {
                        score += 1000;
                    } else {
                        // 回復の必要がないのに生産施設や回復施設の上にいる場合は、
                        // 施設の生産ラインを塞がないようにペナルティを与えてどかせる
                        score -= 2000;
                    }
                } else if is_combat_ineffective {
                    score -= 5000;
                }

                let rank = (
                    emergency_position_priority(emergency_mission, pos, current_grid, &map),
                    score,
                );
                if rank > best_unit_rank {
                    best_unit_rank = rank;
                    best_unit_choice = Some(AiCommand::Wait {
                        target_pos: current_grid,
                    });
                }
            }

            // (D) Merge
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
                        if crate::ai::pruning::is_overflow_merge_without_refund(atk_hp, *t_health) {
                            continue;
                        }

                        let total_hp = atk_hp + t_health.current;
                        if is_combat_ineffective || t_health.current < 40 {
                            merge_score += 4000;
                        }
                        if total_hp <= t_health.max {
                            merge_score += 1000;
                        }

                        let score = base_tile_score + merge_score;
                        let rank = (ActionPriority::Normal, score);
                        if rank > best_unit_rank {
                            best_unit_rank = rank;
                            best_unit_choice = Some(AiCommand::Merge {
                                target_pos: current_grid,
                                target_entity,
                            });
                        }
                    }
                }
            }
        }

        if let Some(choice) = best_unit_choice {
            if best_unit_rank > best_overall_rank {
                best_overall_rank = best_unit_rank;
                best_overall_choice = Some((unit_entity, choice));
            }
        }
    }

    if let Some((entity, ref command)) = best_overall_choice {
        let mission_type = manager
            .squads
            .iter()
            .find(|s| s.members.contains(&entity))
            .map(|s| format!("{:?}", s.mission_type))
            .unwrap_or_else(|| {
                if manager.solo_fallbacks.contains(&entity) {
                    "SoloFallback".to_string()
                } else {
                    "Unknown".to_string()
                }
            });

        // AIが決定した最善の行動評価情報をイベントとして送出する
        if let Some(mut events) =
            world.get_resource_mut::<Events<crate::events::AiActionEvaluatedEvent>>()
        {
            events.send(crate::events::AiActionEvaluatedEvent {
                entity,
                mission_type,
                action_type: format!("{:?}", command),
                score: best_overall_rank.1,
            });
        }
    }

    best_overall_choice
}

fn is_unit_stranded(
    world: &World,
    pos: &GridPosition,
    player_id: PlayerId,
    properties: &[(GridPosition, crate::resources::Terrain, Option<PlayerId>)],
    enemy_units: &[(
        GridPosition,
        crate::resources::UnitType,
        u32,
        u32,
        u32,
        u32,
        u32,
    )],
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
        for (e_pos, _, _, _, _, _, _) in enemy_units {
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
    fn drop_keeps_transport_actionable_until_last_ready_cargo() {
        let mut world = World::new();
        let first = world.spawn(crate::components::ActionCompleted(false)).id();
        let second = world.spawn(crate::components::ActionCompleted(false)).id();
        let transport = world
            .spawn(crate::components::CargoCapacity {
                max: 2,
                loaded: vec![first, second],
            })
            .id();

        assert!(transport_has_other_actionable_cargo(
            &world, transport, first
        ));
        world
            .get_mut::<crate::components::ActionCompleted>(second)
            .unwrap()
            .0 = true;
        assert!(!transport_has_other_actionable_cargo(
            &world, transport, first
        ));
    }

    #[test]
    fn v3_turn_cache_marks_squad_plan_until_cleared() {
        let player = PlayerId(1);
        let mut cache = AiTurnStrategyCache::default();

        assert!(!cache.squads_planned(player));
        cache.mark_squads_planned(player);
        assert!(cache.squads_planned(player));

        cache.clear();
        assert!(!cache.squads_planned(player));
    }

    #[test]
    fn plan_squads_populates_v3_turn_strategy_cache() {
        let master_data = crate::resources::master_data::MasterDataRegistry::load().unwrap();
        let (mut world, _) =
            crate::setup::initialize_world_from_master_data(&master_data, "map_1").unwrap();
        let entities: Vec<Entity> = world.query::<Entity>().iter(&world).collect();
        for entity in entities {
            world.despawn(entity);
        }
        let player = PlayerId(1);
        let mut settings = crate::ai::PlayerAiSettings::default();
        settings.set_version(player, crate::ai::AiVersion::V3);
        world.insert_resource(settings);

        crate::ai::squad::plan_squads(&mut world, player);

        let cache = world.resource::<AiTurnStrategyCache>();
        assert!(cache.squads_planned(player));
        assert!(cache.campaign_portfolio(player).is_some());
    }

    #[test]
    fn v3_campaign_production_plan_consumes_each_command_once() {
        let player = PlayerId(1);
        let first = crate::events::ProduceUnitCommand {
            player_id: player,
            target_x: 1,
            target_y: 2,
            unit_type: UnitType::Infantry,
        };
        let second = crate::events::ProduceUnitCommand {
            player_id: player,
            target_x: 3,
            target_y: 4,
            unit_type: UnitType::Mech,
        };
        let mut cache = AiTurnStrategyCache::default();

        cache.set_campaign_production_plan(player, vec![first, second], true);

        assert!(cache.campaign_production_planned(player));
        let actual_first = cache.take_campaign_production_command(player).unwrap();
        let actual_second = cache.take_campaign_production_command(player).unwrap();
        assert_eq!(
            (
                actual_first.target_x,
                actual_first.target_y,
                actual_first.unit_type,
            ),
            (1, 2, UnitType::Infantry)
        );
        assert_eq!(
            (
                actual_second.target_x,
                actual_second.target_y,
                actual_second.unit_type,
            ),
            (3, 4, UnitType::Mech)
        );
        assert!(cache.take_campaign_production_command(player).is_none());
        assert!(!cache.campaign_production_blocks_generic(player));
    }

    #[test]
    fn incomplete_v3_campaign_production_plan_blocks_generic_fallback() {
        let player = PlayerId(1);
        let mut cache = AiTurnStrategyCache::default();

        cache.set_campaign_production_plan(player, Vec::new(), false);

        assert!(cache.campaign_production_planned(player));
        assert!(cache.campaign_production_blocks_generic(player));
    }

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
        if let AiCommand::Load { .. } = cmd {
            panic!("Expected Load command to be completely removed from normal decision making")
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
        if let AiCommand::Drop { .. } = cmd {
            panic!("Expected Drop command to be completely removed from normal decision making")
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
    fn issue73_v1_overflow_merge_is_not_selected() {
        let mut world = World::new();
        world.insert_resource(DamageChart::new());
        world.insert_resource(Map {
            width: 3,
            height: 1,
            tiles: vec![Terrain::Plains, Terrain::Forest, Terrain::Plains],
            topology: crate::resources::GridTopology::Square,
        });
        crate::resources::master_data::MasterDataRegistry::load()
            .map(|master_data| world.insert_resource(master_data))
            .unwrap();

        let player = PlayerId(1);
        world.spawn((
            player,
            Faction(player),
            HasMoved(false),
            ActionCompleted(false),
            GridPosition { x: 0, y: 0 },
            UnitStats {
                unit_type: UnitType::Infantry,
                max_movement: 3,
                movement_type: crate::resources::MovementType::Infantry,
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
        let target = world
            .spawn((
                player,
                Faction(player),
                HasMoved(true),
                ActionCompleted(true),
                GridPosition { x: 1, y: 0 },
                UnitStats {
                    unit_type: UnitType::Infantry,
                    max_movement: 3,
                    movement_type: crate::resources::MovementType::Infantry,
                    ..UnitStats::mock()
                },
                Health {
                    current: 34,
                    max: 100,
                },
                crate::components::Fuel {
                    current: 99,
                    max: 99,
                },
            ))
            .id();

        let action = decide_ai_action(&mut world, player, &std::collections::HashSet::new());

        assert!(
            !matches!(
                action,
                Some((_, AiCommand::Merge { target_entity, .. })) if target_entity == target
            ),
            "HP上限を超えるMergeはV1の候補から除外されること"
        );
    }

    #[test]
    fn issue73_v3_position_score_does_not_revive_overflow_merge() {
        let mut world = setup_v3_test_world(3, crate::ai::AiVersion::V3);
        world.insert_resource(DamageChart::new());
        let player = PlayerId(1);
        let stats = UnitStats {
            unit_type: UnitType::Infantry,
            max_movement: 3,
            movement_type: crate::resources::MovementType::Infantry,
            ..UnitStats::mock()
        };
        let source = spawn_v3_test_unit(&mut world, player, 0, 100, stats.clone());
        let target = world
            .spawn((
                Faction(player),
                HasMoved(true),
                ActionCompleted(true),
                GridPosition { x: 1, y: 0 },
                stats,
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
        insert_single_unit_squad(&mut world, source, GridPosition { x: 1, y: 0 });

        let action = decide_ai_action_v2(&mut world, player, &std::collections::HashSet::new());

        assert!(
            !matches!(
                action,
                Some((entity, AiCommand::Merge { target_entity, .. }))
                    if entity == source && target_entity == target
            ),
            "部隊目標による大きな位置スコアがあってもHP超過Mergeを復活させないこと"
        );
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

    #[test]
    fn test_is_unit_stranded_coast_attraction() {
        let mut world = World::new();
        let mut damage_chart = DamageChart::new();
        damage_chart.insert_damage(UnitType::Infantry, UnitType::Infantry, 55);
        world.insert_resource(damage_chart);

        // 5x5のマップ。左上の3x3が陸地、それ以外は海
        // (0,0) ~ (2,2) は Plains、それ以外は Sea
        let mut tiles = vec![Terrain::Sea; 25];
        for y in 0..3 {
            for x in 0..3 {
                tiles[y * 5 + x] = Terrain::Plains;
            }
        }

        let map = Map {
            width: 5,
            height: 5,
            tiles,
            topology: crate::resources::GridTopology::Square,
        };
        let island_map = crate::ai::islands::IslandMap::analyze(&map);
        world.insert_resource(map);
        world.insert_resource(island_map);

        crate::resources::master_data::MasterDataRegistry::load()
            .map(|m| world.insert_resource(m))
            .unwrap();

        let p1 = PlayerId(1);
        let p2 = PlayerId(2);

        // 孤立した歩兵を (1,1) に配置。周囲は海に接する Plains
        let infantry = world
            .spawn((
                p1,
                Faction(p1),
                HasMoved(false),
                ActionCompleted(false),
                GridPosition { x: 1, y: 1 },
                UnitStats {
                    unit_type: UnitType::Infantry,
                    cost: 1000,
                    max_movement: 1, // 移動力1
                    movement_type: crate::resources::MovementType::Infantry,
                    can_capture: true,
                    min_range: 1,
                    max_range: 1,
                    max_ammo1: 10,
                    max_ammo2: 10,
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
            ))
            .id();

        // 1. 敵や建物が他にない場合（孤立状態）
        // (1,1) にいる歩兵は海に隣接するマス（例: (0,1), (1,0), (1,2), (2,1)）のいずれかに移動して待機するはず。
        // なぜなら (1,1) は海に隣接しておらず、海までの距離が2だが、
        // 周囲4マスは海に隣接しており距離1だからである。
        let skips = std::collections::HashSet::new();
        let action = decide_ai_action(&mut world, p1, &skips);
        assert!(action.is_some());
        if let Some((entity, AiCommand::Wait { target_pos })) = action {
            assert_eq!(entity, infantry);
            // (1,1) のままでなく、海に面した隣接マスのいずれかに移動していることを確認
            let dist = (target_pos.x as i32 - 1).abs() + (target_pos.y as i32 - 1).abs();
            assert_eq!(dist, 1); // 隣接マスへ移動
            assert!(target_pos.x < 3 && target_pos.y < 3); // かつ陸地の中
        } else {
            panic!("Expected Wait at coast tile, got {:?}", action);
        }

        // 2. 同じ島に敵ユニットを配置した場合（孤立していない状態）
        // 敵ユニットを (0,0) に配置
        let enemy = world
            .spawn((
                p2,
                Faction(p2),
                HasMoved(false),
                ActionCompleted(false),
                GridPosition { x: 0, y: 0 },
                UnitStats {
                    unit_type: UnitType::Infantry,
                    cost: 1000,
                    max_movement: 3,
                    movement_type: crate::resources::MovementType::Infantry,
                    max_ammo1: 10,
                    max_ammo2: 10,
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
            ))
            .id();

        // これにより、島には敵がいるため、is_unit_stranded は false になるはず。
        // 歩兵は敵を攻撃しようとするはず。
        // (1,1) から (0,1) または (1,0) に移動して (0,0) の敵を攻撃するコマンドになるはず。
        let action2 = decide_ai_action(&mut world, p1, &skips);
        assert!(action2.is_some());
        if let Some((
            entity,
            AiCommand::Attack {
                target_pos,
                target_entity,
            },
        )) = action2
        {
            assert_eq!(entity, infantry);
            assert!(
                (target_pos.x == 0 && target_pos.y == 1)
                    || (target_pos.x == 1 && target_pos.y == 0)
            );
            assert_eq!(target_entity, enemy);
        } else {
            panic!("Expected Attack command on enemy, got {:?}", action2);
        }
    }

    #[test]
    #[allow(deprecated)]
    fn test_ai_mission_priority_and_cooldown() {
        let mut world = World::new();
        world.insert_resource(DamageChart::new());

        // 5x5の平地マップ
        let map = Map {
            width: 5,
            height: 5,
            tiles: vec![Terrain::Plains; 25],
            topology: crate::resources::GridTopology::Square,
        };
        let island_map = crate::ai::islands::IslandMap::analyze(&map);
        world.insert_resource(map);
        world.insert_resource(island_map);
        crate::resources::master_data::MasterDataRegistry::load()
            .map(|m| world.insert_resource(m))
            .unwrap();

        // 必要なイベントリソースを登録
        world.insert_resource(Events::<crate::events::MoveUnitCommand>::default());
        world.insert_resource(Events::<crate::events::WaitUnitCommand>::default());
        world.insert_resource(Events::<crate::events::NextPhaseCommand>::default());

        let p1 = PlayerId(1);
        let mut ai_settings = crate::ai::ai_version::PlayerAiSettings::new();
        ai_settings.set_version(p1, crate::ai::ai_version::AiVersion::V1);
        world.insert_resource(ai_settings);

        // 1. 輸送機(ヘリ)を(0,0)に配置
        let heli = world
            .spawn((
                p1,
                Faction(p1),
                HasMoved(false),
                ActionCompleted(false),
                GridPosition { x: 0, y: 0 },
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
            ))
            .id();

        // 2. 歩兵を(3,0)に配置
        let infantry = world
            .spawn((
                p1,
                Faction(p1),
                HasMoved(false),
                ActionCompleted(false),
                GridPosition { x: 3, y: 0 },
                UnitStats {
                    unit_type: UnitType::Infantry,
                    cost: 1000,
                    max_movement: 3,
                    movement_type: crate::resources::MovementType::Infantry,
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

        // 3. ミッションを登録する
        // phase: Pickup, transport: heli, cargo: infantry
        let mission = crate::ai::missions::TransportMission {
            transport_entity: heli,
            cargo_entity: infantry,
            phase: crate::ai::missions::TransportPhase::Pickup,
            target_island: None,
        };
        let mut manager = crate::ai::missions::TransportMissionManager::default();
        manager.missions.push(mission);
        world.insert_resource(manager);

        // クールダウン用のリソースを登録
        world.insert_resource(AiActionCooldown(std::collections::HashSet::new()));

        // 4. execute_ai_turn を呼び出す (1回目)
        // ミッション優先実行により、ヘリが歩兵(3,0)へ向かうコマンドが実行され、Someが返るはず。
        let result1 = execute_ai_turn(&mut world, p1);
        assert!(result1.is_some());

        // 5. ヘリが AiActionCooldown に追加されていることを確認
        let cooldown = world.get_resource::<AiActionCooldown>().unwrap();
        assert!(cooldown.0.contains(&heli));

        // イベントが送られていることを確認
        let move_events = world
            .get_resource::<Events<crate::events::MoveUnitCommand>>()
            .unwrap();
        let mut reader = move_events.get_reader();
        let sent_move = reader.read(move_events).next();
        assert!(sent_move.is_some());
        let move_cmd = sent_move.unwrap();
        assert_eq!(move_cmd.unit_entity, heli);
        // ヘリが (0, 0) から右方向 (x > 0) の歩兵 (3, 0) に向けて移動を開始したことを検証する
        assert!(move_cmd.target_x > 0 && move_cmd.target_x < 5);
        assert!(move_cmd.target_y < 5);

        // 6. 同一ターン内での2回目の execute_ai_turn 呼び出し
        // ヘリは cooldown のため無視される。
        let _result2 = execute_ai_turn(&mut world, p1);

        // cooldown リソースを確認し、ヘリがクールダウン中に留まっていること
        let cooldown2 = world.get_resource::<AiActionCooldown>().unwrap();
        assert!(cooldown2.0.contains(&heli));
    }

    /// V3 テスト用の共通ワールドを構築するヘルパー。
    /// 幅 width x 高さ 1 の平原マップと必要リソースを登録する。
    fn setup_v3_test_world(width: usize, version: crate::ai::ai_version::AiVersion) -> World {
        let mut world = World::new();
        world.insert_resource(Map {
            width,
            height: 1,
            tiles: vec![Terrain::Plains; width],
            topology: crate::resources::GridTopology::Square,
        });
        crate::resources::master_data::MasterDataRegistry::load()
            .map(|m| world.insert_resource(m))
            .unwrap();
        let mut settings = crate::ai::ai_version::PlayerAiSettings::new();
        settings.set_version(PlayerId(1), version);
        settings.set_version(PlayerId(2), version);
        world.insert_resource(settings);
        world
    }

    #[test]
    fn v2_v3_transport_executor_skips_foreign_owned_squads() {
        for version in [
            crate::ai::ai_version::AiVersion::V2,
            crate::ai::ai_version::AiVersion::V3,
        ] {
            let mut world = setup_v3_test_world(5, version);
            let map = world.resource::<Map>().clone();
            world.insert_resource(crate::ai::islands::IslandMap::analyze(&map));
            world.insert_resource(Events::<crate::events::WaitUnitCommand>::default());
            world.insert_resource(Events::<crate::events::MoveUnitCommand>::default());
            let player_a = PlayerId(1);
            let player_b = PlayerId(2);
            let property_a = GridPosition { x: 1, y: 0 };
            let property_b = GridPosition { x: 3, y: 0 };
            world.spawn((
                property_a,
                Property::new(Terrain::City, Some(player_a), 100),
            ));
            world.spawn((
                property_b,
                Property::new(Terrain::City, Some(player_b), 100),
            ));
            let transport_stats = UnitStats {
                unit_type: UnitType::TransportHelicopter,
                movement_type: crate::resources::MovementType::Air,
                max_movement: 6,
                max_cargo: 2,
                ..UnitStats::mock()
            };
            let transport_a = world
                .spawn((
                    Faction(player_a),
                    GridPosition { x: 0, y: 0 },
                    transport_stats.clone(),
                    crate::components::Fuel {
                        current: 99,
                        max: 99,
                    },
                    crate::components::CargoCapacity {
                        max: 2,
                        loaded: Vec::new(),
                    },
                ))
                .id();
            let ownerless_transport = world
                .spawn((
                    Faction(player_b),
                    GridPosition { x: 2, y: 0 },
                    transport_stats.clone(),
                    crate::components::Fuel {
                        current: 99,
                        max: 99,
                    },
                    crate::components::CargoCapacity {
                        max: 2,
                        loaded: Vec::new(),
                    },
                ))
                .id();
            let transport_b = world
                .spawn((
                    Faction(player_b),
                    GridPosition { x: 4, y: 0 },
                    transport_stats,
                    crate::components::Fuel {
                        current: 99,
                        max: 99,
                    },
                    crate::components::CargoCapacity {
                        max: 2,
                        loaded: Vec::new(),
                    },
                ))
                .id();
            let (foreign_id, foreign_snapshot) = {
                let mut manager = crate::ai::squad::SquadManager::new();
                let foreign =
                    manager.create_owned_squad(crate::ai::squad::MissionType::Transport, player_a);
                foreign.members.insert(transport_a);
                foreign.transport_entity = Some(transport_a);
                foreign.phase = crate::ai::squad::MissionPhase::Transport(
                    crate::ai::squad::TransportPhase::Return,
                );
                let snapshot = (
                    foreign.owner_id,
                    foreign.members.clone(),
                    foreign.transport_entity,
                    foreign.cargo_entities.clone(),
                    foreign.delivered_cargo.clone(),
                    foreign.target_island,
                    foreign.target,
                    foreign.phase.clone(),
                    foreign.pickup_position,
                    foreign.drop_position,
                );
                let id = foreign.id;
                let ownerless = manager.create_squad(crate::ai::squad::MissionType::Transport);
                ownerless.members.insert(ownerless_transport);
                ownerless.transport_entity = Some(ownerless_transport);
                ownerless.phase = crate::ai::squad::MissionPhase::Transport(
                    crate::ai::squad::TransportPhase::Return,
                );
                let own =
                    manager.create_owned_squad(crate::ai::squad::MissionType::Transport, player_b);
                own.members.insert(transport_b);
                own.transport_entity = Some(transport_b);
                own.phase = crate::ai::squad::MissionPhase::Transport(
                    crate::ai::squad::TransportPhase::Return,
                );
                world.insert_resource(manager);
                (id, snapshot)
            };
            let sentinel = world.spawn_empty().id();
            world.insert_resource(AiActionCooldown(HashSet::from([sentinel])));

            let result_b = execute_ai_turn(&mut world, player_b);
            assert!(result_b.is_some());
            let manager = world.resource::<crate::ai::squad::SquadManager>();
            let foreign = manager
                .squads
                .iter()
                .find(|squad| squad.id == foreign_id)
                .unwrap();
            assert_eq!(
                (
                    foreign.owner_id,
                    foreign.members.clone(),
                    foreign.transport_entity,
                    foreign.cargo_entities.clone(),
                    foreign.delivered_cargo.clone(),
                    foreign.target_island,
                    foreign.target,
                    foreign.phase.clone(),
                    foreign.pickup_position,
                    foreign.drop_position,
                ),
                foreign_snapshot
            );
            let cooldown = world.resource::<AiActionCooldown>();
            assert!(!cooldown.0.contains(&transport_a));
            assert!(!cooldown.0.contains(&ownerless_transport));
            assert!(cooldown.0.contains(&transport_b));

            let result_a = execute_ai_turn(&mut world, player_a);
            assert!(result_a.is_some());
            assert!(
                world
                    .resource::<AiActionCooldown>()
                    .0
                    .contains(&transport_a)
            );
        }
    }

    /// 移動可能な自軍ユニットをスポーンするヘルパー
    fn spawn_v3_test_unit(
        world: &mut World,
        player: PlayerId,
        x: usize,
        hp: u32,
        stats: UnitStats,
    ) -> Entity {
        world
            .spawn((
                Faction(player),
                HasMoved(false),
                ActionCompleted(false),
                GridPosition { x, y: 0 },
                stats,
                Health {
                    current: hp,
                    max: 100,
                },
                crate::components::Fuel {
                    current: 99,
                    max: 99,
                },
            ))
            .id()
    }

    /// 指定ユニット1体のみからなる部隊 (目標つき) を登録するヘルパー
    fn insert_single_unit_squad(world: &mut World, member: Entity, target: GridPosition) {
        let mut manager = crate::ai::squad::SquadManager::default();
        let mut members = std::collections::BTreeSet::new();
        members.insert(member);
        manager.squads.push(crate::ai::squad::Squad {
            id: crate::ai::squad::SquadId(1),
            owner_id: None,
            members,
            mission_type: crate::ai::squad::MissionType::Attack,
            target: Some(target),
            target_island: None,
            phase: crate::ai::squad::MissionPhase::MovingToTarget,
            transport_entity: None,
            cargo_entities: Vec::new(),
            pickup_position: None,
            drop_position: None,
            delivered_cargo: Vec::new(),
            return_after_combat: false,
        });
        world.insert_resource(manager);
    }

    /// Issue #50: V3 は敵間接攻撃ユニットの射程 (脅威マップ) 内への
    /// 前進を避け、V2 は露出を考慮せず前進することを検証する
    #[test]
    fn test_v3_avoids_indirect_fire_exposure() {
        use crate::ai::ai_version::AiVersion;

        let run = |version: AiVersion| -> (usize, usize) {
            let mut world = setup_v3_test_world(12, version);
            let mut dc = DamageChart::new();
            // 重自走砲 (射程3-5) は軽戦車に大ダメージ、軽戦車の反対方向は中程度
            dc.insert_damage(UnitType::HeavySpGun, UnitType::Tank, 92);
            dc.insert_damage(UnitType::Tank, UnitType::HeavySpGun, 43);
            world.insert_resource(dc);

            // 自軍: 軽戦車 (移動4) at x=0
            let tank = spawn_v3_test_unit(
                &mut world,
                PlayerId(1),
                0,
                100,
                UnitStats {
                    unit_type: UnitType::Tank,
                    cost: 6000,
                    max_movement: 4,
                    movement_type: crate::resources::MovementType::Tank,
                    min_range: 1,
                    max_range: 1,
                    max_fuel: 99,
                    ..UnitStats::mock()
                },
            );
            let _ = tank;

            // 敵軍: 重自走砲 (射程3-5) at x=9 -> 脅威ゾーンは x in [4,6]
            world.spawn((
                Faction(PlayerId(2)),
                HasMoved(true),
                ActionCompleted(true),
                GridPosition { x: 9, y: 0 },
                UnitStats {
                    unit_type: UnitType::HeavySpGun,
                    cost: 16500,
                    max_movement: 5,
                    movement_type: crate::resources::MovementType::Tank,
                    min_range: 3,
                    max_range: 5,
                    ..UnitStats::mock()
                },
                Health {
                    current: 100,
                    max: 100,
                },
            ));

            let skips = std::collections::HashSet::new();
            let action =
                decide_ai_action_v2(&mut world, PlayerId(1), &skips).expect("行動が決定されること");
            match action.1 {
                AiCommand::Wait { target_pos } => (target_pos.x, target_pos.y),
                other => panic!("Wait/Move を期待したが {:?}", other),
            }
        };

        // V2: 露出を考慮せず、最も敵に近い x=4 (脅威ゾーン内) へ前進する
        let (v2_x, _) = run(AiVersion::V2);
        assert_eq!(v2_x, 4, "V2 は脅威ゾーン内 (x=4) まで前進するはず");

        // V3: 脅威ゾーン (x in [4,6]) を避けて手前で待機する
        let (v3_x, _) = run(AiVersion::V3);
        assert!(
            v3_x < 4,
            "V3 は敵間接攻撃の射程外 (x<4) で待機するはず (actual: x={})",
            v3_x
        );
    }

    /// Issue #50 (Gemini #5): 間接攻撃ユニット (自走砲) が、敵直接攻撃ユニット
    /// (戦車) の「移動+攻撃」到達圏を避けることを検証する。反撃できない自走砲が
    /// 戦車の踏み込みに轢かれる配置を防ぐ。
    #[test]
    fn test_v3_avoids_direct_attacker_move_reach() {
        use crate::ai::ai_version::AiVersion;

        let run = |version: AiVersion| -> usize {
            let mut world = setup_v3_test_world(14, version);
            let mut dc = DamageChart::new();
            // 戦車 → 自走砲に大ダメージ (踏み込まれると一方的に轢かれる)
            dc.insert_damage(UnitType::Tank, UnitType::LightSpGun, 80);
            world.insert_resource(dc);

            // 自軍: 軽自走砲 (間接 射程2-3, 移動4) at x=0、部隊目標は x=13
            let sp = spawn_v3_test_unit(
                &mut world,
                PlayerId(1),
                0,
                100,
                UnitStats {
                    unit_type: UnitType::LightSpGun,
                    cost: 6200,
                    max_movement: 4,
                    movement_type: crate::resources::MovementType::Tank,
                    min_range: 2,
                    max_range: 3,
                    max_ammo1: 5,
                    max_fuel: 99,
                    ..UnitStats::mock()
                },
            );
            insert_single_unit_squad(&mut world, sp, GridPosition { x: 13, y: 0 });

            // 敵軍: 軽戦車 (直接 射程1, 移動4) at x=9 -> 移動+攻撃到達圏は距離5以内 (x>=4)
            world.spawn((
                Faction(PlayerId(2)),
                HasMoved(true),
                ActionCompleted(true),
                GridPosition { x: 9, y: 0 },
                UnitStats {
                    unit_type: UnitType::Tank,
                    cost: 6000,
                    max_movement: 4,
                    movement_type: crate::resources::MovementType::Tank,
                    min_range: 1,
                    max_range: 1,
                    ..UnitStats::mock()
                },
                Health {
                    current: 100,
                    max: 100,
                },
            ));

            let skips = std::collections::HashSet::new();
            let action =
                decide_ai_action_v2(&mut world, PlayerId(1), &skips).expect("行動が決定されること");
            match action.1 {
                AiCommand::Wait { target_pos } | AiCommand::Attack { target_pos, .. } => {
                    target_pos.x
                }
                other => panic!("Wait/Attack を期待したが {:?}", other),
            }
        };

        // V2: 移動+攻撃到達圏を考慮せず、目標へ最接近する x=4 (戦車の踏み込み圏内) へ
        let v2_x = run(AiVersion::V2);
        assert_eq!(v2_x, 4, "V2 は戦車の移動+攻撃圏内 (x=4) まで前進するはず");

        // V3: 戦車の移動+攻撃到達圏 (x>=4) を避けて x<=3 で待機する
        let v3_x = run(AiVersion::V3);
        assert!(
            v3_x <= 3,
            "V3 は戦車の移動+攻撃到達圏外 (x<=3) で待機するはず (actual: x={})",
            v3_x
        );
    }

    /// Issue #45: 間接攻撃ユニットが最小射程より内側へ不要な前進をせず、
    /// 先制攻撃圏 (待ち伏せ位置) で待機することを検証する
    #[test]
    fn test_v3_indirect_ambush_positioning() {
        use crate::ai::ai_version::AiVersion;

        let run = |version: AiVersion| -> usize {
            let mut world = setup_v3_test_world(12, version);
            let mut dc = DamageChart::new();
            dc.insert_damage(UnitType::LightSpGun, UnitType::Tank, 55);
            dc.insert_damage(UnitType::Tank, UnitType::LightSpGun, 56);
            world.insert_resource(dc);

            // 自軍: 軽自走砲 (射程2-3, 移動4) at x=0
            let sp_gun = spawn_v3_test_unit(
                &mut world,
                PlayerId(1),
                0,
                100,
                UnitStats {
                    unit_type: UnitType::LightSpGun,
                    cost: 6200,
                    max_movement: 4,
                    movement_type: crate::resources::MovementType::Tank,
                    min_range: 2,
                    max_range: 3,
                    max_fuel: 99,
                    ..UnitStats::mock()
                },
            );

            // 敵軍: 軽戦車 at x=5
            world.spawn((
                Faction(PlayerId(2)),
                HasMoved(true),
                ActionCompleted(true),
                GridPosition { x: 5, y: 0 },
                UnitStats {
                    unit_type: UnitType::Tank,
                    cost: 6000,
                    max_movement: 4,
                    movement_type: crate::resources::MovementType::Tank,
                    min_range: 1,
                    max_range: 1,
                    ..UnitStats::mock()
                },
                Health {
                    current: 100,
                    max: 100,
                },
            ));

            // 部隊目標は敵戦車の位置
            insert_single_unit_squad(&mut world, sp_gun, GridPosition { x: 5, y: 0 });

            let skips = std::collections::HashSet::new();
            let action =
                decide_ai_action_v2(&mut world, PlayerId(1), &skips).expect("行動が決定されること");
            match action.1 {
                AiCommand::Wait { target_pos } => target_pos.x,
                other => panic!("Wait/Move を期待したが {:?}", other),
            }
        };

        // V2: 接近ボーナスの勾配に従い、最小射程の内側 x=4 (距離1) まで前進してしまう
        let v2_x = run(AiVersion::V2);
        assert_eq!(v2_x, 4, "V2 は最小射程の内側 (x=4) まで前進するはず");

        // V3: 先制攻撃圏 (距離2-3 = x in [2,3]) で待ち伏せする
        let v3_x = run(AiVersion::V3);
        let v3_dist = 5 - v3_x;
        assert!(
            (2..=3).contains(&v3_dist),
            "V3 は射程内の待ち伏せ位置 (距離2-3) で待機するはず (actual: x={}, dist={})",
            v3_x,
            v3_dist
        );
    }

    /// Issue #44: HP が低下したユニットが、接近ボーナスの勾配に逆らってでも
    /// 平地より防御効果の高い森で待機することを検証する。
    /// ただし敵の脅威が近くにある場合に限る（Gemini 指摘 #3: 安全な後方での
    /// 無意味な引きこもりを防ぐゲート）。
    #[test]
    fn test_v3_low_hp_prefers_defensive_terrain() {
        use crate::ai::ai_version::AiVersion;

        // enemy_x: 敵ユニットの位置。None なら敵なし（安全な後方）
        let run = |version: AiVersion, hp: u32, enemy_x: Option<usize>| -> usize {
            let mut world = setup_v3_test_world(12, version);
            world.insert_resource(DamageChart::new());
            // x=2 だけ森 (防御20)、他は平地 (防御5)
            world
                .resource_mut::<Map>()
                .set_terrain(2, 0, Terrain::Forest)
                .unwrap();

            // 自軍: 軽戦車 at x=0 (部隊目標 x=10 に向かって前進中)
            let tank = spawn_v3_test_unit(
                &mut world,
                PlayerId(1),
                0,
                hp,
                UnitStats {
                    unit_type: UnitType::Tank,
                    cost: 6000,
                    max_movement: 4,
                    movement_type: crate::resources::MovementType::Tank,
                    min_range: 1,
                    max_range: 1,
                    max_fuel: 99,
                    ..UnitStats::mock()
                },
            );
            insert_single_unit_squad(&mut world, tank, GridPosition { x: 10, y: 0 });

            // 敵ユニット (行動済み・脅威の存在のみを表現)
            if let Some(ex) = enemy_x {
                world.spawn((
                    Faction(PlayerId(2)),
                    HasMoved(true),
                    ActionCompleted(true),
                    GridPosition { x: ex, y: 0 },
                    UnitStats {
                        unit_type: UnitType::Tank,
                        cost: 6000,
                        max_movement: 4,
                        movement_type: crate::resources::MovementType::Tank,
                        min_range: 1,
                        max_range: 1,
                        ..UnitStats::mock()
                    },
                    Health {
                        current: 100,
                        max: 100,
                    },
                ));
            }

            let skips = std::collections::HashSet::new();
            let action =
                decide_ai_action_v2(&mut world, PlayerId(1), &skips).expect("行動が決定されること");
            match action.1 {
                AiCommand::Wait { target_pos }
                | AiCommand::Attack { target_pos, .. }
                | AiCommand::Capture { target_pos }
                | AiCommand::Merge { target_pos, .. } => target_pos.x,
                other => panic!("位置を伴う行動を期待したが {:?}", other),
            }
        };

        // 近傍脅威あり (敵戦車 x=6, 前線の森 x=2 から距離4)
        let threat = Some(6usize);

        // V2 は低HPでも目標へ最短で前進する (森 x=2 の移動コストにより x=3 が最遠到達点)
        let v2_x = run(AiVersion::V2, 40, threat);
        assert_eq!(v2_x, 3, "V2 は低HPでも平地 (x=3) まで前進するはず");

        // V3 は健全時は前進を優先し、低HP＋近傍脅威ありなら森 (x=2) で待機する
        let v3_healthy_x = run(AiVersion::V3, 100, threat);
        assert_eq!(v3_healthy_x, 3, "V3 も健全時は前進を優先するはず");
        let v3_low_hp_x = run(AiVersion::V3, 40, threat);
        assert_eq!(
            v3_low_hp_x, 2,
            "V3 は低HP+近傍脅威で森 (x=2) に退避するはず (actual: x={})",
            v3_low_hp_x
        );

        // #3 ゲート: 敵が近くにいない安全な後方では、低HPでも森に引きこもらず前進する
        let v3_low_hp_safe = run(AiVersion::V3, 40, None);
        assert_eq!(
            v3_low_hp_safe, 3,
            "V3 は低HPでも脅威がなければ森に籠らず前進するはず (actual: x={})",
            v3_low_hp_safe
        );
    }
}
