//! 拡張経路（小規模マップ戦略・非V4）の実装。
//!
//! 親モジュールはmain互換の実装を持ち、本文の異なる関数は
//! `crate::ai::code_path::is_extended()` でここへ委譲する。

#[allow(unused_imports)]
use super::*;

pub(super) fn analyze_strategy_internal(
    world: &mut World,
    player_id: PlayerId,
    cached_campaign: Option<IslandCampaignPortfolio>,
    reserved_entities: &HashSet<Entity>,
) -> ProductionStrategy {
    let mut strategy = ProductionStrategy::default();

    let mut unowned_properties = Vec::new();
    let mut my_properties = Vec::new();
    let mut enemy_properties = Vec::new();
    let mut my_capital_pos = None;

    let master_data = world
        .get_resource::<crate::resources::master_data::MasterDataRegistry>()
        .cloned()
        .unwrap_or_else(|| {
            crate::resources::master_data::MasterDataRegistry::load().unwrap_or_default()
        });
    let map = world.resource::<crate::resources::Map>().clone();
    let ai_version = crate::ai::resolve_player_ai_version(world, player_id);
    let is_v3 = ai_version.uses_v3_tactics();
    let is_v4 = ai_version.uses_operation_driven_production();
    if is_v3 {
        if let Some(cached) = cached_campaign {
            strategy.campaign_portfolio = cached;
        } else {
            // 通常分析では盤面から再構築し、同じ呼び出し内と診断出力で共有する。
            strategy.campaign_portfolio = if reserved_entities.is_empty() {
                analyze_island_campaign(world, player_id)
            } else {
                analyze_island_campaign_excluding(world, player_id, reserved_entities)
            };
            if is_v4 {
                // 小規模/通常の分岐はここで持たず、ターン入口で確定したパイプラインへ
                // portfolioを渡す。各層が別々に小規模作戦を有効化しないための境界である。
                crate::ai::v4::strategy_pipeline::prepare_strategy(
                    world,
                    player_id,
                    &mut strategy.campaign_portfolio,
                );
            }
            // 診断Resourceは意思決定に戻さず、最後の分析結果だけをプレイヤー別に上書きする。
            if let Some(mut diagnostics) = world.get_resource_mut::<IslandCampaignDiagnostics>() {
                diagnostics
                    .by_player
                    .insert(player_id, strategy.campaign_portfolio.clone());
            } else {
                let mut diagnostics = IslandCampaignDiagnostics::default();
                diagnostics
                    .by_player
                    .insert(player_id, strategy.campaign_portfolio.clone());
                world.insert_resource(diagnostics);
            }
        }
    }
    let mut turn_cache = TurnDistanceCache::default();
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
    let island_map = world
        .get_resource::<crate::ai::islands::IslandMap>()
        .cloned()
        .unwrap_or_else(|| crate::ai::islands::IslandMap::analyze(&map));

    let mut allowed_islands = std::collections::HashMap::new();
    for island in &island_map.islands {
        let allowed = crate::ai::planner::is_invasion_allowed(world, player_id, island.id, island);
        allowed_islands.insert(island.id, allowed);
    }

    let get_island_id =
        |pos: &GridPosition| -> Option<usize> { island_map.get_island_at(pos).map(|i| i.id.0) };

    let mut my_base_island_ids = std::collections::HashSet::new();

    // 1. 拠点の分析
    {
        let mut q_props = world.query::<(&GridPosition, &Property)>();
        for (pos, prop) in q_props.iter(world) {
            if prop.owner_id == Some(player_id) {
                my_properties.push(*pos);
                if prop.terrain == crate::resources::Terrain::Capital {
                    my_capital_pos = Some(*pos);
                }
                // 自軍の任意の拠点がある島IDを収集
                if let Some(island_id) = get_island_id(pos) {
                    my_base_island_ids.insert(island_id);
                }
            } else if prop.owner_id.is_none() {
                unowned_properties.push(*pos);
            } else {
                enemy_properties.push(*pos);
            }
        }
    }

    // unowned_properties / enemy_properties を strategy に保存
    strategy.unowned_properties = unowned_properties.iter().cloned().collect();
    strategy.enemy_properties = enemy_properties.iter().cloned().collect();

    let mut my_units = Vec::new();
    let mut enemy_units = Vec::new();

    // 2. ユニットの分析
    {
        let mut q_units = world.query::<(
            &GridPosition,
            &Faction,
            &UnitStats,
            Option<&crate::components::Transporting>,
        )>();
        for (pos, faction, stats, transporting) in q_units.iter(world) {
            // マップ外（輸送機内など）のユニットは距離計算などの分析から除外
            if pos.x >= 9999 || transporting.is_some() {
                continue;
            }
            if faction.0 == player_id {
                my_units.push((*pos, stats.clone()));
            } else {
                enemy_units.push((*pos, stats.clone()));
            }
        }
    }

    let capability_units = {
        let mut query = world.query::<(
            &GridPosition,
            &Faction,
            &UnitStats,
            &Health,
            Option<&Ammo>,
            Option<&Fuel>,
            Option<&ActionCompleted>,
            Option<&crate::components::Transporting>,
        )>();
        query
            .iter(world)
            .filter(|(position, _, _, health, _, _, _, transporting)| {
                position.x < 9999 && health.current > 0 && transporting.is_none()
            })
            .map(
                |(position, faction, stats, health, ammo, fuel, action_completed, _)| {
                    CombatCapabilitySnapshot {
                        faction: faction.0,
                        position: *position,
                        unit_type: stats.unit_type,
                        movement_type: stats.movement_type,
                        hp: health.current,
                        cost: stats.cost,
                        max_movement: stats.max_movement,
                        min_range: stats.min_range,
                        max_range: stats.max_range,
                        ammo1: ammo.map_or(99, |ammo| ammo.ammo1),
                        max_ammo1: stats.max_ammo1,
                        ammo2: ammo.map_or(99, |ammo| ammo.ammo2),
                        max_ammo2: stats.max_ammo2,
                        fuel: fuel.map_or(u32::MAX, |fuel| fuel.current),
                        action_delay: u32::from(
                            action_completed.is_some_and(|completed| completed.0),
                        ),
                    }
                },
            )
            .collect::<Vec<_>>()
    };

    // 自軍の歩兵が存在する島IDも収集対象に加える
    for (pos, stats) in &my_units {
        #[allow(clippy::collapsible_if)]
        if stats.can_capture {
            if let Some(island_id) = get_island_id(pos) {
                my_base_island_ids.insert(island_id);
            }
        }
    }

    let mut terrain_connectivity = TerrainConnectivity::default();

    // 交戦可能性の判定。地上部隊同士は、海や進入不能地形で分断された組を除外する。
    let mut min_enemy_dist = 999;
    for (m_pos, m_stats) in &my_units {
        for (e_pos, e_stats) in &enemy_units {
            if !can_form_ground_engagement(
                &map,
                &master_data,
                *m_pos,
                m_stats,
                *e_pos,
                e_stats,
                &mut terrain_connectivity,
            ) {
                continue;
            }
            let distance = map.distance(m_pos.x, m_pos.y, e_pos.x, e_pos.y) as i32;
            if distance < min_enemy_dist {
                min_enemy_dist = distance;
            }
        }
    }

    // 自軍平均移動力 + 射程 を閾値とする
    let avg_engagement_range = if !my_units.is_empty() {
        let total_reach: u32 = my_units
            .iter()
            .map(|(_, s)| s.max_movement + s.max_range)
            .sum();
        total_reach / my_units.len() as u32
    } else {
        5
    };

    let is_engaged = min_enemy_dist <= (avg_engagement_range + 1) as i32;

    // 3. フェーズの判定
    // 首都付近に敵がいるかチェック
    let mut capital_threatened = false;
    if let Some(cap_pos) = my_capital_pos {
        for (enemy_pos, enemy_stats) in &enemy_units {
            let enemy_id = player_id.opposite();
            let dist = calculate_turn_distance(
                &map,
                &master_data,
                &unit_positions,
                (enemy_pos.x, enemy_pos.y),
                (cap_pos.x, cap_pos.y),
                enemy_stats.movement_type,
                enemy_stats.max_movement,
                1,
                enemy_id,
                &mut turn_cache,
            );
            if dist.turns <= 2 {
                capital_threatened = true;
                break;
            }
        }
    }

    // --- 3.1 島嶼（IslandMap）と拠点獲得劣勢（収入不足）の分析 ---
    let mut island_unowned_properties = 0;
    for pos in &unowned_properties {
        if let Some(island_id) = get_island_id(pos)
            && my_base_island_ids.contains(&island_id)
        {
            island_unowned_properties += 1;
        }
    }

    let mut island_enemy_properties = 0;
    for pos in &enemy_properties {
        if let Some(island_id) = get_island_id(pos)
            && my_base_island_ids.contains(&island_id)
        {
            island_enemy_properties += 1;
        }
    }

    let mut island_my_capture_units = 0;
    for (pos, stats) in &my_units {
        if stats.can_capture
            && let Some(island_id) = get_island_id(pos)
            && my_base_island_ids.contains(&island_id)
        {
            island_my_capture_units += 1;
        }
    }

    // 島内の拠点数に基づく目標占領ユニット数
    let total_island_properties = island_unowned_properties + island_enemy_properties;
    let ideal_capture_units = ((total_island_properties as f32 * 0.5).ceil() as usize).clamp(3, 10);

    // 収入（拠点確保）優先判定: 島内に中立拠点が残っており、歩兵が目標未満である場合
    let need_more_revenue =
        island_unowned_properties > 0 && island_my_capture_units < ideal_capture_units;

    // フェーズの判定
    if capital_threatened {
        strategy.phase = GamePhase::Defense;
    } else if need_more_revenue {
        strategy.phase = GamePhase::Expansion;
    } else if is_engaged {
        if enemy_units.len() >= my_units.len() {
            strategy.phase = GamePhase::Contested;
        } else {
            strategy.phase = GamePhase::Assault;
        }
    } else {
        strategy.phase = GamePhase::Expansion;
    }

    // ターゲットの統合: フェーズに関わらず、中立拠点と敵拠点の両方を考慮する
    // ただしフェーズによって重みを変えるために、ここではリストの順序や内容を調整
    strategy.priority_targets = match strategy.phase {
        GamePhase::Expansion => {
            let mut targets = unowned_properties.clone();
            targets.extend(enemy_properties.iter().cloned());
            targets
        }
        GamePhase::Contested | GamePhase::Assault => {
            let mut targets = enemy_properties.clone();
            // 中立拠点も近いものはターゲットに含める
            targets.extend(unowned_properties.iter().cloned());
            targets
        }
        GamePhase::Defense => {
            if let Some(cap_pos) = my_capital_pos {
                vec![cap_pos]
            } else {
                enemy_properties.clone()
            }
        }
    };

    // 理想構成の適用
    match strategy.phase {
        GamePhase::Expansion => {
            strategy.ideal_composition.insert(UnitType::Infantry, 0.7);
            strategy.ideal_composition.insert(UnitType::Tank, 0.2);
            strategy.ideal_composition.insert(UnitType::Recon, 0.1);
        }
        GamePhase::Contested => {
            strategy.ideal_composition.insert(UnitType::Infantry, 0.4);
            strategy.ideal_composition.insert(UnitType::Tank, 0.4);
            strategy.ideal_composition.insert(UnitType::Artillery, 0.2);
        }
        GamePhase::Assault => {
            strategy.ideal_composition.insert(UnitType::Infantry, 0.2);
            strategy.ideal_composition.insert(UnitType::Tank, 0.6);
            strategy.ideal_composition.insert(UnitType::Artillery, 0.2);
        }
        GamePhase::Defense => {
            strategy.ideal_composition.insert(UnitType::Infantry, 0.5);

            // 対空の理想構成は「敵に航空戦力が実在する」ことを条件とする（V3限定）。
            // 対空砲・地対空ミサイルは地上ユニットへ一切ダメージを与えられないため、
            // 空港のないマップ（例: map_1）で対空需要を立てると死に駒を量産し、
            // 占領・機動地上戦力の生産枠と予算を奪って自軍を弱体化させる。
            // V1/V2 は評価の基準線として従来挙動のまま維持する。
            let enemy_has_air = enemy_units
                .iter()
                .any(|(_, stats)| stats.movement_type == MovementType::Air);
            if !is_v3 || enemy_has_air {
                strategy.ideal_composition.insert(UnitType::Tank, 0.3);
                strategy.ideal_composition.insert(UnitType::AntiAir, 0.2);
            } else {
                // 対空へ割り当てるはずだった需要は、防衛の主力である戦車へ振り替える
                strategy.ideal_composition.insert(UnitType::Tank, 0.5);
            }
        }
    }

    // 占領需要の計算
    // 自軍の島に未占領・敵拠点があるかどうかにかかわらず、マップ全体で占領すべき拠点があるなら歩兵を需要とする
    let total_unowned_or_enemy = unowned_properties.len() + enemy_properties.len();
    if total_unowned_or_enemy > 0 {
        // マップ全体での占領目標に対する歩兵的理想数（マップが広ければ多い）
        let ideal_capture_units_global =
            ((total_unowned_or_enemy as f32 * 0.4).ceil() as usize).clamp(3, 10);
        let total_my_capture_units = my_units.iter().filter(|(_, s)| s.can_capture).count();

        let base_demand = ideal_capture_units_global.saturating_sub(total_my_capture_units);

        // 収入不足（自軍の島の未占領拠点が残っているのに歩兵が足りない等）の場合はさらに上乗せ
        if need_more_revenue {
            strategy.capture_demand = (base_demand + 4).max(1) as u32;
        } else {
            strategy.capture_demand = base_demand as u32;
        }
    } else {
        strategy.capture_demand = 0;
    }

    // 包括的需要マトリクスの計算
    // 自軍・敵軍の状況から、占領脅威・消耗ギャップを数値化した需要ベクトル。
    {
        let damage_chart = world
            .get_resource::<crate::resources::DamageChart>()
            .cloned();
        let unit_registry = world
            .get_resource::<crate::resources::UnitRegistry>()
            .cloned();

        if let (Some(chart), Some(registry)) = (damage_chart, unit_registry) {
            // 自軍屠性を制限する：拠点の terrain を取得
            let my_props_for_demand: Vec<(GridPosition, Terrain)> = {
                let mut q = world.query::<(&GridPosition, &Property)>();
                q.iter(world)
                    .filter(|(_, p)| p.owner_id == Some(player_id))
                    .map(|(pos, p)| (*pos, p.terrain))
                    .collect()
            };
            strategy.demand = compute_demand(
                &my_units,
                &enemy_units,
                &my_props_for_demand,
                &chart,
                &registry,
            );
            if is_v3 {
                let critical_sites = my_props_for_demand
                    .iter()
                    .filter(|(_, terrain)| {
                        matches!(
                            terrain,
                            Terrain::Capital
                                | Terrain::Factory
                                | Terrain::Airport
                                | Terrain::Port
                                | Terrain::City
                        )
                    })
                    .map(|(position, _)| *position)
                    .collect::<Vec<_>>();
                strategy.air_defense = assess_air_defense(
                    player_id,
                    &capability_units,
                    &critical_sites,
                    &map,
                    &master_data,
                    &unit_positions,
                    &chart,
                );
                strategy.demand.anti_air = strategy.air_defense.shortage_ratio;
            }

            // 輸送が必要なユニット（停滞ユニット）的抽出。
            // V3はallocatorが確定した全攻勢を、割当優先順位どおりの輸送先として共有する。
            let map = world.resource::<crate::resources::Map>();
            let normalization_scale = average_attack_expectation(&chart, &registry);
            let transport_targets = if is_v3 {
                strategy.campaign_portfolio.offensive_target_positions()
            } else {
                strategy.priority_targets.clone()
            };
            for (pos, stats) in &my_units {
                // 陸上ユニットかつ、輸送能力を持たない戦闘/占領用ユニットのみ
                if matches!(
                    stats.movement_type,
                    MovementType::Infantry
                        | MovementType::Tank
                        | MovementType::ArmoredCar
                        | MovementType::Artillery
                ) && stats.max_cargo == 0
                {
                    // 実在する最寄り目標がある場合だけ輸送候補を評価し、目標なしを遠距離扱いしない。
                    let nearest_target = transport_targets
                        .iter()
                        .filter(|target| {
                            // V2 は既存の侵攻許可を維持する。V3 はallocator確定済み目標を使う。
                            is_v3
                                || island_map
                                    .get_island_at(target)
                                    .map(|island| {
                                        allowed_islands.get(&island.id).copied().unwrap_or(true)
                                    })
                                    .unwrap_or(true)
                        })
                        .map(|target| {
                            let distance = (pos.x as i32 - target.x as i32).abs()
                                + (pos.y as i32 - target.y as i32).abs();
                            (distance, *target)
                        })
                        .min_by_key(|(distance, _)| *distance);
                    let Some((min_dist, nearest_target)) = nearest_target else {
                        continue;
                    };

                    // IslandMap を使って異なる島かどうかを判定（海で遮断されているとみなす）
                    let my_island_id = get_island_id(pos);
                    let target_island_id = get_island_id(&nearest_target);
                    let mut blocked_by_sea = my_island_id.is_some()
                        && target_island_id.is_some()
                        && my_island_id != target_island_id;
                    if !blocked_by_sea {
                        // 同一島IDがない場合（海の上など）や判定できない場合は簡易パスサンプリングでフォールバック
                        let steps = 4;
                        for i in 1..steps {
                            let check_x =
                                pos.x as i32 + (nearest_target.x as i32 - pos.x as i32) * i / steps;
                            let check_y =
                                pos.y as i32 + (nearest_target.y as i32 - pos.y as i32) * i / steps;
                            if let Some(Terrain::Sea | Terrain::Shoal) =
                                map.get_terrain(check_x as usize, check_y as usize)
                            {
                                blocked_by_sea = true;
                                break;
                            }
                        }
                    }

                    // 輸送を検討すべき条件: 海で遮断されている、または距離が極端に遠い
                    if blocked_by_sea || min_dist > 15 {
                        let affinity = compute_unit_affinity(
                            stats.unit_type,
                            &chart,
                            &registry,
                            normalization_scale,
                        );
                        // 価値 = (需要との一致度 * 基本コスト相当) + (占領能力ボーナス)
                        let base_value = stats.cost as f32;
                        let value = strategy.demand.dot(&affinity) * base_value
                            + (if stats.can_capture { 3000.0 } else { 0.0 });

                        strategy
                            .transport_candidates
                            .push((*pos, stats.clone(), value));
                    }
                }
            }

            let (current_light_capacity, current_heavy_capacity) = if is_v3 {
                my_units
                    .iter()
                    .fold((0_u32, 0_u32), |capacity, (_, stats)| {
                        let sea_capacity = sea_transport_capacity(stats.unit_type, stats);
                        (
                            capacity.0.saturating_add(sea_capacity.0),
                            capacity.1.saturating_add(sea_capacity.1),
                        )
                    })
            } else {
                let light = my_units
                    .iter()
                    .filter(|(_, stats)| stats.loadable_unit_types.contains(&UnitType::Infantry))
                    .map(|(_, stats)| stats.max_cargo)
                    .sum();
                let heavy = my_units
                    .iter()
                    .filter(|(_, stats)| stats.loadable_unit_types.contains(&UnitType::Tank))
                    .map(|(_, stats)| stats.max_cargo)
                    .sum();
                (light, heavy)
            };

            // V3では海を越えられる輸送だけを数え、地上輸送車による需要減衰を防ぐ。
            strategy.existing_transport_count = if is_v3 {
                my_units
                    .iter()
                    .filter(|(_, stats)| {
                        let capacity = sea_transport_capacity(stats.unit_type, stats);
                        capacity.0 > 0 || capacity.1 > 0
                    })
                    .count()
            } else {
                my_units
                    .iter()
                    .filter(|(_, stats)| stats.max_cargo > 0)
                    .count()
            };

            // 軽ユニット（歩兵・バズーカ）と重ユニット（車両）の待機数をカウント
            let light_candidates = strategy
                .transport_candidates
                .iter()
                .filter(|(_, s, _)| {
                    s.unit_type == UnitType::Infantry || s.unit_type == UnitType::Mech
                })
                .count() as u32;
            let heavy_candidates = strategy
                .transport_candidates
                .iter()
                .filter(|(_, s, _)| {
                    s.unit_type != UnitType::Infantry && s.unit_type != UnitType::Mech
                })
                .count() as u32;

            strategy.light_transport_demand = light_candidates
                .saturating_sub(current_light_capacity)
                .max(if light_candidates > 0 && current_light_capacity == 0 {
                    1
                } else {
                    0
                });
            strategy.heavy_transport_demand = heavy_candidates
                .saturating_sub(current_heavy_capacity)
                .max(if heavy_candidates > 0 && current_heavy_capacity == 0 {
                    1
                } else {
                    0
                });

            // 海を越えた島への侵攻需要（Base Invasion Demand）の計算
            let has_sea_bound_target = if is_v3 {
                !strategy.campaign_portfolio.active_offensives.is_empty()
            } else {
                strategy.priority_targets.iter().any(|target| {
                    get_island_id(target).is_some_and(|target_island_id| {
                        !my_base_island_ids.contains(&target_island_id)
                    })
                })
            };

            if has_sea_bound_target {
                // 海を越えた侵攻目標がある場合、輸送需要のベースラインを保証する
                // 現存する輸送能力が0であれば、最低1を要求する
                if current_light_capacity == 0 {
                    strategy.light_transport_demand = strategy.light_transport_demand.max(1);
                }
                if current_heavy_capacity == 0 {
                    strategy.heavy_transport_demand = strategy.heavy_transport_demand.max(1);
                }

                // さらに、乗車させる部隊の需要ベースラインも保証する
                strategy.capture_demand = strategy.capture_demand.max(2);
                strategy.demand.anti_ground = strategy.demand.anti_ground.max(0.5);
            }
        }
    }

    if is_v3 {
        let campaign_owns_role_demand = !strategy.campaign_portfolio.defenses.is_empty()
            || !strategy.campaign_portfolio.active_offensives.is_empty();
        let shortfalls = strategy.campaign_portfolio.aggregate_missing_requirements();
        let mut capture_demand = 0_u32;
        let mut light_transport_demand = 0_u32;
        let mut heavy_transport_demand = 0_u32;

        // helperが保証するpriority rank・島ID順をfieldにも保持し、productionで順次消費する。
        for shortfall in &shortfalls {
            capture_demand = capture_demand.saturating_add(shortfall.capture_units);
            light_transport_demand =
                light_transport_demand.saturating_add(shortfall.light_transport_slots);
            heavy_transport_demand =
                heavy_transport_demand.saturating_add(shortfall.heavy_transport_slots);
        }

        // assignmentが存在する間はcampaign側が役割需要を所有する。完全編成済みでshortfallが
        // 0の場合もgeneric需要へ戻さず、同じ役割の重複生産を防ぐ。
        if campaign_owns_role_demand {
            strategy.capture_demand = capture_demand;
            strategy.light_transport_demand = light_transport_demand;
            strategy.heavy_transport_demand = heavy_transport_demand;
        }
        strategy.campaign_shortfalls = shortfalls;
    }

    // 平和な拠点（敵ユニットから十分に離れており、脅かされていない拠点）の分析
    let mut peaceful_properties = std::collections::HashSet::new();

    for pos in &my_properties {
        let prop_island_id = get_island_id(pos);
        let mut is_peaceful = true;

        for (e_pos, e_stats) in &enemy_units {
            let e_island_id = get_island_id(e_pos);
            let enemy_id = player_id.opposite();
            if enemy_threatens_property(
                &map,
                &master_data,
                &unit_positions,
                &mut turn_cache,
                enemy_id,
                pos,
                prop_island_id,
                e_pos,
                e_stats,
                e_island_id,
            ) {
                is_peaceful = false;
                break;
            }
        }

        if is_peaceful {
            peaceful_properties.insert(*pos);
        }
    }
    strategy.peaceful_properties = peaceful_properties;

    strategy
}
