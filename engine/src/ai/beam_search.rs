#![allow(clippy::collapsible_if)]
#![allow(clippy::map_entry)]

use crate::ai::eval::evaluate_board;
use crate::ai::simulation::AiSimulationState;
use crate::ai::squad::{MissionType, Squad, SquadManager};
use crate::components::{GridPosition, PlayerId};
use bevy_ecs::prelude::*;
use std::collections::HashMap;

/// ビーム幅のデフォルト値
pub const BEAM_WIDTH: usize = 3;

/// 各部隊（Squad）の目標割り当てプラン
#[derive(Debug, Clone)]
pub struct SquadAssignmentPlan {
    /// キー: Squad ID
    /// 値: 割り当てられた目標座標
    pub assignments: HashMap<u32, GridPosition>,
    /// プランの暫定評価スコア
    pub score: i32,
}

/// 全ての部隊に対して、最も盤面評価が高くなるような目標割り当てをビーム探索で決定します。
pub fn run_squad_beam_search(world: &mut World, perspective_player: PlayerId) {
    let mut manager = world.remove_resource::<SquadManager>().unwrap_or_default();

    if manager.squads.is_empty() {
        world.insert_resource(manager);
        return;
    }

    // 1. 目標候補（Target）の収集
    let mut target_enemies = Vec::new();
    let enemy_clusters = crate::ai::cluster::detect_enemy_clusters(world, perspective_player);
    for cluster in &enemy_clusters {
        if !target_enemies.contains(&cluster.center) {
            target_enemies.push(cluster.center);
        }
    }

    let mut target_props = Vec::new();
    let mut my_capital_pos = None;
    let mut enemy_capital_pos = None;

    let mut q_props = world.query::<(&GridPosition, &crate::components::Property)>();
    for (pos, prop) in q_props.iter(world) {
        if prop.owner_id != Some(perspective_player) {
            if !target_props.contains(pos) {
                target_props.push(*pos);
            }
            if prop.terrain == crate::resources::Terrain::Capital && prop.owner_id.is_some() {
                enemy_capital_pos = Some(*pos);
            }
        } else if prop.terrain == crate::resources::Terrain::Capital {
            my_capital_pos = Some(*pos);
        }
    }

    let mut all_target_candidates = Vec::new();
    all_target_candidates.extend(&target_enemies);
    for p in &target_props {
        if !all_target_candidates.contains(p) {
            all_target_candidates.push(*p);
        }
    }
    if let Some(cap) = my_capital_pos {
        if !all_target_candidates.contains(&cap) {
            all_target_candidates.push(cap);
        }
    }

    // 2. 割り当てが必要な部隊（Squad）を収集（輸送部隊は planner が目標を固定するため除外）
    let active_squads: Vec<Squad> = manager
        .squads
        .iter()
        .filter(|s| s.mission_type != MissionType::Transport && !s.members.is_empty())
        .cloned()
        .collect();

    if active_squads.is_empty() || all_target_candidates.is_empty() {
        world.insert_resource(manager);
        return;
    }

    // 3. ビーム探索の開始
    let mut search_cache = crate::ai::turn_distance::AiTurnCache::new();

    // 初期状態：空のプラン
    let mut beam = vec![SquadAssignmentPlan {
        assignments: HashMap::new(),
        score: 0,
    }];

    // 順次、部隊に目標を割り当ててビームを展開
    for squad in &active_squads {
        let mut next_beam = Vec::new();

        let mut valid_targets = Vec::new();
        match squad.mission_type {
            MissionType::Attack => {
                valid_targets.extend(&target_enemies);
                // 前線の押し上げ：未占領拠点や敵拠点もターゲット候補に含める
                valid_targets.extend(&target_props);
                if let Some(cap) = enemy_capital_pos {
                    if !valid_targets.contains(&cap) {
                        valid_targets.push(cap);
                    }
                }
            }
            MissionType::Capture => {
                valid_targets.extend(&target_props);
            }
            MissionType::Defense => {
                if let Some(cap) = my_capital_pos {
                    valid_targets.push(cap);
                }
                valid_targets.extend(&target_enemies);
            }
            MissionType::Transport => {}
        }

        if valid_targets.is_empty() {
            valid_targets.extend(&all_target_candidates);
        }

        // ターゲット候補が多すぎると処理時間が爆発するため、各部隊の現在位置から近い N 個に絞る
        let mut sorted_targets = valid_targets.clone();
        if let Some(&first_member) = squad.members.iter().next() {
            if let Some(pos) = world.get::<GridPosition>(first_member) {
                sorted_targets.sort_by_key(|t| {
                    (pos.x as i32 - t.x as i32).abs() + (pos.y as i32 - t.y as i32).abs()
                });
            }
        }
        sorted_targets.truncate(5); // 直近の5つのターゲット候補に絞る
        valid_targets = sorted_targets;

        for plan in &beam {
            for &target in &valid_targets {
                let mut new_plan = plan.clone();
                new_plan.assignments.insert(squad.id, target);

                // 未割り当ての残りの部隊を貪欲法（最寄りの目標）で一時的に補完して完成プランにする
                let mut complete_assignments = new_plan.assignments.clone();
                for other_squad in &active_squads {
                    if !complete_assignments.contains_key(&other_squad.id) {
                        // 最寄りのターゲットを貪欲に仮割り当て
                        if let Some(&first_member) = other_squad.members.iter().next() {
                            if let Some(pos) = world.get::<GridPosition>(first_member).cloned() {
                                let best_target = all_target_candidates
                                    .iter()
                                    .min_by_key(|t| {
                                        (pos.x as i32 - t.x as i32).abs()
                                            + (pos.y as i32 - t.y as i32).abs()
                                    })
                                    .cloned();
                                if let Some(t) = best_target {
                                    complete_assignments.insert(other_squad.id, t);
                                }
                            }
                        }
                    }
                }

                // 暫定スコアの算出（シミュレーション評価）
                let backup = AiSimulationState::backup(world);
                AiSimulationState::simulate_plan(
                    world,
                    &active_squads,
                    &complete_assignments,
                    perspective_player,
                    &mut search_cache,
                );
                let mut score = evaluate_board(world, perspective_player, Some(&mut search_cache));

                // 目標接近ボーナス (Proximity Bonus) の計算
                // 各部隊のメンバーが割り当てられた目標にどれだけ近いかを評価し、スコアに加算します。
                let map = world.resource::<crate::resources::Map>().clone();
                let registry = world
                    .get_resource::<crate::resources::master_data::MasterDataRegistry>()
                    .cloned()
                    .unwrap_or_default();

                let mut unit_positions = HashMap::new();
                let mut q_all_units = world.query::<(
                    &crate::components::Faction,
                    &GridPosition,
                    &crate::components::UnitStats,
                )>();
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

                for squad in &active_squads {
                    if let Some(&target_pos) = complete_assignments.get(&squad.id) {
                        for &member in &squad.members {
                            if let (Some(pos), Some(stats), Some(faction)) = (
                                world.get::<GridPosition>(member),
                                world.get::<crate::components::UnitStats>(member),
                                world.get::<crate::components::Faction>(member),
                            ) {
                                // 目標を始点とした SSSP で各ユニットへの最短ターン数を取得
                                let dist_map =
                                    crate::ai::turn_distance::calculate_all_turn_distances_cached(
                                        &map,
                                        &registry,
                                        &unit_positions,
                                        (target_pos.x, target_pos.y),
                                        stats.movement_type,
                                        stats.max_movement,
                                        faction.0,
                                        &mut search_cache,
                                    );

                                if let Some(&turns) = dist_map.get(pos) {
                                    if turns != u32::MAX {
                                        // 1ターン近づくごとに 150 相当の加点を行う
                                        let mut proximity_bonus = (50 - turns.min(50)) as i32 * 150;
                                        if Some(target_pos) == enemy_capital_pos
                                            && (squad.mission_type == MissionType::Attack
                                                || squad.mission_type == MissionType::Capture)
                                        {
                                            // 攻撃・占領部隊にとって敵首都は最重要目標なので、3倍のボーナス（極端な特攻はしないが優先はする）
                                            proximity_bonus *= 3;
                                        }
                                        score += proximity_bonus;
                                    }
                                }
                            }
                        }
                    }
                }

                backup.restore(world); // 元に戻す

                new_plan.score = score;
                next_beam.push(new_plan);
            }
        }

        // スコア降順でソートし、ビーム幅（N = 5）に絞り込む
        next_beam.sort_by_key(|p| std::cmp::Reverse(p.score));
        next_beam.truncate(BEAM_WIDTH);
        beam = next_beam;
    }

    // 4. 最もスコアの高いプランを採択して SquadManager 内の部隊目標を決定
    if let Some(best_plan) = beam.first() {
        for squad in &mut manager.squads {
            if let Some(&target) = best_plan.assignments.get(&squad.id) {
                squad.target = Some(target);
                squad.phase = crate::ai::squad::MissionPhase::MovingToTarget;
            }
        }
    }

    world.insert_resource(manager);
}
