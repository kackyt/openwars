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
    pub assignments: HashMap<crate::ai::squad::SquadId, GridPosition>,
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
    if let Some(cap) = my_capital_pos
        && !all_target_candidates.contains(&cap)
    {
        all_target_candidates.push(cap);
    }

    // 2. 割り当てが必要な部隊（Squad）を収集
    let active_squads: Vec<Squad> = manager
        .squads
        .iter()
        // active playerのgeneric責務だけを探索し、輸送および島別campaign責務の目標を上書きしない。
        .filter(|s| s.owner_id == Some(perspective_player))
        .filter(|s| {
            !s.members.is_empty()
                && !matches!(
                    s.mission_type,
                    MissionType::Transport | MissionType::Interception(_)
                )
        })
        .filter(|s| s.target_island.is_none())
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
                if let Some(cap) = enemy_capital_pos
                    && !valid_targets.contains(&cap)
                {
                    valid_targets.push(cap);
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
            MissionType::Interception(_) => {
                // 緊急ミッションの目標は盤面分析で確定済みのため、ビーム探索で上書きしない。
                if let Some(target) = squad.target {
                    valid_targets.push(target);
                }
            }
            MissionType::Transport => {
                // squad.rs の段階ですでに戦略的価値に基づく target_island が設定されているはず
                if let Some(target_island_id) = squad.target_island {
                    let mut island_targets = Vec::new();
                    if let Some(island_map) = world.get_resource::<crate::ai::islands::IslandMap>()
                    {
                        for &t_pos in &target_props {
                            if let Some(island) = island_map.get_island_at(&t_pos)
                                && island.id == target_island_id
                            {
                                island_targets.push(t_pos);
                            }
                        }
                    }
                    if !island_targets.is_empty() {
                        valid_targets.extend(&island_targets);
                    } else {
                        // 万が一、対象の島に未占領拠点が無い（すでに占領済み等の）場合はフォールバック
                        valid_targets.extend(&target_props);
                    }
                } else {
                    // target_island が未設定の場合は通常のフォールバック
                    valid_targets.extend(&target_props);
                }
            }
        }

        // 上陸後に引き継がれた部隊は、侵攻対象島の外へ目標を飛ばさない。
        if squad.mission_type != MissionType::Transport
            && let Some(target_island) = squad.target_island
            && let Some(island_map) = world.get_resource::<crate::ai::islands::IslandMap>()
        {
            valid_targets.retain(|target| {
                island_map
                    .get_island_at(target)
                    .is_some_and(|island| island.id == target_island)
            });
            if valid_targets.is_empty()
                && let Some(current_target) = squad.target
            {
                valid_targets.push(current_target);
            }
        }

        if valid_targets.is_empty() {
            valid_targets.extend(&all_target_candidates);
        }

        // ターゲット候補が多すぎると処理時間が爆発するため、各部隊の現在位置から近い N 個に絞る
        let mut sorted_targets = valid_targets.clone();
        if let Some(&first_member) = squad.members.iter().next()
            && let Some(pos) = world.get::<GridPosition>(first_member)
        {
            let stats = world
                .get::<crate::components::UnitStats>(first_member)
                .cloned()
                .unwrap_or_default();
            let map = world.resource::<crate::resources::Map>();

            // ターゲットが Naval ユニットの攻撃対象として適切か（水域が射程内にあるか）を判定
            sorted_targets.retain(|t| {
                if stats.movement_type != crate::resources::MovementType::Ship {
                    return true; // 海軍以外はフィルタしない
                }
                let range = if stats.max_range > 0 {
                    stats.max_range as i32
                } else {
                    1
                };
                for dx in -range..=range {
                    for dy in -range..=range {
                        if dx.abs() + dy.abs() > range {
                            continue;
                        }
                        let nx = t.x as i32 + dx;
                        let ny = t.y as i32 + dy;
                        if nx >= 0
                            && nx < map.width as i32
                            && ny >= 0
                            && ny < map.height as i32
                            && let Some(terrain) = map.get_terrain(nx as usize, ny as usize)
                            && matches!(
                                terrain,
                                crate::resources::Terrain::Sea
                                    | crate::resources::Terrain::Shoal
                                    | crate::resources::Terrain::Port
                            )
                        {
                            return true;
                        }
                    }
                }
                false
            });

            sorted_targets.sort_by_key(|t| {
                (pos.x as i32 - t.x as i32).abs() + (pos.y as i32 - t.y as i32).abs()
            });
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
                    if let std::collections::hash_map::Entry::Vacant(e) =
                        complete_assignments.entry(other_squad.id)
                    {
                        // 最寄りのターゲットを貪欲に仮割り当て
                        if let Some(&first_member) = other_squad.members.iter().next()
                            && let Some(pos) = world.get::<GridPosition>(first_member).cloned()
                        {
                            let best_target = all_target_candidates
                                .iter()
                                .min_by_key(|t| {
                                    (pos.x as i32 - t.x as i32).abs()
                                        + (pos.y as i32 - t.y as i32).abs()
                                })
                                .cloned();
                            if let Some(t) = best_target {
                                e.insert(t);
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
                                let mut turns = u32::MAX;

                                let interaction_max_range = match squad.mission_type {
                                    MissionType::Attack | MissionType::Defense => {
                                        if stats.max_range > 0 {
                                            stats.max_range
                                        } else {
                                            1
                                        }
                                    }
                                    MissionType::Capture => 0,
                                    MissionType::Interception(_) => stats.max_range.max(1),
                                    MissionType::Transport => 1,
                                };

                                let dist_map =
                                    crate::ai::turn_distance::calculate_all_turn_distances_cached(
                                        &map,
                                        &registry,
                                        &unit_positions,
                                        (target_pos.x, target_pos.y),
                                        stats.movement_type,
                                        stats.max_movement,
                                        interaction_max_range,
                                        faction.0,
                                        &mut search_cache,
                                    );
                                if let Some(&t) = dist_map.get(pos) {
                                    turns = t.turns;
                                }

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

                                    // 輸送部隊の Time Discounting: 到達ターン数に応じたペナルティ（遠すぎる島への無謀な輸送を防ぐ）
                                    if squad.mission_type == MissionType::Transport {
                                        proximity_bonus -= turns as i32 * 200;
                                    }

                                    score += proximity_bonus;
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

        // スコア降順でソートし、ビーム幅（BEAM_WIDTH = 3）に絞り込む
        next_beam.sort_by_key(|p| std::cmp::Reverse(p.score));
        next_beam.truncate(BEAM_WIDTH);
        beam = next_beam;
    }

    // 4. 最もスコアの高いプランを採択して SquadManager 内の部隊目標を決定
    if let Some(best_plan) = beam.first() {
        for squad in &mut manager.squads {
            if let Some(&target) = best_plan.assignments.get(&squad.id) {
                squad.target = Some(target);
                if squad.mission_type == MissionType::Transport {
                    if let Some(island_map) = world.get_resource::<crate::ai::islands::IslandMap>()
                        && let Some(island) = island_map.get_island_at(&target)
                    {
                        squad.target_island = Some(island.id);
                    }
                } else {
                    squad.phase = crate::ai::squad::MissionPhase::MovingToTarget;
                }
            }
        }
    }

    world.insert_resource(manager);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::*;
    use crate::resources::master_data::*;
    use crate::resources::*;
    use std::collections::BTreeSet;

    fn setup_test_world() -> World {
        let master_data = MasterDataRegistry::load().unwrap();
        let (mut world, _) =
            crate::setup::initialize_world_from_master_data(&master_data, "map_1").unwrap();

        let entities: Vec<Entity> = world.query::<Entity>().iter(&world).collect();
        for e in entities {
            world.despawn(e);
        }

        let map = Map {
            width: 10,
            height: 10,
            tiles: vec![Terrain::Plains; 100],
            topology: GridTopology::Square,
        };

        world.insert_resource(map);
        world.insert_resource(crate::ai::ai_version::PlayerAiSettings::default());
        world
    }

    #[test]
    fn test_beam_search_assigns_targets() {
        let mut world = setup_test_world();
        let p1 = PlayerId(1);
        let p2 = PlayerId(2);

        // Enemy unit to act as a target at (8, 8)
        world.spawn((
            p2,
            Faction(p2),
            GridPosition { x: 8, y: 8 },
            UnitStats {
                unit_type: UnitType::Tank,
                movement_type: MovementType::Tank,
                max_movement: 6,
                cost: 7000,
                ..UnitStats::mock()
            },
        ));

        // Friendly unit
        let u1 = world
            .spawn((
                p1,
                Faction(p1),
                GridPosition { x: 2, y: 2 },
                UnitStats {
                    unit_type: UnitType::Tank,
                    movement_type: MovementType::Tank,
                    max_movement: 6,
                    cost: 7000,
                    ..UnitStats::mock()
                },
                Health {
                    current: 100,
                    max: 100,
                },
            ))
            .id();

        let mut squad = Squad {
            id: crate::ai::squad::SquadId(1),
            owner_id: Some(p1),
            members: BTreeSet::new(),
            mission_type: MissionType::Attack,
            target: None,
            target_island: None,
            phase: crate::ai::squad::MissionPhase::Forming,
            transport_entity: None,
            cargo_entities: Vec::new(),
            pickup_position: None,
            drop_position: None,
            delivered_cargo: Vec::new(),
            return_after_combat: false,
        };
        squad.members.insert(u1);

        let mut manager = SquadManager::new();
        manager.squads.push(squad);
        world.insert_resource(manager);

        // Also add PlayerAiSettings so the evaluation runs V2 (if the default is correctly mapped, but run_squad_beam_search uses evaluate_board which checks it)
        let mut settings = crate::ai::ai_version::PlayerAiSettings::default();
        settings.set_version(p1, crate::ai::ai_version::AiVersion::V2);
        world.insert_resource(settings);

        run_squad_beam_search(&mut world, p1);

        let manager = world.get_resource::<SquadManager>().unwrap();

        assert_eq!(manager.squads.len(), 1);
        assert_eq!(manager.squads[0].target, Some(GridPosition { x: 8, y: 8 }));
        assert_eq!(
            manager.squads[0].phase,
            crate::ai::squad::MissionPhase::MovingToTarget
        );
    }

    #[test]
    fn beam_search_preserves_foreign_owned_squad() {
        let mut world = setup_test_world();
        let player_a = PlayerId(1);
        let player_b = PlayerId(2);
        let player_c = PlayerId(3);
        world.spawn((
            Faction(player_c),
            GridPosition { x: 8, y: 8 },
            UnitStats {
                unit_type: UnitType::Tank,
                movement_type: MovementType::Tank,
                max_movement: 6,
                ..UnitStats::mock()
            },
        ));
        let unit_a = world
            .spawn((
                Faction(player_a),
                GridPosition { x: 2, y: 2 },
                UnitStats {
                    unit_type: UnitType::Tank,
                    movement_type: MovementType::Tank,
                    max_movement: 6,
                    ..UnitStats::mock()
                },
            ))
            .id();
        let unit_b = world
            .spawn((
                Faction(player_b),
                GridPosition { x: 7, y: 7 },
                UnitStats {
                    unit_type: UnitType::Tank,
                    movement_type: MovementType::Tank,
                    max_movement: 6,
                    ..UnitStats::mock()
                },
            ))
            .id();
        let mut manager = SquadManager::new();
        let own = manager.create_owned_squad(MissionType::Attack, player_a);
        own.members.insert(unit_a);
        let foreign = manager.create_owned_squad(MissionType::Attack, player_b);
        foreign.members.insert(unit_b);
        foreign.target = Some(GridPosition { x: 1, y: 1 });
        foreign.phase = crate::ai::squad::MissionPhase::Executing;
        let foreign_id = foreign.id;
        world.insert_resource(manager);

        run_squad_beam_search(&mut world, player_a);

        let manager = world.resource::<SquadManager>();
        let foreign = manager
            .squads
            .iter()
            .find(|squad| squad.id == foreign_id)
            .unwrap();
        assert_eq!(foreign.owner_id, Some(player_b));
        assert_eq!(foreign.target, Some(GridPosition { x: 1, y: 1 }));
        assert_eq!(foreign.phase, crate::ai::squad::MissionPhase::Executing);
        assert_eq!(foreign.members, BTreeSet::from([unit_b]));
    }

    #[test]
    fn beam_search_preserves_campaign_managed_local_target_and_phase() {
        let mut world = setup_test_world();
        let player = PlayerId(1);
        let exact_target = GridPosition { x: 4, y: 4 };
        let map = world.resource::<Map>().clone();
        let island_map = crate::ai::islands::IslandMap::analyze(&map);
        let island_id = island_map.get_island_at(&exact_target).unwrap().id;
        world.insert_resource(island_map);
        world.spawn((
            GridPosition { x: 0, y: 0 },
            Property::new(Terrain::Capital, Some(player), 100),
        ));
        world.spawn((
            Faction(PlayerId(2)),
            GridPosition { x: 8, y: 8 },
            UnitStats {
                unit_type: UnitType::Tank,
                movement_type: MovementType::Tank,
                max_movement: 6,
                ..UnitStats::mock()
            },
        ));
        let defender = world
            .spawn((
                Faction(player),
                GridPosition { x: 3, y: 4 },
                UnitStats {
                    unit_type: UnitType::Tank,
                    movement_type: MovementType::Tank,
                    max_movement: 6,
                    ..UnitStats::mock()
                },
            ))
            .id();
        let mut manager = SquadManager::new();
        let squad = manager.create_owned_squad(MissionType::Defense, player);
        squad.members.insert(defender);
        squad.target = Some(exact_target);
        squad.target_island = Some(island_id);
        squad.phase = crate::ai::squad::MissionPhase::Executing;
        let squad_id = squad.id;
        world.insert_resource(manager);

        run_squad_beam_search(&mut world, player);

        let manager = world.resource::<SquadManager>();
        let squad = manager
            .squads
            .iter()
            .find(|squad| squad.id == squad_id)
            .unwrap();
        assert_eq!(squad.target, Some(exact_target));
        assert_eq!(squad.target_island, Some(island_id));
        assert_eq!(squad.phase, crate::ai::squad::MissionPhase::Executing);
    }

    #[test]
    fn beam_search_does_not_redirect_transport_invasion() {
        let mut world = setup_test_world();
        let p1 = PlayerId(1);
        let map = world.resource::<Map>().clone();
        let island_map = crate::ai::islands::IslandMap::analyze(&map);
        let island_id = island_map
            .get_island_at(&GridPosition { x: 8, y: 8 })
            .unwrap()
            .id;
        world.insert_resource(island_map);
        world.spawn((
            GridPosition { x: 1, y: 1 },
            crate::components::Property::new(Terrain::Capital, Some(p1), 100),
        ));
        world.spawn((
            GridPosition { x: 8, y: 8 },
            crate::components::Property::new(Terrain::City, Some(PlayerId(2)), 100),
        ));
        let transport = world
            .spawn((
                Faction(p1),
                GridPosition { x: 2, y: 2 },
                UnitStats {
                    unit_type: UnitType::Lander,
                    movement_type: MovementType::Ship,
                    max_movement: 6,
                    max_cargo: 2,
                    ..UnitStats::mock()
                },
            ))
            .id();

        let mut manager = SquadManager::new();
        let squad = manager.create_squad(MissionType::Transport);
        squad.members.insert(transport);
        squad.transport_entity = Some(transport);
        squad.target = Some(GridPosition { x: 8, y: 8 });
        squad.target_island = Some(island_id);
        squad.phase =
            crate::ai::squad::MissionPhase::Transport(crate::ai::squad::TransportPhase::Transit);
        world.insert_resource(manager);

        run_squad_beam_search(&mut world, p1);
        let manager = world.resource::<SquadManager>();
        assert_eq!(manager.squads[0].target, Some(GridPosition { x: 8, y: 8 }));
        assert_eq!(manager.squads[0].target_island, Some(island_id));
    }
}
