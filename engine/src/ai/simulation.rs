#![allow(clippy::collapsible_if)]

use crate::ai::squad::{MissionType, Squad};
use crate::ai::turn_distance::calculate_all_turn_distances_cached;
use crate::components::{Ammo, Faction, GridPosition, Health, PlayerId, Property, UnitStats};
use crate::resources::{Map, master_data::MasterDataRegistry};
use bevy_ecs::prelude::*;
use std::collections::HashMap;

/// ビーム探索用の、一時的な盤面状態の退避・仮想シミュレーションを行うための構造体
pub struct AiSimulationState {
    unit_backups: Vec<(Entity, GridPosition, Health, Option<Ammo>)>,
    property_backups: Vec<(Entity, Option<PlayerId>, u32)>,
}

impl AiSimulationState {
    /// 現在の World の状態を完全にバックアップします
    pub fn backup(world: &mut World) -> Self {
        let mut unit_backups = Vec::new();
        let mut q_units = world.query::<(Entity, &GridPosition, &Health, Option<&Ammo>)>();
        for (entity, pos, health, ammo_opt) in q_units.iter(world) {
            unit_backups.push((entity, *pos, *health, ammo_opt.cloned()));
        }

        let mut property_backups = Vec::new();
        let mut q_props = world.query::<(Entity, &Property)>();
        for (entity, prop) in q_props.iter(world) {
            property_backups.push((entity, prop.owner_id, prop.capture_points));
        }

        Self {
            unit_backups,
            property_backups,
        }
    }

    /// バックアップした状態を World に書き戻し、完全に元の状態へ復帰させます
    pub fn restore(self, world: &mut World) {
        for (entity, pos, health, ammo_opt) in self.unit_backups {
            if let Some(mut p) = world.get_mut::<GridPosition>(entity) {
                *p = pos;
            }
            if let Some(mut h) = world.get_mut::<Health>(entity) {
                *h = health;
            }
            if let Some(ammo) = ammo_opt {
                if let Some(mut a) = world.get_mut::<Ammo>(entity) {
                    *a = ammo;
                }
            }
        }

        for (entity, owner, cap_points) in self.property_backups {
            if let Some(mut prop) = world.get_mut::<Property>(entity) {
                prop.owner_id = owner;
                prop.capture_points = cap_points;
            }
        }
    }

    /// 各部隊（Squad）の目標割り当てプランを World 内の仮想ユニットに反映し、1ターン後の状態をシミュレートします。
    pub fn simulate_plan(
        world: &mut World,
        squads: &[Squad],
        assignments: &HashMap<crate::ai::squad::SquadId, GridPosition>, // Squad ID -> Target GridPosition
        perspective_player: PlayerId,
        cache: &mut crate::ai::turn_distance::AiTurnCache,
    ) {
        let map = world.resource::<Map>().clone();
        let registry = world
            .get_resource::<MasterDataRegistry>()
            .cloned()
            .unwrap_or_default();

        let mut unit_positions = HashMap::new();
        let mut q_all_units = world.query::<(&Faction, &GridPosition, &UnitStats)>();
        for (faction, pos, stats) in q_all_units.iter(world) {
            unit_positions.insert(
                (pos.x, pos.y),
                crate::systems::movement::OccupantInfo {
                    player_id: faction.0,
                    is_transport: stats.max_cargo > 0,
                    unit_type: stats.unit_type,
                    loadable_types: stats.loadable_unit_types.clone(),
                    free_slots: stats.max_cargo,
                },
            );
        }

        for squad in squads {
            let Some(&target_pos) = assignments.get(&squad.id) else {
                continue;
            };

            if squad.mission_type == MissionType::Transport {
                // マクロ・シミュレーション（深い読み）: ターゲットにワープ＆ドロップ
                // 歩兵を目標地点に直接配置することで、直後の evaluate_board が「上陸完了」として高く評価する
                if let Some(cargo_ent) = squad.transport_cargo {
                    if let Some(mut pos) = world.get_mut::<GridPosition>(cargo_ent) {
                        *pos = target_pos;
                    }
                }

                for &member in &squad.members {
                    if let Some(mut pos) = world.get_mut::<GridPosition>(member) {
                        *pos = target_pos;
                    }
                }
                continue;
            }

            for &member in &squad.members {
                let Some(stats) = world.get::<UnitStats>(member).cloned() else {
                    continue;
                };

                let mut is_capture_squad_reached = false;
                let pos_val;

                // pos のスコープを限定して借用チェッカーを回避する
                {
                    let Some(mut pos) = world.get_mut::<GridPosition>(member) else {
                        continue;
                    };

                    // 目標からの SSSP 最短距離テーブルをキャッシュを利用して取得 (O(1)再利用)
                    let dist_map = calculate_all_turn_distances_cached(
                        &map,
                        &registry,
                        &unit_positions,
                        (target_pos.x, target_pos.y),
                        stats.movement_type,
                        stats.max_movement,
                        perspective_player,
                        cache,
                    );

                    let mut best_tile = *pos;
                    let current_turns = *dist_map.get(&pos).unwrap_or(&u32::MAX);

                    if current_turns != u32::MAX && current_turns > 0 {
                        // 目標までのターン数が 1 減る位置（あるいは目標そのもの）まで隣接マスを辿って高速に進む
                        let target_turns = current_turns.saturating_sub(1);
                        let mut temp_pos = *pos;
                        let mut temp_turns = current_turns;

                        // 無限ループを防ぐため、最大移動力 max_movement 以上のステップは進まない
                        let max_steps = stats.max_movement as usize;
                        for _ in 0..max_steps {
                            let mut next_best_tile = temp_pos;
                            let mut min_next_turns = temp_turns;

                            for next_tile in map.get_adjacent(temp_pos.x, temp_pos.y) {
                                let next_pos = GridPosition {
                                    x: next_tile.0,
                                    y: next_tile.1,
                                };
                                let next_dist = *dist_map.get(&next_pos).unwrap_or(&u32::MAX);
                                if next_dist < min_next_turns {
                                    min_next_turns = next_dist;
                                    next_best_tile = next_pos;
                                }
                            }

                            if next_best_tile == temp_pos {
                                // これ以上目標に近づけない（障害物や進入不可地形など）
                                break;
                            }

                            temp_pos = next_best_tile;
                            temp_turns = min_next_turns;

                            // ターン数が目標値（現在ターン-1）以下になったら終了
                            if temp_turns <= target_turns {
                                break;
                            }
                        }
                        best_tile = temp_pos;
                    }

                    *pos = best_tile;
                    pos_val = best_tile;

                    if squad.mission_type == MissionType::Capture
                        && pos.x == target_pos.x
                        && pos.y == target_pos.y
                    {
                        is_capture_squad_reached = true;
                    }
                }

                // 占領シミュレーション (pos のライフタイムが終了した後に安全に query を実行)
                if is_capture_squad_reached {
                    let mut q_props = world.query::<(Entity, &GridPosition, &mut Property)>();
                    for (_ent, prop_pos, mut prop) in q_props.iter_mut(world) {
                        if prop_pos.x == pos_val.x && prop_pos.y == pos_val.y {
                            prop.owner_id = Some(perspective_player);
                            prop.capture_points = prop.max_capture_points;
                        }
                    }
                }
            }
        }
    }
}
