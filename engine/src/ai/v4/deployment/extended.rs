//! 拡張経路（小規模マップ戦略・非V4）の実装。
//!
//! 親モジュールはmain互換の実装を持ち、本文の異なる関数は
//! `crate::ai::code_path::is_extended()` でここへ委譲する。

#[allow(unused_imports)]
use super::*;

/// 実際に生産されたEntityをpending deploymentへ照合する。
pub fn reconcile_pending_deployments_system(
    mut events: EventReader<UnitProducedEvent>,
    match_state: Res<MatchState>,
    mut registry: ResMut<V4DeploymentRegistry>,
    manager: Option<ResMut<SquadManager>>,
    island_map: Option<Res<IslandMap>>,
    mut contract_registry: Option<ResMut<PlanContractRegistry>>,
) {
    let turn = match_state.current_turn_number.0;
    let mut manager = manager;
    for event in events.read() {
        let slot = registry.assign_produced_extended(event, turn);
        if slot.is_some()
            && let Some(manager) = manager.as_deref_mut()
        {
            registry.resolve_produced_slot(event.entity, manager, island_map.as_deref());
        }
        if let Some(assigned) = registry.assigned.get(&event.entity)
            && let Some(binding) = assigned.intent.operation_binding
            && let Some(contracts) = contract_registry.as_deref_mut()
            && let Err(err) =
                contracts.reconcile_produced_binding(event.entity, binding, assigned.intent.anchor)
        {
            eprintln!("[WARN] Failed to reconcile produced binding: {:?}", err);
        }
    }
    // 失敗した生産命令を翌ターンの同型発注へ誤照合しない。
    registry.pending.retain(|pending| pending.turn >= turn);
}

/// 優先敵の現在位置を追跡し、消滅・到達不能時だけ局地敵へ再目標化する。
pub(super) fn resolve_target(
    world: &mut World,
    connectivity: &mut TerrainConnectivity,
    deployment: &AssignedDeployment,
    target_commitments: &HashMap<Entity, usize>,
    interceptors: usize,
) -> Option<(Entity, GridPosition, bool)> {
    let map = world.get_resource::<Map>().cloned();
    let anchor = deployment.intent.anchor;
    let player_id = deployment.intent.player_id;

    // 1. 防衛対象へのフリーパス侵入の阻止（壁役・迎撃役の緊急選出）:
    //    部隊全体を引き上げる必要はなく、壁役・迎撃役は最大1体（小規模マップ）で十分。
    //    すでに1体でも迎撃役が割り当てられているなら、過剰な引き返し（オーバーコミット）を防ぎ、
    //    主力の攻勢・各個撃破（Focus Fire）を維持する。
    //    また、壁役は能力値・ダメージ相性から「相手に倒されず、その手番で防ぎに向かえる」ユニットのみを選出する。
    let is_small_map = map.as_ref().is_some_and(|m| (m.width * m.height) <= 200)
        || crate::ai::strategy_profile::current_profile(world, player_id)
            == crate::ai::strategy_profile::V4StrategyProfile::SmallMapExpansion;
    // 上限は「実際に壁役へ割いた数」で数える。従来は目標を持つ全Entity数を合計して
    // いたため、誰か一体が通常の攻撃目標を得た時点で以後は誰も壁役になれず、
    // 前線に複数のフリーパス占領が同時に立つと2件目以降を塞げなかった。
    let max_interceptor_quota = if is_small_map { 1 } else { 2 };

    if interceptors < max_interceptor_quota
        && let (Some(m), Some(chart), Some(my_stats)) = (
            map.as_ref(),
            world.get_resource::<DamageChart>(),
            world.get::<UnitStats>(deployment.entity),
        )
    {
        let my_hp = world
            .get::<Health>(deployment.entity)
            .map_or(0, |h| h.current);
        let my_pos = world.get::<GridPosition>(deployment.entity).copied();

        // 壁役の適格条件（兵種ハードコードなし、能力値・状態から判定）:
        // 1. 直接反撃可能（近接で一方的に殴られない）
        // 2. 占領専任ユニットではない
        // 3. 健全な残HP（相手に倒されない）
        let can_act_as_wall =
            my_stats.min_range <= 1 && !my_stats.can_capture && my_hp >= 70 && my_pos.is_some();

        if can_act_as_wall && let Some(my_pos) = my_pos {
            let mut urgent_threats = deployment
                .intent
                .priority_enemies
                .iter()
                .copied()
                .filter(|&candidate| {
                    if target_commitments.get(&candidate).copied().unwrap_or(0) >= 1 {
                        return false;
                    }
                    let Some(candidate_stats) = world.get::<UnitStats>(candidate) else {
                        return false;
                    };
                    // 相手からの被ダメージが小さく、相手に倒されない（装甲・防御相性の判定）
                    let incoming_damage = chart
                        .get_base_damage(candidate_stats.unit_type, my_stats.unit_type)
                        .unwrap_or(0)
                        .max(
                            chart
                                .get_base_damage_secondary(
                                    candidate_stats.unit_type,
                                    my_stats.unit_type,
                                )
                                .unwrap_or(0),
                        );
                    if incoming_damage > 25 {
                        return false;
                    }

                    if let Some(pos) = world.get::<GridPosition>(candidate).copied()
                        && target_within_operation(world, deployment, candidate)
                        && can_engage(world, connectivity, deployment.entity, candidate)
                        && is_uncontested_capture_threat(
                            world, m, player_id, candidate, pos, anchor,
                        )
                    {
                        // 「そのターンで防ぎに向かえる」判定:
                        // 自機の移動力＋最大射程で、そのターン内に敵を捕捉・攻撃できる距離にいる
                        let dist_to_enemy = m.distance(my_pos.x, my_pos.y, pos.x, pos.y);
                        let reach_this_turn =
                            my_stats.max_movement.saturating_add(my_stats.max_range);
                        dist_to_enemy <= reach_this_turn
                    } else {
                        false
                    }
                })
                .collect::<Vec<_>>();

            if !urgent_threats.is_empty() {
                urgent_threats.sort_unstable_by_key(|&candidate| {
                    let c_pos = *world.get::<GridPosition>(candidate).unwrap();
                    let d_anchor = m.distance(anchor.x, anchor.y, c_pos.x, c_pos.y);
                    let d_unit = m.distance(my_pos.x, my_pos.y, c_pos.x, c_pos.y);
                    (d_anchor, d_unit, candidate.to_bits())
                });
                let best = urgent_threats[0];
                let pos = *world.get::<GridPosition>(best).unwrap();
                return Some((best, pos, true));
            }
        }
    }

    // 2. 緊急のフリーパス脅威がなければ、各個撃破（Focus Fire）の原則に従い、
    //    交戦可能な現標的を維持する。
    if let Some(enemy) = deployment.current_target
        && deployment.intent.priority_enemies.contains(&enemy)
        && target_within_operation(world, deployment, enemy)
        && can_engage(world, connectivity, deployment.entity, enemy)
        && let Some(position) = world.get::<GridPosition>(enemy)
    {
        return Some((enemy, *position, false));
    }

    // 3. 現標的が無効なら、購入時に比較した初撃相手の順序で選ぶ。
    //    anchorからの距離による再選定は、実経路・期限の評価を別の前線へ置き換える。
    //    到達不能な相手と、既に担当済みの占領兵は次の候補へ進む。
    for enemy in deployment.intent.priority_enemies.iter().copied() {
        let already_covered = world
            .get::<UnitStats>(enemy)
            .is_some_and(|stats| stats.can_capture)
            && target_commitments.get(&enemy).copied().unwrap_or(0) >= 1;
        if already_covered
            || !target_within_operation(world, deployment, enemy)
            || !can_engage(world, connectivity, deployment.entity, enemy)
        {
            continue;
        }
        if let Some(position) = world.get::<GridPosition>(enemy) {
            return Some((enemy, *position, false));
        }
    }

    local_retarget(world, connectivity, deployment).and_then(|enemy| {
        world
            .get::<GridPosition>(enemy)
            .copied()
            .map(|position| (enemy, position, false))
    })
}

/// 生産済みEntityを汎用free poolより先にV4局地Attack任務へ予約する。
///
/// 島嶼キャンペーンで既に予約されたEntityは既存作戦を優先し、次ターン再試行する。
pub(crate) fn prepare_deployment_squads(
    world: &mut World,
    manager: &mut SquadManager,
    player_id: PlayerId,
    higher_priority_reserved: &HashSet<Entity>,
) -> HashSet<Entity> {
    let mut registry = world
        .remove_resource::<V4DeploymentRegistry>()
        .unwrap_or_default();
    let mut connectivity = TerrainConnectivity::default();
    let mut reserved = HashSet::new();
    let mut releases = Vec::new();
    let island_map = world
        .get_resource::<IslandMap>()
        .cloned()
        .or_else(|| world.get_resource::<Map>().map(IslandMap::analyze));
    let mut entities: Vec<_> = registry
        .assigned
        .iter()
        .filter(|(_, deployment)| deployment.intent.player_id == player_id && deployment.active)
        .map(|(&entity, _)| entity)
        .collect();
    entities.sort_unstable_by_key(|entity| entity.to_bits());
    let mut target_commitments: HashMap<Entity, usize> = HashMap::new();
    let mut interceptors = 0_usize;

    for entity in entities {
        let Some(snapshot) = registry.assigned.get(&entity).cloned() else {
            continue;
        };
        if world.get_entity(entity).is_err() {
            releases.push((entity, snapshot.squad_id));
            continue;
        }
        if higher_priority_reserved.contains(&entity) {
            if let Some(deployment) = registry.assigned.get_mut(&entity) {
                deployment.current_target = None;
            }
            continue;
        }
        let (target_entity, target, mission_type) = match snapshot.intent.posture {
            // 首都攻略パッケージが揃う前は、個々の生産Entityを敵地へ逐次投入しない。
            DeploymentPosture::Forming => {
                (None, snapshot.intent.staging_anchor, MissionType::Defense)
            }
            DeploymentPosture::Execute => {
                // 占領スロットの場合はターゲット解決を行わず、直接占領任務を割り当てる
                if snapshot.intent.slot_kind == SlotKind::Capture {
                    // anchor物件が既に自軍所有なら占領任務は達成済み。任務を出し続けると
                    // 占領役が物件上に留まり続け、生産マス前の通路を塞ぐため解放する。
                    if anchor_is_owned_by(world, snapshot.intent.anchor, player_id) {
                        releases.push((entity, snapshot.squad_id));
                        continue;
                    }
                    (None, snapshot.intent.anchor, MissionType::Capture)
                } else {
                    match resolve_target(
                        world,
                        &mut connectivity,
                        &snapshot,
                        &target_commitments,
                        interceptors,
                    ) {
                        Some((target_entity, target, is_wall)) => {
                            *target_commitments.entry(target_entity).or_insert(0) += 1;
                            if is_wall {
                                interceptors += 1;
                            }
                            (Some(target_entity), target, MissionType::Attack)
                        }
                        // Combat排除後も対象拠点の占領完了まではPlanを閉じない。
                        // 生産戦力をfree poolへ返さず、anchorで反撃増援を待ち受ける。
                        None if snapshot.intent.plan_step.is_some() => {
                            (None, snapshot.intent.anchor, MissionType::Defense)
                        }
                        None => {
                            releases.push((entity, snapshot.squad_id));
                            continue;
                        }
                    }
                }
            }
        };

        // 過去の汎用Squadへ混入していた場合は、このEntityだけを切り離す。
        for squad in &mut manager.squads {
            if squad.owner_id == Some(player_id) && Some(squad.id) != snapshot.squad_id {
                squad.members.remove(&entity);
            }
        }
        let squad_index = snapshot.squad_id.and_then(|id| {
            manager
                .squads
                .iter()
                .position(|squad| squad.id == id && squad.owner_id == Some(player_id))
        });
        let squad = if let Some(index) = squad_index {
            &mut manager.squads[index]
        } else {
            manager.create_owned_squad(MissionType::Attack, player_id)
        };
        squad.members.clear();
        squad.members.insert(entity);
        squad.mission_type = mission_type;
        squad.target = Some(target);
        // 生産時Planと同じ島campaign ownerをSquadにも持たせる。Noneの汎用Squadに
        // すると排他reconcileで正本campaign ownerと競合し、任務が断続的に消える。
        squad.target_island = island_map
            .as_ref()
            .and_then(|islands| islands.get_island_at(&snapshot.intent.anchor))
            .map(|island| island.id);
        squad.phase = MissionPhase::MovingToTarget;
        let squad_id = squad.id;
        if let Some(deployment) = registry.assigned.get_mut(&entity) {
            deployment.squad_id = Some(squad_id);
            deployment.current_target = target_entity;
        }
        reserved.insert(entity);
    }

    for (entity, squad_id) in releases {
        if let Some(deployment) = registry.assigned.get_mut(&entity) {
            deployment.active = false;
            deployment.squad_id = None;
            deployment.current_target = None;
        }
        if let Some(squad_id) = squad_id {
            manager.squads.retain(|squad| squad.id != squad_id);
        }
    }
    world.insert_resource(registry);
    reserved
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::islands::IslandMap;
    use crate::resources::{GridTopology, MovementType, Terrain};

    fn pending(order: u32) -> PendingDeployment {
        PendingDeployment {
            player_id: PlayerId(1),
            turn: 3,
            order,
            facility: GridPosition { x: 2, y: 4 },
            unit_type: UnitType::Fighter,
            anchor: GridPosition { x: 8, y: 8 },
            staging_anchor: GridPosition { x: 2, y: 4 },
            posture: DeploymentPosture::Execute,
            slot_kind: SlotKind::Combat,
            priority_enemies: vec![Entity::from_raw(order + 10)],
            threat_horizon: 4,
            forecast: DeploymentForecast::default(),
            plan_step: None,
            forming_slot: None,
            operation_binding: None,
        }
    }

    #[test]
    fn produced_entity_resolves_the_forming_squad_slot_immediately() {
        let player = PlayerId(1);
        let mut registry = V4DeploymentRegistry::default();
        registry.replace_turn_orders(player, 3, [pending(0)]);
        let entity = Entity::from_raw(100);
        let slot = registry
            .assign_produced_extended(
                &UnitProducedEvent {
                    player_id: player,
                    target_x: 2,
                    target_y: 4,
                    unit_type: UnitType::Fighter,
                    entity,
                },
                3,
            )
            .expect("発注時にslotを予約する");
        let mut manager = SquadManager::default();

        registry.resolve_produced_slot(entity, &mut manager, None);

        let squad_id = registry.assigned[&entity]
            .squad_id
            .expect("生産Eventと同じ処理でSquadへ解決する");
        assert_eq!(registry.forming_slots[&slot].entity, Some(entity));
        assert_eq!(registry.forming_slots[&slot].squad_id, Some(squad_id));
        assert!(
            manager
                .squads
                .iter()
                .any(|squad| squad.id == squad_id && squad.members.contains(&entity))
        );
    }

    fn combat_stats(unit_type: UnitType, can_capture: bool, max_cargo: u32) -> UnitStats {
        UnitStats {
            unit_type,
            movement_type: MovementType::Infantry,
            max_movement: 3,
            max_ammo1: 9,
            can_capture,
            max_cargo,
            cost: 5_000,
            ..UnitStats::mock()
        }
    }

    fn deployment_world() -> (World, Entity, Entity, Entity, Entity) {
        let mut world = World::new();
        world.insert_resource(Map::new(10, 1, Terrain::Plains, GridTopology::Square));
        world.insert_resource(MasterDataRegistry::load().unwrap());
        let mut chart = DamageChart::new();
        chart.insert_damage(UnitType::Tank, UnitType::Infantry, 60);
        chart.insert_damage(UnitType::Tank, UnitType::TransportHelicopter, 40);
        world.insert_resource(chart);
        let attacker = world
            .spawn((
                Faction(PlayerId(1)),
                GridPosition { x: 0, y: 0 },
                combat_stats(UnitType::Tank, false, 0),
                Ammo {
                    ammo1: 9,
                    max_ammo1: 9,
                    ammo2: 0,
                    max_ammo2: 0,
                },
            ))
            .id();
        let priority = world
            .spawn((
                Faction(PlayerId(2)),
                GridPosition { x: 3, y: 0 },
                combat_stats(UnitType::Infantry, true, 0),
            ))
            .id();
        // より近い輸送より占領可能ユニットを局地fallbackで優先する。
        let transport = world
            .spawn((
                Faction(PlayerId(2)),
                GridPosition { x: 2, y: 0 },
                combat_stats(UnitType::TransportHelicopter, false, 2),
            ))
            .id();
        let capture = world
            .spawn((
                Faction(PlayerId(2)),
                GridPosition { x: 4, y: 0 },
                combat_stats(UnitType::Infantry, true, 0),
            ))
            .id();
        let mut registry = V4DeploymentRegistry::default();
        registry.assigned.insert(
            attacker,
            AssignedDeployment {
                entity: attacker,
                intent: PendingDeployment {
                    player_id: PlayerId(1),
                    turn: 3,
                    order: 0,
                    facility: GridPosition { x: 0, y: 0 },
                    unit_type: UnitType::Tank,
                    anchor: GridPosition { x: 5, y: 0 },
                    staging_anchor: GridPosition { x: 0, y: 0 },
                    posture: DeploymentPosture::Execute,
                    slot_kind: SlotKind::Combat,
                    priority_enemies: vec![priority],
                    threat_horizon: 3,
                    forecast: DeploymentForecast::default(),
                    plan_step: None,
                    forming_slot: None,
                    operation_binding: None,
                },
                squad_id: None,
                current_target: None,
                active: true,
                assigned_turn: 3,
                attack_count: 0,
                priority_attack_count: 0,
                mission_target_attack_count: 0,
                mission_target_kill_count: 0,
                mission_target_damage_value_dealt: 0,
                mission_target_counter_value_received: 0,
                mission_target_destroyed_value: 0,
                mission_target_first_attack_turn: None,
                capture_unit_attack_count: 0,
                transport_unit_attack_count: 0,
                kill_count: 0,
                damage_value_dealt: 0,
                counter_value_received: 0,
                destroyed_value: 0,
                first_attack_turn: None,
            },
        );
        world.insert_resource(registry);
        (world, attacker, priority, transport, capture)
    }

    #[test]
    fn capture_slot_releases_entity_once_anchor_is_already_owned() {
        let (mut world, attacker, _, _, _) = deployment_world();
        let anchor = GridPosition { x: 5, y: 0 };
        // 占領スロットのanchor物件は既に自軍所有になっている。
        world.spawn((anchor, Property::new(Terrain::City, Some(PlayerId(1)), 20)));
        {
            let mut registry = world.resource_mut::<V4DeploymentRegistry>();
            let deployment = registry.assigned.get_mut(&attacker).unwrap();
            deployment.intent.slot_kind = SlotKind::Capture;
            deployment.intent.anchor = anchor;
        }
        let mut manager = SquadManager::default();

        let reserved =
            prepare_deployment_squads(&mut world, &mut manager, PlayerId(1), &HashSet::new());

        // 占領済みの物件に占領任務を出し続けると、生産マス前の都市に歩兵が居座り
        // 後続の装甲が出撃できなくなる。任務は完了扱いで解放する。
        assert!(!reserved.contains(&attacker));
        assert!(!world.resource::<V4DeploymentRegistry>().assigned[&attacker].active);
        assert!(
            !manager
                .squads
                .iter()
                .any(|squad| squad.members.contains(&attacker))
        );
    }

    #[test]
    fn capture_slot_keeps_capture_mission_while_anchor_is_not_owned() {
        let (mut world, attacker, _, _, _) = deployment_world();
        let anchor = GridPosition { x: 5, y: 0 };
        world.spawn((anchor, Property::new(Terrain::City, None, 20)));
        {
            let mut registry = world.resource_mut::<V4DeploymentRegistry>();
            let deployment = registry.assigned.get_mut(&attacker).unwrap();
            deployment.intent.slot_kind = SlotKind::Capture;
            deployment.intent.anchor = anchor;
        }
        let mut manager = SquadManager::default();

        let reserved =
            prepare_deployment_squads(&mut world, &mut manager, PlayerId(1), &HashSet::new());

        assert!(reserved.contains(&attacker));
        let squad = manager
            .squads
            .iter()
            .find(|squad| squad.members.contains(&attacker))
            .unwrap();
        assert_eq!(squad.mission_type, MissionType::Capture);
        assert_eq!(squad.target, Some(anchor));
    }

    #[test]
    fn first_assignment_keeps_the_production_planners_target_order() {
        let (mut world, attacker, planned, _, anchor_nearest) = deployment_world();
        world
            .resource_mut::<V4DeploymentRegistry>()
            .assigned
            .get_mut(&attacker)
            .unwrap()
            .intent
            .priority_enemies = vec![planned, anchor_nearest];
        let mut manager = SquadManager::default();
        prepare_deployment_squads(&mut world, &mut manager, PlayerId(1), &HashSet::new());
        assert_eq!(
            world
                .resource::<V4DeploymentRegistry>()
                .attack_target(attacker),
            Some(planned),
            "購入時に初撃を割り当てた敵を、作戦中心からの距離で選び直さない"
        );
    }

    #[test]
    fn local_retarget_does_not_leave_the_operation_island() {
        let mut world = World::new();
        let mut map = Map::new(7, 1, Terrain::Plains, GridTopology::Square);
        map.set_terrain(2, 0, Terrain::Sea).unwrap();
        world.insert_resource(IslandMap::analyze(&map));
        world.insert_resource(map);
        world.insert_resource(MasterDataRegistry::load().unwrap());
        let mut chart = DamageChart::new();
        chart.insert_damage(UnitType::Bcopters, UnitType::Infantry, 60);
        world.insert_resource(chart);
        let mut attacker_stats = combat_stats(UnitType::Bcopters, false, 0);
        attacker_stats.movement_type = MovementType::Air;
        attacker_stats.max_movement = 6;
        let attacker = world
            .spawn((
                Faction(PlayerId(1)),
                GridPosition { x: 5, y: 0 },
                attacker_stats,
                Ammo {
                    ammo1: 9,
                    max_ammo1: 9,
                    ammo2: 0,
                    max_ammo2: 0,
                },
            ))
            .id();
        let remote_enemy = world
            .spawn((
                Faction(PlayerId(2)),
                GridPosition { x: 1, y: 0 },
                combat_stats(UnitType::Infantry, true, 0),
            ))
            .id();
        let mut deployment = AssignedDeployment {
            entity: attacker,
            intent: PendingDeployment {
                player_id: PlayerId(1),
                turn: 3,
                order: 0,
                facility: GridPosition { x: 5, y: 0 },
                unit_type: UnitType::Bcopters,
                anchor: GridPosition { x: 5, y: 0 },
                staging_anchor: GridPosition { x: 5, y: 0 },
                posture: DeploymentPosture::Execute,
                slot_kind: SlotKind::Combat,
                priority_enemies: Vec::new(),
                threat_horizon: 4,
                forecast: DeploymentForecast::default(),
                plan_step: None,
                forming_slot: None,
                operation_binding: None,
            },
            squad_id: None,
            current_target: None,
            active: true,
            assigned_turn: 3,
            attack_count: 0,
            priority_attack_count: 0,
            mission_target_attack_count: 0,
            mission_target_kill_count: 0,
            mission_target_damage_value_dealt: 0,
            mission_target_counter_value_received: 0,
            mission_target_destroyed_value: 0,
            mission_target_first_attack_turn: None,
            capture_unit_attack_count: 0,
            transport_unit_attack_count: 0,
            kill_count: 0,
            damage_value_dealt: 0,
            counter_value_received: 0,
            destroyed_value: 0,
            first_attack_turn: None,
        };

        assert_eq!(
            local_retarget(&mut world, &mut TerrainConnectivity::default(), &deployment),
            None,
            "別島の敵を局地fallbackで追跡してはならない"
        );

        let local_enemy = world
            .spawn((
                Faction(PlayerId(2)),
                GridPosition { x: 6, y: 0 },
                combat_stats(UnitType::Infantry, true, 0),
            ))
            .id();
        deployment.intent.priority_enemies = vec![remote_enemy];
        deployment.current_target = Some(remote_enemy);
        assert_eq!(
            resolve_target(
                &mut world,
                &mut TerrainConnectivity::default(),
                &deployment,
                &HashMap::new(),
                0,
            )
            .map(|target| target.0),
            Some(local_enemy),
            "生産時の優先敵が別島へ移動したら同じ島の敵へ再目標化する"
        );
    }

    #[test]
    fn islandless_target_must_be_interceptable_by_the_assigned_unit() {
        let mut world = World::new();
        let mut map = Map::new(7, 1, Terrain::Plains, GridTopology::Square);
        for x in 2..7 {
            map.set_terrain(x, 0, Terrain::Sea).unwrap();
        }
        world.insert_resource(IslandMap::analyze(&map));
        world.insert_resource(map);
        world.insert_resource(MasterDataRegistry::load().unwrap());
        let mut chart = DamageChart::new();
        chart.insert_damage(UnitType::Bcopters, UnitType::Bcopters, 40);
        world.insert_resource(chart);

        let mut attacker_stats = combat_stats(UnitType::Bcopters, false, 0);
        attacker_stats.movement_type = MovementType::Air;
        attacker_stats.max_movement = 1;
        attacker_stats.max_range = 1;
        let attacker = world
            .spawn((
                Faction(PlayerId(1)),
                GridPosition { x: 0, y: 0 },
                attacker_stats,
            ))
            .id();
        let mut enemy_stats = combat_stats(UnitType::Bcopters, false, 0);
        enemy_stats.movement_type = MovementType::Air;
        enemy_stats.max_movement = 6;
        let remote_enemy = world
            .spawn((
                Faction(PlayerId(2)),
                GridPosition { x: 6, y: 0 },
                enemy_stats.clone(),
            ))
            .id();
        let deployment = AssignedDeployment {
            entity: attacker,
            intent: PendingDeployment {
                player_id: PlayerId(1),
                turn: 3,
                order: 0,
                facility: GridPosition { x: 0, y: 0 },
                unit_type: UnitType::Bcopters,
                anchor: GridPosition { x: 0, y: 0 },
                staging_anchor: GridPosition { x: 0, y: 0 },
                posture: DeploymentPosture::Execute,
                slot_kind: SlotKind::Combat,
                priority_enemies: vec![remote_enemy],
                threat_horizon: 2,
                forecast: DeploymentForecast::default(),
                plan_step: None,
                forming_slot: None,
                operation_binding: None,
            },
            squad_id: None,
            current_target: Some(remote_enemy),
            active: true,
            assigned_turn: 3,
            attack_count: 0,
            priority_attack_count: 0,
            mission_target_attack_count: 0,
            mission_target_kill_count: 0,
            mission_target_damage_value_dealt: 0,
            mission_target_counter_value_received: 0,
            mission_target_destroyed_value: 0,
            mission_target_first_attack_turn: None,
            capture_unit_attack_count: 0,
            transport_unit_attack_count: 0,
            kill_count: 0,
            damage_value_dealt: 0,
            counter_value_received: 0,
            destroyed_value: 0,
            first_attack_turn: None,
        };

        assert!(!target_within_operation(&world, &deployment, remote_enemy));
        assert_eq!(
            local_retarget(&mut world, &mut TerrainConnectivity::default(), &deployment),
            None
        );

        let nearby_enemy = world
            .spawn((
                Faction(PlayerId(2)),
                GridPosition { x: 2, y: 0 },
                enemy_stats,
            ))
            .id();
        assert_eq!(
            local_retarget(&mut world, &mut TerrainConnectivity::default(), &deployment),
            Some(nearby_enemy)
        );
    }
}

/// 生産命令時点で確保する、実Entityをまだ持たないSquad受入先の値オブジェクト。
///
/// Squad IDそのものはSquadManagerだけが発番する。ここでは発注の意図を安定IDで
/// 予約し、生産完了Eventで実Squadへ一度だけ解決する。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct FormingSquadSlotId(u64);

/// まだ実体が無いSquad slotの予約状態。発注と生産完了のあいだも作戦所有権を保つ。
#[derive(Debug, Clone)]
pub(super) struct FormingSquadReservation {
    id: FormingSquadSlotId,
    player_id: PlayerId,
    turn: u32,
    squad_id: Option<SquadId>,
    entity: Option<Entity>,
}

/// 敵の占領歩兵が、自軍の邪魔（物理ブロック）もZoC足止めも受けずに
/// 拠点群へフリーパスで侵入・占領できる状態（無防備な脅威）であるかを判定する。
pub(super) fn is_uncontested_capture_threat(
    world: &World,
    map: &Map,
    player_id: PlayerId,
    enemy_entity: Entity,
    enemy_pos: GridPosition,
    anchor_pos: GridPosition,
) -> bool {
    let Some(enemy_stats) = world.get::<UnitStats>(enemy_entity) else {
        return false;
    };
    if !enemy_stats.can_capture {
        return false;
    }

    // 敵歩兵が向かっている防衛作戦圏内の自軍・未所有物件を探す
    let threatened_property = world.iter_entities().find_map(|e| {
        let pos = *e.get::<GridPosition>()?;
        let prop = e.get::<Property>()?;
        if prop.max_capture_points == 0 {
            return None;
        }
        // 敵自身がすでに所有している物件は防衛対象ではない
        if prop.owner_id.is_some_and(|o| o != player_id) {
            return None;
        }
        let dist_to_enemy = map.distance(pos.x, pos.y, enemy_pos.x, enemy_pos.y);
        let dist_to_anchor = map.distance(pos.x, pos.y, anchor_pos.x, anchor_pos.y);
        // 作戦anchorと同じセクター内にあり、敵歩兵が次ターン直接侵入・占領可能な物件（1ターン圏内）
        (dist_to_anchor <= 8 && dist_to_enemy <= enemy_stats.max_movement).then_some(pos)
    });

    let Some(target_property) = threatened_property else {
        return false;
    };

    // 1. その物件そのものに自軍ユニットが乗っていれば、無血侵入は物理的に阻止されている
    let prop_occupied_by_friendly = world.iter_entities().any(|e| {
        e.get::<Faction>().is_some_and(|f| f.0 == player_id)
            && e.get::<GridPosition>() == Some(&target_property)
            && e.get::<Health>().is_some_and(|h| h.current > 0)
    });
    if prop_occupied_by_friendly {
        return false;
    }

    // 2. 敵歩兵に隣接してZoCをかけている（または交戦中の）自軍ユニットがいるか
    let engaged_by_friendly_zoc = world.iter_entities().any(|e| {
        e.get::<Faction>().is_some_and(|f| f.0 == player_id)
            && e.get::<Health>().is_some_and(|h| h.current > 0)
            && e.get::<GridPosition>()
                .is_some_and(|pos| map.distance(pos.x, pos.y, enemy_pos.x, enemy_pos.y) <= 1)
    });
    if engaged_by_friendly_zoc {
        return false;
    }

    // 物件上に味方もおらず、敵歩兵にZoCをかけている味方もいない -> 完全なフリーパス状態！
    true
}

/// 指定座標の物件が既に `player_id` の所有かどうか。
pub(super) fn anchor_is_owned_by(world: &World, anchor: GridPosition, player_id: PlayerId) -> bool {
    world.iter_entities().any(|entity| {
        entity.get::<GridPosition>() == Some(&anchor)
            && entity
                .get::<Property>()
                .is_some_and(|property| property.owner_id == Some(player_id))
    })
}

impl V4DeploymentRegistry {
    /// 同じ手番に再計画した古い発注を除き、新しい発注意図を順番どおり登録する。
    pub(crate) fn replace_turn_orders_extended(
        &mut self,
        player_id: PlayerId,
        turn: u32,
        orders: impl IntoIterator<Item = PendingDeployment>,
    ) {
        self.pending
            .retain(|pending| pending.player_id != player_id || pending.turn != turn);
        // 同一手番の再計画で未発行slotを残すと、取り消された生産先へ後続Eventを
        // 誤接続する。その手番の予約はPendingと同じ粒度で置換する。
        self.forming_slots.retain(|_, reservation| {
            reservation.player_id != player_id
                || reservation.turn != turn
                || reservation.entity.is_some()
        });
        let mut normalized = orders.into_iter().collect::<Vec<_>>();
        normalized.sort_unstable_by_key(|pending| pending.order);
        for pending in &mut normalized {
            let slot = pending.forming_slot.unwrap_or_else(|| {
                let id = FormingSquadSlotId(self.next_forming_slot);
                self.next_forming_slot = self.next_forming_slot.saturating_add(1);
                id
            });
            pending.forming_slot = Some(slot);
            self.forming_slots
                .entry(slot)
                .or_insert(FormingSquadReservation {
                    id: slot,
                    player_id: pending.player_id,
                    turn: pending.turn,
                    squad_id: None,
                    entity: None,
                });
        }
        self.pending.extend(normalized);
    }

    /// 生産完了イベントを、同じ手番・施設・兵種の最初の発注へ決定的に照合する。
    pub(super) fn assign_produced_extended(
        &mut self,
        event: &UnitProducedEvent,
        turn: u32,
    ) -> Option<FormingSquadSlotId> {
        let index = self.pending.iter().position(|pending| {
            pending.player_id == event.player_id
                && pending.turn == turn
                && pending.facility.x == event.target_x
                && pending.facility.y == event.target_y
                && pending.unit_type == event.unit_type
        })?;
        let intent = self
            .pending
            .remove(index)
            .expect("照合済みpending deploymentが存在する");
        let forming_slot = intent.forming_slot;
        self.assigned.insert(
            event.entity,
            AssignedDeployment {
                entity: event.entity,
                intent,
                squad_id: None,
                current_target: None,
                active: true,
                assigned_turn: turn,
                attack_count: 0,
                priority_attack_count: 0,
                mission_target_attack_count: 0,
                mission_target_kill_count: 0,
                mission_target_damage_value_dealt: 0,
                mission_target_counter_value_received: 0,
                mission_target_destroyed_value: 0,
                mission_target_first_attack_turn: None,
                capture_unit_attack_count: 0,
                transport_unit_attack_count: 0,
                kill_count: 0,
                damage_value_dealt: 0,
                counter_value_received: 0,
                destroyed_value: 0,
                first_attack_turn: None,
            },
        );
        if let Some(slot) = forming_slot
            && let Some(reservation) = self.forming_slots.get_mut(&slot)
        {
            reservation.entity = Some(event.entity);
        }
        forming_slot
    }

    /// Combat見積で既存戦力として数えてよいEntityと、その実任務の対象を返す。
    ///
    /// Entityだけを返すと、1機を複数の島作戦へ同時に計上できてしまう。生産時の
    /// 優先敵と現在の再目標を併せて返し、対象敵が属する1作戦だけへ参加させる。
    pub(crate) fn active_target_assignments_extended(
        &self,
        player_id: PlayerId,
    ) -> HashMap<Entity, ActiveTargetAssignment> {
        self.assigned
            .values()
            .filter(|deployment| deployment.intent.player_id == player_id && deployment.active)
            .map(|deployment| {
                let mut targets = deployment
                    .intent
                    .priority_enemies
                    .iter()
                    .copied()
                    .collect::<HashSet<_>>();
                targets.extend(deployment.current_target);
                (
                    deployment.entity,
                    ActiveTargetAssignment {
                        plan_id: deployment.intent.plan_step.map(|step| step.plan_id),
                        slot_kind: deployment.intent.slot_kind,
                        targets,
                    },
                )
            })
            .collect()
    }

    /// 完了・撤回された計画の局地任務保護を解除し、上位防衛や汎用任務へ返す。
    pub(crate) fn release_closed_plans_extended(
        &mut self,
        plan_ids: &HashSet<super::super::plan_revision::PlanId>,
    ) {
        if plan_ids.is_empty() {
            return;
        }
        let released_slots = self
            .pending
            .iter()
            .filter(|pending| {
                pending
                    .plan_step
                    .is_some_and(|step| plan_ids.contains(&step.plan_id))
            })
            .filter_map(|pending| pending.forming_slot)
            .collect::<HashSet<_>>();
        self.pending.retain(|pending| {
            pending
                .plan_step
                .is_none_or(|step| !plan_ids.contains(&step.plan_id))
        });
        self.forming_slots.retain(|slot, reservation| {
            !released_slots.contains(slot) || reservation.entity.is_some()
        });
        for deployment in self.assigned.values_mut() {
            if deployment
                .intent
                .plan_step
                .is_some_and(|step| plan_ids.contains(&step.plan_id))
            {
                deployment.active = false;
                deployment.current_target = None;
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn assign_target_for_test_extended(
        &mut self,
        player_id: PlayerId,
        attacker: Entity,
        target: Entity,
    ) {
        self.assigned.insert(
            attacker,
            AssignedDeployment {
                entity: attacker,
                intent: PendingDeployment {
                    player_id,
                    turn: 1,
                    order: 0,
                    facility: GridPosition { x: 0, y: 0 },
                    unit_type: UnitType::Fighter,
                    anchor: GridPosition { x: 0, y: 0 },
                    staging_anchor: GridPosition { x: 0, y: 0 },
                    posture: DeploymentPosture::Execute,
                    slot_kind: SlotKind::Combat,
                    priority_enemies: vec![target],
                    threat_horizon: 1,
                    forecast: DeploymentForecast::default(),
                    plan_step: None,
                    forming_slot: None,
                    operation_binding: None,
                },
                squad_id: None,
                current_target: Some(target),
                active: true,
                assigned_turn: 1,
                attack_count: 0,
                priority_attack_count: 0,
                mission_target_attack_count: 0,
                mission_target_kill_count: 0,
                mission_target_damage_value_dealt: 0,
                mission_target_counter_value_received: 0,
                mission_target_destroyed_value: 0,
                mission_target_first_attack_turn: None,
                capture_unit_attack_count: 0,
                transport_unit_attack_count: 0,
                kill_count: 0,
                damage_value_dealt: 0,
                counter_value_received: 0,
                destroyed_value: 0,
                first_attack_turn: None,
            },
        );
    }

    /// 生産完了直後に予約slotを実Squadへ解決する。
    ///
    /// 実Entityは生成手番に行動できないが、次の再編まで`None`所属にしない。対象敵の
    /// 再探索など戦術的な詳細は次手番の`prepare_deployment_squads`が更新する。
    fn resolve_produced_slot(
        &mut self,
        entity: Entity,
        manager: &mut SquadManager,
        island_map: Option<&IslandMap>,
    ) {
        let Some(snapshot) = self.assigned.get(&entity).cloned() else {
            return;
        };
        let Some(slot) = snapshot.intent.forming_slot else {
            return;
        };
        let existing = self
            .forming_slots
            .get(&slot)
            .and_then(|reservation| reservation.squad_id)
            .and_then(|squad_id| {
                manager.squads.iter().position(|squad| {
                    squad.id == squad_id && squad.owner_id == Some(snapshot.intent.player_id)
                })
            });
        let squad = if let Some(index) = existing {
            &mut manager.squads[index]
        } else {
            manager.create_owned_squad(
                match snapshot.intent.posture {
                    DeploymentPosture::Execute => MissionType::Attack,
                    DeploymentPosture::Forming => MissionType::Defense,
                },
                snapshot.intent.player_id,
            )
        };
        squad.members.insert(entity);
        squad.target = Some(match snapshot.intent.posture {
            DeploymentPosture::Execute => snapshot.intent.anchor,
            DeploymentPosture::Forming => snapshot.intent.staging_anchor,
        });
        squad.target_island = island_map
            .and_then(|islands| islands.get_island_at(&snapshot.intent.anchor))
            .map(|island| island.id);
        squad.phase = MissionPhase::Forming;
        let squad_id = squad.id;
        if let Some(reservation) = self.forming_slots.get_mut(&slot) {
            debug_assert_eq!(reservation.id, slot);
            reservation.squad_id = Some(squad_id);
        }
        if let Some(deployment) = self.assigned.get_mut(&entity) {
            deployment.squad_id = Some(squad_id);
        }
    }

    /// 同じ攻勢作戦の前衛・砲が協働できるか。別の敵を狙っていても作戦IDを失わない。
    /// 台帳が無い二体だけは呼出側のSquad目標による判定へ委ねる。
    pub(crate) fn shares_attack_operation(&self, left: Entity, right: Entity) -> Option<bool> {
        let (left, right) = match (self.assigned.get(&left), self.assigned.get(&right)) {
            (None, None) => return None,
            (Some(left), Some(right)) => (left, right),
            _ => return Some(false),
        };
        if !left.active
            || !right.active
            || left.intent.player_id != right.intent.player_id
            || left.intent.posture != DeploymentPosture::Execute
            || right.intent.posture != DeploymentPosture::Execute
            || left.intent.slot_kind != SlotKind::Combat
            || right.intent.slot_kind != SlotKind::Combat
        {
            return Some(false);
        }
        Some(
            match (
                left.intent.operation_binding,
                right.intent.operation_binding,
            ) {
                (Some(a), Some(b)) => a.operation_id == b.operation_id && a.revision == b.revision,
                (None, None) => match (left.intent.plan_step, right.intent.plan_step) {
                    (Some(a), Some(b)) => a.plan_id == b.plan_id && a.revision == b.revision,
                    (None, None) => left.intent.anchor == right.intent.anchor,
                    _ => false,
                },
                _ => false,
            },
        )
    }
}
