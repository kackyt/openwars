//! 拡張経路（小規模マップ戦略・非V4）の実装。
//!
//! 親モジュールはmain互換の実装を持ち、本文の異なる関数は
//! `crate::ai::code_path::is_extended()` でここへ委譲する。

#[allow(unused_imports)]
use super::*;

impl AiSimulationState {
    /// 各部隊（Squad）の目標割り当てプランを World 内の仮想ユニットに反映し、1ターン後の状態をシミュレートします。
    pub fn simulate_plan_extended(
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
                for &cargo_entity in &squad.cargo_entities {
                    if let Some(mut position) = world.get_mut::<GridPosition>(cargo_entity) {
                        *position = target_pos;
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

                    let interaction_max_range = match squad.mission_type {
                        crate::ai::squad::MissionType::Attack
                        | crate::ai::squad::MissionType::Defense => {
                            if stats.max_range > 0 {
                                stats.max_range
                            } else {
                                1
                            }
                        }
                        crate::ai::squad::MissionType::Capture => 0,
                        crate::ai::squad::MissionType::Transport
                        | crate::ai::squad::MissionType::Reserve => 1,
                    };

                    // 目標からの SSSP 最短距離テーブルをキャッシュを利用して取得 (O(1)再利用)
                    let dist_map = calculate_all_turn_distances_cached(
                        &map,
                        &registry,
                        &unit_positions,
                        (target_pos.x, target_pos.y),
                        stats.movement_type,
                        stats.max_movement,
                        interaction_max_range,
                        perspective_player,
                        cache,
                    );

                    let mut best_tile = *pos;
                    let current_turns =
                        *dist_map
                            .get(&pos)
                            .unwrap_or(&crate::ai::turn_distance::TurnDistance {
                                turns: u32::MAX,
                                used_mp: u32::MAX,
                            });

                    if current_turns.turns != u32::MAX && current_turns.turns > 0 {
                        // 目標までのターン数が 1 減る位置（あるいは目標そのもの）まで隣接マスを辿って高速に進む
                        let target_turns = current_turns.turns.saturating_sub(1);
                        let mut temp_pos = *pos;
                        let mut temp_turns = current_turns;

                        // 無限ループを防ぐため、最大移動力 max_movement 以上のステップは進まない
                        let max_steps = stats.max_movement as usize;
                        for _ in 0..max_steps {
                            let mut next_best_tile = temp_pos;
                            let mut min_next_turns = temp_turns;

                            let mut best_manhattan = (temp_pos.x as i32 - target_pos.x as i32)
                                .abs()
                                + (temp_pos.y as i32 - target_pos.y as i32).abs();

                            for next_tile in map.get_adjacent(temp_pos.x, temp_pos.y) {
                                let next_pos = GridPosition {
                                    x: next_tile.0,
                                    y: next_tile.1,
                                };
                                let next_dist = *dist_map.get(&next_pos).unwrap_or(
                                    &crate::ai::turn_distance::TurnDistance {
                                        turns: u32::MAX,
                                        used_mp: u32::MAX,
                                    },
                                );
                                let next_manhattan = (next_pos.x as i32 - target_pos.x as i32)
                                    .abs()
                                    + (next_pos.y as i32 - target_pos.y as i32).abs();

                                if next_dist < min_next_turns
                                    || (next_dist == min_next_turns
                                        && next_manhattan < best_manhattan)
                                {
                                    min_next_turns = next_dist;
                                    next_best_tile = next_pos;
                                    best_manhattan = next_manhattan;
                                }
                            }

                            if next_best_tile == temp_pos {
                                // これ以上目標に近づけない（障害物や進入不可地形など）
                                break;
                            }

                            temp_pos = next_best_tile;
                            temp_turns = min_next_turns;

                            // ターン数が目標値（現在ターン-1）以下になったら終了
                            if temp_turns.turns <= target_turns {
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
