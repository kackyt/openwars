//! 拡張経路（小規模マップ戦略・非V4）の実装。
//!
//! 親モジュールはmain互換の実装を持ち、本文の異なる関数は
//! `crate::ai::code_path::is_extended()` でここへ委譲する。

#[allow(unused_imports)]
use super::*;

/// 新しいAI (V2) のメイン実行ループ。
/// 最初のステップで部隊再編成とビーム探索をキャッシュし、毎ステップ1アクションずつ実行します。
pub(crate) fn execute_ai_turn_v2(world: &mut World, active_player: PlayerId) -> Option<String> {
    let ai_version = crate::ai::resolve_player_ai_version(world, active_player);
    let is_v4 = ai_version == crate::ai::ai_version::AiVersion::V4;

    let mut skip_entities = std::collections::HashSet::new();
    if let Some(res) = world.get_resource::<AiActionCooldown>() {
        skip_entities = res.0.clone();
    }

    let uses_v3 = ai_version.uses_v3_tactics();
    if is_v4 {
        // このターン以降の生産・Roadmap・Squadが同じ戦略経路を選ぶため、入口で一度だけ
        // 盤面構造からプロファイルを確定する。
        crate::ai::strategy_profile::profile_for(world, active_player);
    }
    let turn = world
        .get_resource::<crate::resources::MatchState>()
        .map_or(0, |state| state.current_turn_number.0);
    let has_queued_production = world
        .get_resource::<AiProductionCommandQueue>()
        .is_some_and(|queue| {
            queue.player_id == Some(active_player)
                && queue.turn == turn
                && !queue.commands.is_empty()
        });
    if has_queued_production
        && let Some(command) = take_next_queued_production_command(world, active_player, turn)
    {
        return issue_production_command(world, command);
    }
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
    // 未完成の島嶼輸送パッケージが生産施設上でFormingすると、不足している次の
    // 輸送役を自分で生産不能にする。任務所属は維持したまま隣接待機地へ一歩だけ退避する。
    if uses_v3 {
        // 実行中のAttack/Capture任務は通常戦術を先に試す。ここでV4だけ全任務を含めると、
        // 工場から届く敵への合法な初撃まで「退避Wait」が横取りする。前段では本当に
        // Forming中の輸送役・護衛待ち占領役だけを動かし、通常戦術で動けなかった
        // 生産施設blockerは下段の共通fallbackで初めて退避させる。
        let relief =
            decide_forming_campaign_site_relief(world, active_player, &skip_entities, false);
        if let Some((entity, command)) = relief {
            let command_text = format!("{:?}", command);
            execute_ai_command(world, entity, command);
            if let Some(mut cooldown) = world.get_resource_mut::<AiActionCooldown>() {
                cooldown.0.insert(entity);
            } else {
                world.insert_resource(AiActionCooldown(HashSet::from([entity])));
            }
            return Some(command_text);
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
                    if is_v4
                        && !world
                            .get_resource::<crate::ai::operation_assignment::UnitOperationRegistry>(
                            )
                            .and_then(|registry| registry.assignment(entity))
                            .is_some_and(|assignment| assignment.squad_id == Some(squad.id))
                    {
                        continue;
                    }
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
                // Forming中は複数の輸送役をmembersへ束ねる。代表transport_entityだけを
                // 除外すると残りが汎用beam searchへ漏れ、空荷で前線へ進んでしまう。
                decide_skip_entities.extend(squad.members.iter().copied());
                if let Some(transport_entity) = squad.transport_entity {
                    decide_skip_entities.insert(transport_entity);
                }
                decide_skip_entities.extend(squad.cargo_entities.iter().copied());
            }
        }
    }

    // 2. 通常部隊・SoloFallback ユニットの行動決定 (V2意思決定)
    // 小規模mapの序盤は、Squadの目標同期より先に占領開始turnを失うと、後から戦闘力を
    // 足しても収入差を埋められない。未割当の歩兵を単にWaitさせず、現盤面の未所有物件へ
    // 到達できるCapture命令だけを先に実行する。敵・味方・地形から毎手番判定するため、
    // 特定mapや固定の生産列には依存しない。
    let normal_action = if is_v4 {
        crate::ai::v4::home_defense::decide_action(world, active_player, &decide_skip_entities)
            .or_else(|| decide_squad_action_v4(world, active_player, &decide_skip_entities))
    } else {
        decide_ai_action_v2(world, active_player, &decide_skip_entities)
    };
    if let Some((entity, command)) = normal_action {
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
    // Drop/Capture/作戦完了で手番途中にSquadを失ったV4 Entityを、全通常行動が
    // 尽きた時点で1度だけ再接続する。従来は次手番までownerだけが残り、
    // 行動可能な歩兵・輸送役が生産拠点付近で遊兵化していた。
    // Reserve行動そのものが目標到達でSquadを完了させる場合もあるため、一度で打ち切らず、
    // 通常行動が尽きるたびに固定点を取り直す。行動候補が無ければそのまま生産へ進む。
    let should_reassign_idle = is_v4;
    if should_reassign_idle {
        crate::ai::squad::reconcile_v4_end_turn_reserves(world, active_player);
        let reassigned_action = decide_squad_action_v4(world, active_player, &skip_entities);
        if let Some((entity, command)) = reassigned_action {
            let command_text = format!("{:?}", command);
            execute_ai_command(world, entity, command);
            if let Some(mut cooldown) = world.get_resource_mut::<AiActionCooldown>() {
                cooldown.0.insert(entity);
            } else {
                world.insert_resource(AiActionCooldown(HashSet::from([entity])));
            }
            return Some(command_text);
        }
    }
    // 通常の作戦行動で進めなかった任務所属unitも、生産施設だけは塞ぎ続けない。
    // 先に通常行動を試した後のfallbackなので、前線へ進めるunitの移動距離は奪わない。
    if is_v4 {
        let relief =
            decide_forming_campaign_site_relief(world, active_player, &skip_entities, true);
        if let Some((entity, command)) = relief {
            let command_text = format!("{:?}", command);
            execute_ai_command(world, entity, command);
            if let Some(mut cooldown) = world.get_resource_mut::<AiActionCooldown>() {
                cooldown.0.insert(entity);
            } else {
                world.insert_resource(AiActionCooldown(HashSet::from([entity])));
            }
            return Some(command_text);
        }
    }
    // 3. 生産行動
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

    let mut production_queue = world
        .remove_resource::<AiProductionCommandQueue>()
        .unwrap_or_default();
    if production_queue.player_id != Some(active_player) || production_queue.turn != turn {
        production_queue.player_id = Some(active_player);
        production_queue.turn = turn;
        production_queue.commands.clear();
    }

    // 直前の計画に残る別施設の命令を、重い戦略分析を再実行せず先に消費する。
    while let Some(cmd) = production_queue.commands.pop_front() {
        if cooldown_set.contains(&(cmd.target_x, cmd.target_y)) {
            continue;
        }
        let cmd_str = format!("{:?}", cmd);
        if last_error.is_some() && last_event_str.as_deref() == Some(&cmd_str) {
            continue;
        }
        world.insert_resource(production_queue);
        if let Some(mut cooldown) = world.get_resource_mut::<AiProductionCooldown>() {
            cooldown.0.insert((cmd.target_x, cmd.target_y));
        } else {
            world.insert_resource(AiProductionCooldown(HashSet::from([(
                cmd.target_x,
                cmd.target_y,
            )])));
        }
        if let Some(mut events) =
            world.get_resource_mut::<Events<crate::events::ProduceUnitCommand>>()
        {
            events.send(cmd);
            return Some(cmd_str);
        }
        return None;
    }

    // 敵が行動しない同一生産フェーズでは、全施設分を一度だけ計画する。
    let mut prod_commands =
        super::super::production::decide_production(world, active_player).into_iter();
    while let Some(cmd) = prod_commands.next() {
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

        {
            if let Some(mut res) = world.get_resource_mut::<AiProductionCooldown>() {
                res.0.insert((cmd.target_x, cmd.target_y));
            } else {
                let mut set = HashSet::new();
                set.insert((cmd.target_x, cmd.target_y));
                world.insert_resource(AiProductionCooldown(set));
            }
        }

        production_queue.commands.extend(prod_commands);
        world.insert_resource(production_queue);
        if let Some(mut events) =
            world.get_resource_mut::<Events<crate::events::ProduceUnitCommand>>()
        {
            events.send(cmd);
            return Some(cmd_str);
        }
        return None;
    }
    world.insert_resource(production_queue);

    // 4. 全行動完了 -> ターン終了
    // ターン終了直前は「このターン結局何が動かなかったか」が確定する唯一の点。
    // AiActionCooldown はターン境界で破棄されるため、ここで遊兵を計上しておく。
    let acted_entities = world
        .get_resource::<AiActionCooldown>()
        .map(|res| res.0.clone())
        .unwrap_or_default();
    let idle_audit = crate::ai::idle_audit::audit_idle_units(world, active_player, &acted_entities);
    let idle_audit_turn = world
        .get_resource::<crate::resources::MatchState>()
        .map_or(0, |state| state.current_turn_number.0);
    if let Some(mut diagnostics) =
        world.get_resource_mut::<crate::ai::idle_audit::IdleAuditDiagnostics>()
    {
        diagnostics.record(idle_audit_turn, idle_audit);
    } else {
        let mut diagnostics = crate::ai::idle_audit::IdleAuditDiagnostics::default();
        diagnostics.record(idle_audit_turn, idle_audit);
        world.insert_resource(diagnostics);
    }

    if let Some(mut end_events) =
        world.get_resource_mut::<Events<crate::events::NextPhaseCommand>>()
    {
        end_events.send(crate::events::NextPhaseCommand);
    }
    None
}

pub(super) use super::campaign_action_context;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::{Faction, Health, PlayerId, Property, UnitStats};
    use crate::resources::{DamageChart, UnitType};

    /// 弾薬を備えた前衛と砲を同一攻略目標へ割り当てる。
    fn setup_cooperative_fire_world() -> (World, Entity, Entity, Entity) {
        let player = PlayerId(1);
        let mut world = setup_v3_test_world(6, crate::ai::ai_version::AiVersion::V4);
        world.insert_resource(Map::new(6, 3, Terrain::Plains, GridTopology::Square));
        world.insert_resource(DamageChart::new());
        let stats = |world: &World, kind: UnitType| {
            world
                .resource::<MasterDataRegistry>()
                .create_unit_stats(&UnitName(kind.as_str().to_owned()))
                .unwrap()
        };
        let front_stats = stats(&world, UnitType::Recon);
        let gun_stats = stats(&world, UnitType::LightSpGun);
        let enemy_stats = stats(&world, UnitType::Mech);
        let front = spawn_v3_test_unit(&mut world, player, 2, 100, front_stats);
        let gun = spawn_v3_test_unit(&mut world, player, 0, 100, gun_stats);
        let enemy = spawn_v3_test_unit(&mut world, PlayerId(2), 3, 100, enemy_stats);
        // 共通spawn helperは弾薬を付けないため、実際に射撃できる盤面を明示する。
        for entity in [front, gun, enemy] {
            let stats = world.get::<UnitStats>(entity).unwrap();
            let ammo = crate::components::Ammo {
                ammo1: stats.max_ammo1,
                max_ammo1: stats.max_ammo1,
                ammo2: stats.max_ammo2,
                max_ammo2: stats.max_ammo2,
            };
            world.entity_mut(entity).insert(ammo);
        }
        let target = GridPosition { x: 3, y: 0 };
        let mut manager = crate::ai::squad::SquadManager::new();
        for entity in [front, gun] {
            let squad = manager.create_owned_squad(crate::ai::squad::MissionType::Attack, player);
            squad.members.insert(entity);
            squad.target = Some(target);
            squad.phase = crate::ai::squad::MissionPhase::MovingToTarget;
        }
        world.insert_resource(manager);
        (world, front, gun, enemy)
    }

    /// 同じ敵を攻略する前衛と砲がいる場合、編成順で前衛を先に消耗させない。
    #[test]
    fn v4_frontline_waits_for_stationary_support_fire() {
        let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
        let player = PlayerId(1);
        let (mut world, front, gun, enemy) = setup_cooperative_fire_world();
        let (actor, command) = decide_squad_action_v4(&mut world, player, &HashSet::new()).unwrap();
        assert_eq!(
            actor, gun,
            "前衛より後方の静止射撃を先に実行する: {command:?}"
        );
        assert!(
            matches!(command, AiCommand::Attack { target_entity, target_pos }
            if target_entity == enemy && target_pos == GridPosition { x: 0, y: 0 })
        );

        // 仮想ダメージでなく、本番の戦闘SystemでHP・弾薬・行動済みを更新する。
        world.insert_resource(crate::resources::MatchState {
            current_phase: crate::resources::Phase::Main,
            ..Default::default()
        });
        world.insert_resource(crate::resources::Players(vec![
            crate::resources::Player::new(1, "P1".to_owned()),
            crate::resources::Player::new(2, "P2".to_owned()),
        ]));
        world.insert_resource(crate::resources::GameRng::new(42));
        world.insert_resource(Events::<AttackUnitCommand>::default());
        world.insert_resource(Events::<crate::events::UnitAttackedEvent>::default());
        world.insert_resource(Events::<crate::events::UnitDestroyedEvent>::default());
        world.send_event(AttackUnitCommand {
            attacker_entity: gun,
            defender_entity: enemy,
        });
        let mut schedule = Schedule::default();
        schedule.add_systems(crate::systems::combat::attack_unit_system);
        schedule.run(&mut world);
        assert!(world.get::<Health>(enemy).unwrap().current < 100);
        assert!(!world.get::<ActionCompleted>(front).unwrap().0);
        let (actor, command) = decide_squad_action_v4(&mut world, player, &HashSet::new()).unwrap();
        assert_eq!(actor, front, "砲撃後に前衛を再評価する: {command:?}");
        assert!(
            matches!(command, AiCommand::Attack { target_entity, .. } if target_entity == enemy)
        );
    }

    #[test]
    fn v4_same_operation_supports_front_with_a_different_squad_target() {
        let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
        let (mut world, front, gun, enemy) = setup_cooperative_fire_world();
        world
            .resource_mut::<crate::ai::squad::SquadManager>()
            .squads
            .iter_mut()
            .find(|s| s.members.contains(&gun))
            .unwrap()
            .target = Some(GridPosition { x: 5, y: 2 });
        let mut deployments = crate::ai::v4::deployment::V4DeploymentRegistry::default();
        deployments.assign_target_for_test(PlayerId(1), front, enemy);
        deployments.assign_target_for_test(PlayerId(1), gun, enemy);
        world.insert_resource(deployments);
        let (actor, command) =
            decide_squad_action_v4(&mut world, PlayerId(1), &HashSet::new()).unwrap();
        assert_eq!(
            actor, gun,
            "Squadの一点目標が異なっても作戦所属を使う: {command:?}"
        );
        assert!(
            matches!(command, AiCommand::Attack { target_entity, .. } if target_entity == enemy)
        );
    }

    #[test]
    fn v4_unavailable_support_does_not_hold_the_frontline() {
        let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
        for mode in 0..5 {
            let (mut world, front, gun, _) = setup_cooperative_fire_world();
            match mode {
                0 => world.get_mut::<HasMoved>(gun).unwrap().0 = true,
                1 => world.get_mut::<ActionCompleted>(gun).unwrap().0 = true,
                2 => {
                    let mut ammo = world.get_mut::<crate::components::Ammo>(gun).unwrap();
                    ammo.ammo1 = 0;
                    ammo.ammo2 = 0;
                }
                3 => {
                    // 別の作戦へ向かう砲の任務を横取りしない。
                    let mut manager = world.resource_mut::<crate::ai::squad::SquadManager>();
                    manager
                        .squads
                        .iter_mut()
                        .find(|s| s.members.contains(&gun))
                        .unwrap()
                        .target = Some(GridPosition { x: 5, y: 2 });
                }
                _ => {
                    world.despawn(gun);
                }
            }
            let (actor, _) =
                decide_squad_action_v4(&mut world, PlayerId(1), &HashSet::new()).unwrap();
            assert_eq!(
                actor, front,
                "支援が実行できなくても前衛を停止させない: mode={mode}"
            );
        }
    }

    /// 前衛が進んだ後、砲は遮断された敵経路の手前の射点へ追随する。
    #[test]
    fn v4_support_follows_advanced_front_without_passing_it() {
        let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
        let player = PlayerId(1);
        let mut world = setup_v3_test_world(9, crate::ai::ai_version::AiVersion::V4);
        world.insert_resource(DamageChart::new());
        let master = world.resource::<MasterDataRegistry>().clone();
        let gun_stats = master
            .create_unit_stats(&UnitName(UnitType::LightSpGun.as_str().to_owned()))
            .unwrap();
        let front_stats = master
            .create_unit_stats(&UnitName(UnitType::MdTank.as_str().to_owned()))
            .unwrap();
        let enemy_stats = master
            .create_unit_stats(&UnitName(UnitType::Infantry.as_str().to_owned()))
            .unwrap();
        let gun = spawn_v3_test_unit(&mut world, player, 1, 100, gun_stats);
        let front = spawn_v3_test_unit(&mut world, player, 5, 100, front_stats);
        let enemy = spawn_v3_test_unit(&mut world, PlayerId(2), 7, 100, enemy_stats);
        for entity in [front, gun, enemy] {
            let stats = world.get::<UnitStats>(entity).unwrap();
            let ammo = crate::components::Ammo {
                ammo1: stats.max_ammo1,
                max_ammo1: stats.max_ammo1,
                ammo2: stats.max_ammo2,
                max_ammo2: stats.max_ammo2,
            };
            world.entity_mut(entity).insert(ammo);
        }
        world.get_mut::<HasMoved>(front).unwrap().0 = true;
        world.get_mut::<ActionCompleted>(front).unwrap().0 = true;
        let mut manager = crate::ai::squad::SquadManager::new();
        for entity in [front, gun] {
            let squad = manager.create_owned_squad(crate::ai::squad::MissionType::Attack, player);
            squad.members.insert(entity);
            squad.target = Some(GridPosition { x: 7, y: 0 });
            squad.phase = crate::ai::squad::MissionPhase::MovingToTarget;
        }
        world.insert_resource(manager);
        let (actor, command) = decide_squad_action_v4(&mut world, player, &HashSet::new()).unwrap();
        assert_eq!(actor, gun);
        assert!(
            matches!(command, AiCommand::Wait { target_pos } if target_pos == GridPosition { x: 4, y: 0 }),
            "移動済みの前衛より後方の射点へ追随し、同ターンの射撃を捏造しない: {command:?}"
        );
        *world.get_mut::<GridPosition>(gun).unwrap() = GridPosition { x: 4, y: 0 };
        world.get_mut::<HasMoved>(gun).unwrap().0 = true;
        assert!(decide_squad_action_v4(&mut world, player, &HashSet::new()).is_none());
        // 翌手番は再配置した射点から動かずに撃つ。
        world.get_mut::<HasMoved>(gun).unwrap().0 = false;
        let (actor, command) = decide_squad_action_v4(&mut world, player, &HashSet::new()).unwrap();
        assert_eq!(actor, gun);
        assert!(
            matches!(command, AiCommand::Attack { target_entity, target_pos }
            if target_entity == enemy && target_pos == GridPosition { x: 4, y: 0 })
        );
    }

    #[test]
    fn v4_squad_executor_defers_blocked_wait_until_front_squad_vacates() {
        let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
        let player = PlayerId(1);
        let mut world = setup_v3_test_world(4, crate::ai::ai_version::AiVersion::V4);
        world.insert_resource(Map::new(4, 2, Terrain::Plains, GridTopology::Square));
        world.insert_resource(DamageChart::new());
        let mut stats = world
            .resource::<MasterDataRegistry>()
            .create_unit_stats(&UnitName(UnitType::Infantry.as_str().to_owned()))
            .unwrap();
        // 1マス先を塞がれると後方は同位置Waitになる一方、前方は確実に退去できる配置。
        stats.max_movement = 1;
        let rear = spawn_v3_test_unit(&mut world, player, 0, 100, stats.clone());
        let front = spawn_v3_test_unit(&mut world, player, 1, 100, stats.clone());
        let side = spawn_v3_test_unit(&mut world, player, 0, 100, stats);
        *world.get_mut::<GridPosition>(side).unwrap() = GridPosition { x: 0, y: 1 };
        let objective = GridPosition { x: 3, y: 0 };
        let mut manager = crate::ai::squad::SquadManager::new();
        let rear_squad = manager.create_owned_squad(crate::ai::squad::MissionType::Attack, player);
        rear_squad.members.insert(rear);
        rear_squad.target = Some(objective);
        rear_squad.phase = crate::ai::squad::MissionPhase::MovingToTarget;
        let front_squad = manager.create_owned_squad(crate::ai::squad::MissionType::Attack, player);
        front_squad.members.insert(front);
        front_squad.target = Some(objective);
        front_squad.phase = crate::ai::squad::MissionPhase::MovingToTarget;
        // 前方退去後にこの第三Squadへカーソルを進めず、保留した後方を先に再評価する。
        let side_squad = manager.create_owned_squad(crate::ai::squad::MissionType::Attack, player);
        side_squad.members.insert(side);
        side_squad.target = Some(GridPosition { x: 3, y: 1 });
        side_squad.phase = crate::ai::squad::MissionPhase::MovingToTarget;
        world.insert_resource(manager);

        let (first_entity, first_command) =
            decide_squad_action_v4(&mut world, player, &HashSet::new())
                .expect("後方の同位置Waitより前方Squadの退去を先に選ぶ");
        assert_eq!(first_entity, front);
        let AiCommand::Wait {
            target_pos: front_target,
        } = first_command
        else {
            panic!("前方Squadは目的方向へ移動してWaitすること");
        };
        assert_eq!(front_target, GridPosition { x: 2, y: 0 });

        // 実行器が前方の移動を適用した次stepを再現する。保留した後方は未行動のまま。
        *world.get_mut::<GridPosition>(front).unwrap() = front_target;
        world.get_mut::<HasMoved>(front).unwrap().0 = true;
        world.get_mut::<ActionCompleted>(front).unwrap().0 = true;
        let skip_entities = HashSet::from([front]);
        let (second_entity, second_command) =
            decide_squad_action_v4(&mut world, player, &skip_entities)
                .expect("出口が空いた後方Squadを同じ自手番で再評価する");
        assert_eq!(second_entity, rear);
        assert!(matches!(
            second_command,
            AiCommand::Wait {
                target_pos: GridPosition { x: 1, y: 0 }
            }
        ));
        assert!(!world.get::<HasMoved>(rear).unwrap().0);
        assert!(!world.get::<ActionCompleted>(rear).unwrap().0);
    }

    #[test]
    fn v4_squad_executor_commits_wait_after_one_fully_blocked_pass() {
        let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
        let player = PlayerId(1);
        let mut world = setup_v3_test_world(2, crate::ai::ai_version::AiVersion::V4);
        world.insert_resource(DamageChart::new());
        let mut stats = world
            .resource::<MasterDataRegistry>()
            .create_unit_stats(&UnitName(UnitType::Infantry.as_str().to_owned()))
            .unwrap();
        stats.max_movement = 1;
        let left = spawn_v3_test_unit(&mut world, player, 0, 100, stats.clone());
        let right = spawn_v3_test_unit(&mut world, player, 1, 100, stats);
        let mut manager = crate::ai::squad::SquadManager::new();
        let left_squad = manager.create_owned_squad(crate::ai::squad::MissionType::Attack, player);
        left_squad.members.insert(left);
        left_squad.target = Some(GridPosition { x: 1, y: 0 });
        left_squad.phase = crate::ai::squad::MissionPhase::MovingToTarget;
        let right_squad = manager.create_owned_squad(crate::ai::squad::MissionType::Attack, player);
        right_squad.members.insert(right);
        right_squad.target = Some(GridPosition { x: 0, y: 0 });
        right_squad.phase = crate::ai::squad::MissionPhase::MovingToTarget;
        world.insert_resource(manager);

        let (entity, command) = decide_squad_action_v4(&mut world, player, &HashSet::new())
            .expect("相互閉塞でも一巡後は最初のWaitを確定して終了する");

        assert_eq!(entity, left);
        assert!(matches!(
            command,
            AiCommand::Wait {
                target_pos: GridPosition { x: 0, y: 0 }
            }
        ));
    }

    #[test]
    fn map1_recon_can_move_and_attack_adjacent_infantry_on_hex_grid() {
        let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
        let player = PlayerId(2);
        let enemy_player = PlayerId(1);
        let mut world = setup_v3_test_world(10, crate::ai::ai_version::AiVersion::V4);
        world.insert_resource(Map::new(10, 14, Terrain::Plains, GridTopology::Hex));
        let registry = world.resource::<MasterDataRegistry>().clone();
        let recon_stats = registry
            .create_unit_stats(&UnitName(UnitType::Recon.as_str().to_owned()))
            .unwrap();
        let infantry_stats = registry
            .create_unit_stats(&UnitName(UnitType::Infantry.as_str().to_owned()))
            .unwrap();
        let recon = world
            .spawn((
                Faction(player),
                GridPosition { x: 3, y: 10 },
                recon_stats.clone(),
                Health {
                    current: 100,
                    max: 100,
                },
                crate::components::Ammo {
                    ammo1: recon_stats.max_ammo1,
                    max_ammo1: recon_stats.max_ammo1,
                    ammo2: recon_stats.max_ammo2,
                    max_ammo2: recon_stats.max_ammo2,
                },
            ))
            .id();
        let infantry = world
            .spawn((
                Faction(enemy_player),
                GridPosition { x: 4, y: 6 },
                infantry_stats,
            ))
            .id();

        let snapshot = AiTacticalSnapshot::from_world(&mut world, &registry, GridTopology::Hex);
        let actions =
            snapshot.action_targets_at(recon, &recon_stats, GridPosition { x: 4, y: 7 }, true);

        assert!(
            actions.attackable_targets.contains(&infantry),
            "map_1のT2射撃位置では、移動後の装甲車が隣接歩兵を攻撃できる"
        );
    }

    #[test]
    fn damaged_unit_does_not_enter_factory_when_it_can_wait_off_site() {
        let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
        let player = PlayerId(1);
        let mut world = setup_v3_test_world(3, crate::ai::ai_version::AiVersion::V4);
        world.insert_resource(Map {
            width: 3,
            height: 2,
            tiles: vec![
                Terrain::Plains,
                Terrain::Factory,
                Terrain::Plains,
                Terrain::Capital,
                Terrain::Plains,
                Terrain::Plains,
            ],
            topology: crate::resources::GridTopology::Square,
        });
        world.insert_resource(DamageChart::new());
        world.spawn((
            GridPosition { x: 0, y: 1 },
            Property::new(Terrain::Capital, Some(player), 100),
        ));
        world.spawn((
            GridPosition { x: 1, y: 0 },
            Property::new(Terrain::Factory, Some(player), 100),
        ));
        let stats = world
            .resource::<MasterDataRegistry>()
            .create_unit_stats(&crate::resources::master_data::UnitName(
                UnitType::Infantry.as_str().to_owned(),
            ))
            .unwrap();
        let infantry = world
            .spawn((
                Faction(player),
                HasMoved(false),
                ActionCompleted(false),
                GridPosition { x: 0, y: 0 },
                stats.clone(),
                Health {
                    current: 50,
                    max: 100,
                },
                crate::components::Ammo {
                    ammo1: stats.max_ammo1,
                    max_ammo1: stats.max_ammo1,
                    ammo2: stats.max_ammo2,
                    max_ammo2: stats.max_ammo2,
                },
                crate::components::Fuel {
                    current: stats.max_fuel,
                    max: stats.max_fuel,
                },
            ))
            .id();

        let (entity, command) =
            decide_ai_action_v2(&mut world, player, &HashSet::new()).expect("行動を選ぶこと");
        assert_eq!(entity, infantry);
        let AiCommand::Wait { target_pos } = command else {
            panic!("敵がいないためWaitを選ぶこと");
        };
        assert_ne!(
            target_pos,
            GridPosition { x: 1, y: 0 },
            "回復開始より次手番の生産slotを優先し、工場外で行動終了する"
        );
    }

    #[test]
    fn indirect_unit_fires_from_factory_when_other_production_slots_are_open() {
        let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
        let player = PlayerId(1);
        let enemy = PlayerId(2);
        let mut world = setup_v3_test_world(5, crate::ai::ai_version::AiVersion::V4);
        world.insert_resource(Map {
            width: 5,
            height: 2,
            tiles: vec![
                Terrain::Plains,
                Terrain::Factory,
                Terrain::Plains,
                Terrain::Plains,
                Terrain::Plains,
                Terrain::Capital,
                Terrain::Plains,
                Terrain::Plains,
                Terrain::Plains,
                Terrain::Plains,
            ],
            topology: crate::resources::GridTopology::Square,
        });
        world.insert_resource(DamageChart::new());
        world.spawn((
            GridPosition { x: 0, y: 1 },
            Property::new(Terrain::Capital, Some(player), 100),
        ));
        world.spawn((
            GridPosition { x: 1, y: 0 },
            Property::new(Terrain::Factory, Some(player), 100),
        ));
        let artillery = world
            .resource::<MasterDataRegistry>()
            .create_unit_stats(&crate::resources::master_data::UnitName(
                UnitType::LightSpGun.as_str().to_owned(),
            ))
            .unwrap();
        let unit = world
            .spawn((
                Faction(player),
                HasMoved(false),
                ActionCompleted(false),
                GridPosition { x: 1, y: 0 },
                artillery.clone(),
                Health {
                    current: 100,
                    max: 100,
                },
                crate::components::Ammo {
                    ammo1: artillery.max_ammo1,
                    max_ammo1: artillery.max_ammo1,
                    ammo2: artillery.max_ammo2,
                    max_ammo2: artillery.max_ammo2,
                },
                crate::components::Fuel {
                    current: artillery.max_fuel,
                    max: artillery.max_fuel,
                },
            ))
            .id();
        let infantry = world
            .resource::<MasterDataRegistry>()
            .create_unit_stats(&crate::resources::master_data::UnitName(
                UnitType::Infantry.as_str().to_owned(),
            ))
            .unwrap();
        let target = world
            .spawn((
                Faction(enemy),
                GridPosition { x: 4, y: 0 },
                infantry,
                Health {
                    current: 100,
                    max: 100,
                },
            ))
            .id();

        let (entity, command) =
            decide_ai_action_v2(&mut world, player, &HashSet::new()).expect("行動を選ぶこと");
        assert_eq!(entity, unit);
        assert!(
            matches!(command, AiCommand::Attack { target_entity, target_pos }
            if target_entity == target && target_pos == GridPosition { x: 1, y: 0 }),
            "別の生産口が空いているとき、合法な間接射撃を工場退去で潰さない: {command:?}"
        );
    }

    #[test]
    fn v4_indirect_unit_vacates_factory_when_all_production_slots_are_blocked() {
        let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
        let player = PlayerId(1);
        let enemy = PlayerId(2);
        let mut world = setup_v3_test_world(5, crate::ai::ai_version::AiVersion::V4);
        world.insert_resource(Map {
            width: 5,
            height: 2,
            tiles: vec![
                Terrain::Plains,
                Terrain::Factory,
                Terrain::Plains,
                Terrain::Plains,
                Terrain::Plains,
                Terrain::Capital,
                Terrain::Plains,
                Terrain::Plains,
                Terrain::Plains,
                Terrain::Plains,
            ],
            topology: crate::resources::GridTopology::Square,
        });
        world.insert_resource(DamageChart::new());
        world.spawn((
            GridPosition { x: 0, y: 1 },
            Property::new(Terrain::Capital, Some(player), 100),
        ));
        world.spawn((
            GridPosition { x: 1, y: 0 },
            Property::new(Terrain::Factory, Some(player), 100),
        ));
        let artillery = world
            .resource::<MasterDataRegistry>()
            .create_unit_stats(&crate::resources::master_data::UnitName(
                UnitType::LightSpGun.as_str().to_owned(),
            ))
            .unwrap();
        let unit = world
            .spawn((
                Faction(player),
                HasMoved(false),
                ActionCompleted(false),
                GridPosition { x: 1, y: 0 },
                artillery.clone(),
                Health {
                    current: 100,
                    max: 100,
                },
                crate::components::Ammo {
                    ammo1: artillery.max_ammo1,
                    max_ammo1: artillery.max_ammo1,
                    ammo2: artillery.max_ammo2,
                    max_ammo2: artillery.max_ammo2,
                },
                crate::components::Fuel {
                    current: artillery.max_fuel,
                    max: artillery.max_fuel,
                },
            ))
            .id();
        let infantry = world
            .resource::<MasterDataRegistry>()
            .create_unit_stats(&crate::resources::master_data::UnitName(
                UnitType::Infantry.as_str().to_owned(),
            ))
            .unwrap();
        // 首都（もう一つの生産口）を行動済みの味方が塞ぎ、全生産口が閉塞した状態にする。
        world.spawn((
            Faction(player),
            HasMoved(true),
            ActionCompleted(true),
            GridPosition { x: 0, y: 1 },
            infantry.clone(),
            Health {
                current: 100,
                max: 100,
            },
        ));
        // 工場上から射程内に入る敵。定位置射撃が可能な状況でも退去を選ぶかを確かめる。
        world.spawn((
            Faction(enemy),
            GridPosition { x: 4, y: 0 },
            infantry,
            Health {
                current: 100,
                max: 100,
            },
        ));

        let (entity, command) = decide_ai_action_v2_for_entities(
            &mut world,
            player,
            &HashSet::new(),
            Some(&HashSet::from([unit])),
        )
        .expect("行動を選ぶこと");
        assert_eq!(entity, unit);
        // 定位置射撃の強制は、工場外で手番を終えられる工場上の間接ユニットには
        // 適用しない。全生産口が閉塞している間は生産口の回復を優先し、工場から退く。
        let destination = match command {
            AiCommand::Attack { target_pos, .. }
            | AiCommand::Wait { target_pos }
            | AiCommand::Capture { target_pos }
            | AiCommand::Merge { target_pos, .. } => target_pos,
            other => panic!("工場退去を伴う行動を選ぶこと: {other:?}"),
        };
        assert_ne!(
            destination,
            GridPosition { x: 1, y: 0 },
            "全生産口が閉塞しているとき、工場上の間接ユニットは射撃のために生産口を塞ぎ続けない: {command:?}"
        );
    }

    #[test]
    fn reference_map32_turn4_rocket_keeps_its_factory_firing_position() {
        let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
        // 人間対局のP2第4手番開始時点。Entity番号は再生成し、座標・HP・弾薬を復元する。
        let snapshot: serde_json::Value =
            serde_json::from_str(include_str!("../test_data/map32_human_turn4.json")).unwrap();
        let data = MasterDataRegistry::load().unwrap();
        let (mut world, _) = crate::setup::initialize_world_from_master_data_with_topology(
            &data,
            "map_32",
            GridTopology::Hex,
        )
        .unwrap();
        let mut settings = crate::ai::PlayerAiSettings::new();
        settings.set_version(PlayerId(2), crate::ai::AiVersion::V4);
        world.insert_resource(settings);
        world
            .resource_mut::<crate::resources::MatchState>()
            .current_turn_number
            .0 = 4;
        for funds in snapshot["players_funds"].as_array().unwrap() {
            let id = PlayerId(funds[0].as_u64().unwrap() as u32);
            world
                .resource_mut::<crate::resources::Players>()
                .0
                .iter_mut()
                .find(|player| player.id == id)
                .unwrap()
                .funds = funds[1].as_u64().unwrap() as u32;
        }
        for property in snapshot["properties"].as_array().unwrap() {
            let at = GridPosition {
                x: property["x"].as_u64().unwrap() as usize,
                y: property["y"].as_u64().unwrap() as usize,
            };
            let owner = property["owner"]
                .as_u64()
                .map(|owner| PlayerId(owner as u32));
            let entity = world
                .iter_entities()
                .find(|entity| {
                    entity.get::<GridPosition>() == Some(&at) && entity.contains::<Property>()
                })
                .unwrap()
                .id();
            world.get_mut::<Property>(entity).unwrap().owner_id = owner;
        }
        let mut rocket = None;
        for source in snapshot["units"].as_array().unwrap() {
            let kind: UnitType = serde_json::from_value(source["unit_type"].clone()).unwrap();
            let stats = data
                .create_unit_stats(&UnitName(kind.as_str().to_owned()))
                .unwrap();
            let at = GridPosition {
                x: source["x"].as_u64().unwrap() as usize,
                y: source["y"].as_u64().unwrap() as usize,
            };
            let entity = world
                .spawn((
                    at,
                    Faction(PlayerId(source["player"].as_u64().unwrap() as u32)),
                    Health {
                        current: source["hp"].as_u64().unwrap() as u32,
                        max: 100,
                    },
                    crate::components::Fuel {
                        current: source["fuel"].as_u64().unwrap() as u32,
                        max: stats.max_fuel,
                    },
                    crate::components::Ammo {
                        ammo1: source["ammo1"].as_u64().unwrap() as u32,
                        ammo2: source["ammo2"].as_u64().unwrap() as u32,
                        max_ammo1: stats.max_ammo1,
                        max_ammo2: stats.max_ammo2,
                    },
                    HasMoved(false),
                    ActionCompleted(false),
                    stats,
                ))
                .id();
            if source["entity_index"] == 46 {
                rocket = Some(entity);
            }
        }
        let rocket = rocket.unwrap();
        let (_, command) = decide_ai_action_v2_for_entities(
            &mut world,
            PlayerId(2),
            &HashSet::new(),
            Some(&HashSet::from([rocket])),
        )
        .unwrap();
        assert!(
            matches!(command, AiCommand::Attack { target_pos, .. }
            if target_pos == GridPosition { x: 7, y: 8 }),
            "工場上の射撃を残して侵入部隊を削る: {command:?}"
        );
    }

    fn setup_map42_human_turn5_world() -> (World, HashMap<u32, Entity>) {
        let snapshot: serde_json::Value =
            serde_json::from_str(include_str!("../test_data/map42_human_turn5.json")).unwrap();
        let data = MasterDataRegistry::load().unwrap();
        let (mut world, _) = crate::setup::initialize_world_from_master_data_with_topology(
            &data,
            "map_42",
            GridTopology::Hex,
        )
        .unwrap();

        let existing_units: Vec<Entity> = world
            .iter_entities()
            .filter(|e| e.contains::<UnitStats>())
            .map(|e| e.id())
            .collect();
        for entity in existing_units {
            world.despawn(entity);
        }

        let mut settings = crate::ai::PlayerAiSettings::new();
        settings.set_version(PlayerId(2), crate::ai::AiVersion::V4);
        world.insert_resource(settings);

        let p2_idx = world
            .resource::<crate::resources::Players>()
            .0
            .iter()
            .position(|p| p.id == PlayerId(2))
            .unwrap();
        {
            let mut match_state = world.resource_mut::<crate::resources::MatchState>();
            match_state.current_turn_number.0 = 5;
            match_state.active_player_index = crate::resources::PlayerIndex(p2_idx);
            match_state.current_phase = crate::resources::Phase::Main;
        }

        for funds in snapshot["players_funds"].as_array().unwrap() {
            let id = PlayerId(funds[0].as_u64().unwrap() as u32);
            world
                .resource_mut::<crate::resources::Players>()
                .0
                .iter_mut()
                .find(|player| player.id == id)
                .unwrap()
                .funds = funds[1].as_u64().unwrap() as u32;
        }

        for property in snapshot["properties"].as_array().unwrap() {
            let at = GridPosition {
                x: property["x"].as_u64().unwrap() as usize,
                y: property["y"].as_u64().unwrap() as usize,
            };
            let owner = property["owner"]
                .as_u64()
                .map(|owner| PlayerId(owner as u32));
            let entity = world
                .iter_entities()
                .find(|entity| {
                    entity.get::<GridPosition>() == Some(&at) && entity.contains::<Property>()
                })
                .unwrap()
                .id();
            world.get_mut::<Property>(entity).unwrap().owner_id = owner;
        }

        if let Some(captures) = snapshot
            .get("capture_progress_in_flight")
            .and_then(|c| c.as_array())
        {
            for cp in captures {
                let at = GridPosition {
                    x: cp["x"].as_u64().unwrap() as usize,
                    y: cp["y"].as_u64().unwrap() as usize,
                };
                let remaining = cp["remaining_capture_points"].as_u64().unwrap() as u32;
                let entity = world
                    .iter_entities()
                    .find(|entity| {
                        entity.get::<GridPosition>() == Some(&at) && entity.contains::<Property>()
                    })
                    .unwrap()
                    .id();
                world.get_mut::<Property>(entity).unwrap().capture_points = remaining;
            }
        }

        let mut entity_by_index = HashMap::new();
        for source in snapshot["units"].as_array().unwrap() {
            let kind: UnitType = serde_json::from_value(source["unit_type"].clone()).unwrap();
            let stats = data
                .create_unit_stats(&UnitName(kind.as_str().to_owned()))
                .unwrap();
            let at = GridPosition {
                x: source["x"].as_u64().unwrap() as usize,
                y: source["y"].as_u64().unwrap() as usize,
            };
            let has_moved = source
                .get("has_moved")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let action_completed = source
                .get("action_completed")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let entity = world
                .spawn((
                    at,
                    Faction(PlayerId(source["player"].as_u64().unwrap() as u32)),
                    Health {
                        current: source["hp"].as_u64().unwrap() as u32,
                        max: 100,
                    },
                    crate::components::Fuel {
                        current: source["fuel"].as_u64().unwrap() as u32,
                        max: stats.max_fuel,
                    },
                    crate::components::Ammo {
                        ammo1: source["ammo1"].as_u64().unwrap() as u32,
                        ammo2: source["ammo2"].as_u64().unwrap() as u32,
                        max_ammo1: stats.max_ammo1,
                        max_ammo2: stats.max_ammo2,
                    },
                    HasMoved(has_moved),
                    ActionCompleted(action_completed),
                    stats,
                ))
                .id();
            let idx = source["entity_index"].as_u64().unwrap() as u32;
            entity_by_index.insert(idx, entity);
        }

        (world, entity_by_index)
    }

    #[test]
    fn reference_map42_turn5_heavyspgun_shoots_rockets_from_city() {
        let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
        // 人間対局の知見: 自走砲は移動せず射程4の敵ロケット砲を安全に間接射撃すべき（コールドスタート再現、Squad履歴なし）
        let (mut world, entities) = setup_map42_human_turn5_world();
        let gun = entities[&77];
        let target_rocket = entities[&75];

        let (actor, command) = decide_ai_action_v2_for_entities(
            &mut world,
            PlayerId(2),
            &HashSet::new(),
            Some(&HashSet::from([gun])),
        )
        .expect("自走砲の行動決定が得られること");

        assert_eq!(actor, gun, "自走砲自身が行動すること");
        assert!(
            matches!(command, AiCommand::Attack { target_entity, target_pos }
            if target_entity == target_rocket && target_pos == GridPosition { x: 3, y: 5 }),
            "自走砲(3,5)は移動せず(3,5)から敵ロケット砲(6,8)を攻撃すること。実際の結果: {command:?}"
        );
    }

    #[test]
    fn reference_map42_turn5_global_v4_first_command_is_heavyspgun_attack() {
        let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
        // 全体V4実行でも自走砲による敵ロケット砲への間接射撃が初手として選ばれることを検証する
        let (mut world, entities) = setup_map42_human_turn5_world();
        let target_rocket = entities[&75];

        let first_command = execute_ai_turn_v2(&mut world, PlayerId(2));
        let expected_command = format!(
            "Attack {{ target_pos: GridPosition {{ x: 3, y: 5 }}, target_entity: {:?} }}",
            target_rocket
        );
        assert_eq!(
            first_command.as_deref(),
            Some(expected_command.as_str()),
            "全体V4初手で自走砲によるロケット砲への間接攻撃が実行されること"
        );
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
        let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
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

    #[test]
    fn enemy_replacement_dependence_scales_with_reliance_on_production() {
        let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
        // 敵が大軍を擁する序盤〜中盤: 補充1体を止めても次ターン戦力への寄与は小さい。
        let (num, den) = enemy_replacement_dependence(4, 36);
        assert_eq!((num, den), (4, 40));
        assert!(
            num * 10 <= den,
            "大軍を持つ敵への占位価値は1割以下に割り引かれるべき"
        );

        // 敵が消耗し補充だけが生命線になった終盤: 占位価値が最大化する。
        let (num, den) = enemy_replacement_dependence(4, 2);
        assert_eq!((num, den), (4, 6));
        assert!(
            num * 2 > den,
            "消耗した敵への占位価値は半分より大きく評価されるべき"
        );

        // 敵が全滅していれば止めるべき補充はないが、生産能力は価値を保つ。
        assert_eq!(enemy_replacement_dependence(3, 0), (3, 3));

        // 敵の生産能力がなければ占位の価値はゼロ。
        assert_eq!(enemy_replacement_dependence(0, 10), (0, 10));

        // 生産能力も戦力もない敵に対しては、ゼロ除算せず価値ゼロを返す。
        assert_eq!(enemy_replacement_dependence(0, 0), (0, 1));
    }

    #[test]
    fn v3_combat_unit_does_not_blockade_enemy_production_site() {
        let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
        let player = PlayerId(2);
        let enemy = PlayerId(1);
        let mut world = setup_v3_test_world(6, crate::ai::ai_version::AiVersion::V3);
        world.insert_resource(Map {
            width: 6,
            height: 2,
            tiles: vec![
                Terrain::Plains,
                Terrain::Factory,
                Terrain::Plains,
                Terrain::Plains,
                Terrain::Plains,
                Terrain::Capital,
                Terrain::Plains,
                Terrain::Plains,
                Terrain::Plains,
                Terrain::Plains,
                Terrain::Plains,
                Terrain::Plains,
            ],
            topology: crate::resources::GridTopology::Square,
        });
        world.insert_resource(DamageChart::new());
        world.spawn((
            GridPosition { x: 4, y: 0 },
            Property::new(Terrain::Capital, Some(enemy), 100),
        ));
        world.spawn((
            GridPosition { x: 1, y: 0 },
            Property::new(Terrain::Factory, Some(enemy), 100),
        ));
        let enemy_stats = world
            .resource::<MasterDataRegistry>()
            .create_unit_stats(&crate::resources::master_data::UnitName(
                UnitType::Infantry.as_str().to_owned(),
            ))
            .unwrap();
        spawn_v3_test_unit(&mut world, enemy, 4, 100, enemy_stats);

        let stats = world
            .resource::<MasterDataRegistry>()
            .create_unit_stats(&crate::resources::master_data::UnitName(
                UnitType::Tank.as_str().to_owned(),
            ))
            .unwrap();
        let tank = spawn_v3_test_unit(&mut world, player, 0, 100, stats);
        *world.get_mut::<GridPosition>(tank).unwrap() = GridPosition { x: 0, y: 0 };

        let mut manager = crate::ai::squad::SquadManager::new();
        let squad = manager.create_owned_squad(crate::ai::squad::MissionType::Attack, player);
        squad.members.insert(tank);
        squad.target = Some(GridPosition { x: 4, y: 0 });
        squad.phase = crate::ai::squad::MissionPhase::MovingToTarget;
        world.insert_resource(manager);

        // V3思考では敵生産マスの封鎖評価を行わないため、敵工場 (1, 0) に留まらず
        // 敵首都 (4, 0) に向けて最遠到達点 (3, 0) まで前進する。
        let (entity, command) = decide_ai_action_v2(&mut world, player, &HashSet::new())
            .expect("Tank should take an action");
        assert_eq!(entity, tank);
        let AiCommand::Wait { target_pos } = command else {
            panic!("Wait行動を期待したが {:?} であった", command);
        };
        assert_eq!(
            target_pos,
            GridPosition { x: 3, y: 0 },
            "V3の戦車は封鎖評価を行わず、目標へ向かって前進すべき"
        );
    }

    #[test]
    fn v4_healthy_combat_unit_occupies_reachable_enemy_production_site_to_blockade_reinforcements()
    {
        let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
        let player = PlayerId(2);
        let enemy = PlayerId(1);
        let mut world = setup_v3_test_world(6, crate::ai::ai_version::AiVersion::V4);
        world.insert_resource(Map {
            width: 6,
            height: 2,
            tiles: vec![
                Terrain::Plains,
                Terrain::Factory,
                Terrain::Plains,
                Terrain::Plains,
                Terrain::Plains,
                Terrain::Capital,
                Terrain::Plains,
                Terrain::Plains,
                Terrain::Plains,
                Terrain::Plains,
                Terrain::Plains,
                Terrain::Plains,
            ],
            topology: crate::resources::GridTopology::Square,
        });
        world.insert_resource(DamageChart::new());
        // 敵 (P1) の首都 (4, 0) と工場 (1, 0) (距離3以内で生産可能範囲内)
        world.spawn((
            GridPosition { x: 4, y: 0 },
            Property::new(Terrain::Capital, Some(enemy), 100),
        ));
        world.spawn((
            GridPosition { x: 1, y: 0 },
            Property::new(Terrain::Factory, Some(enemy), 100),
        ));
        // 敵首都 (4, 0) に敵守備隊がいるため進入不可
        let enemy_stats = world
            .resource::<MasterDataRegistry>()
            .create_unit_stats(&crate::resources::master_data::UnitName(
                UnitType::Infantry.as_str().to_owned(),
            ))
            .unwrap();
        spawn_v3_test_unit(&mut world, enemy, 4, 100, enemy_stats);

        // 自軍 (P2) の戦車が (0, 0) にいる (工場 (1, 0) にも、より首都に近い平地 (2, 0) や (3, 0) にも移動可能)
        let stats = world
            .resource::<MasterDataRegistry>()
            .create_unit_stats(&crate::resources::master_data::UnitName(
                UnitType::Tank.as_str().to_owned(),
            ))
            .unwrap();
        let tank = spawn_v3_test_unit(&mut world, player, 0, 100, stats);
        *world.get_mut::<GridPosition>(tank).unwrap() = GridPosition { x: 0, y: 0 };

        let mut manager = crate::ai::squad::SquadManager::new();
        let squad = manager.create_owned_squad(crate::ai::squad::MissionType::Attack, player);
        squad.members.insert(tank);
        squad.target = Some(GridPosition { x: 4, y: 0 }); // 敵首都を目標
        squad.phase = crate::ai::squad::MissionPhase::MovingToTarget;
        world.insert_resource(manager);

        let (entity, command) = decide_squad_action_v4(&mut world, player, &HashSet::new())
            .expect("Tank should take an action");
        assert_eq!(entity, tank);
        // 戦車は敵首都への単なる平地前進 (RouteAdvance) ではなく、敵の補充を物理的に封鎖できる工場 (1, 0) を占位してWaitすべき
        let AiCommand::Wait { target_pos } = command else {
            panic!("Wait行動を期待したが {:?} であった", command);
        };
        assert_eq!(
            target_pos,
            GridPosition { x: 1, y: 0 },
            "戦車は敵工場の生産を止めるために (1, 0) を占位すべき"
        );
    }

    #[test]
    fn v4_combat_unit_does_not_blockade_enemy_production_site_reserved_for_friendly_capture() {
        let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
        // 占位（blockade）は敵の補充を止めるだけだが、占領（capture）は補充を止めたうえで
        // 収入と生産力を自軍へ移すため常に上位互換である。
        // 友軍のCapture契約が既にあるマスを戦闘ユニットが塞ぐのは純損失なので、
        // 占位は占領へ道を譲らなければならない。
        let player = PlayerId(2);
        let enemy = PlayerId(1);
        let mut world = setup_v3_test_world(6, crate::ai::ai_version::AiVersion::V4);
        world.insert_resource(Map {
            width: 6,
            height: 2,
            tiles: vec![
                Terrain::Plains,
                Terrain::Factory,
                Terrain::Plains,
                Terrain::Plains,
                Terrain::Plains,
                Terrain::Capital,
                Terrain::Plains,
                Terrain::Plains,
                Terrain::Plains,
                Terrain::Plains,
                Terrain::Plains,
                Terrain::Plains,
            ],
            topology: crate::resources::GridTopology::Square,
        });
        world.insert_resource(DamageChart::new());
        // 敵 (P1) の首都 (4, 0) と工場 (1, 0)（生産可能範囲内）
        world.spawn((
            GridPosition { x: 4, y: 0 },
            Property::new(Terrain::Capital, Some(enemy), 100),
        ));
        world.spawn((
            GridPosition { x: 1, y: 0 },
            Property::new(Terrain::Factory, Some(enemy), 100),
        ));
        // 敵首都 (4, 0) に敵守備隊がいるため進入不可
        let enemy_stats = world
            .resource::<MasterDataRegistry>()
            .create_unit_stats(&crate::resources::master_data::UnitName(
                UnitType::Infantry.as_str().to_owned(),
            ))
            .unwrap();
        spawn_v3_test_unit(&mut world, enemy, 4, 100, enemy_stats);

        // 自軍 (P2) の戦車が (0, 0) にいる
        let stats = world
            .resource::<MasterDataRegistry>()
            .create_unit_stats(&crate::resources::master_data::UnitName(
                UnitType::Tank.as_str().to_owned(),
            ))
            .unwrap();
        let tank = spawn_v3_test_unit(&mut world, player, 0, 100, stats);
        *world.get_mut::<GridPosition>(tank).unwrap() = GridPosition { x: 0, y: 0 };

        let mut manager = crate::ai::squad::SquadManager::new();
        let squad = manager.create_owned_squad(crate::ai::squad::MissionType::Attack, player);
        squad.members.insert(tank);
        squad.target = Some(GridPosition { x: 4, y: 0 }); // 敵首都を目標
        squad.phase = crate::ai::squad::MissionPhase::MovingToTarget;
        // 友軍の占領部隊が敵工場 (1, 0) を占領目標として予約している。
        let capture_squad =
            manager.create_owned_squad(crate::ai::squad::MissionType::Capture, player);
        capture_squad.target = Some(GridPosition { x: 1, y: 0 });
        world.insert_resource(manager);

        let (entity, command) = decide_squad_action_v4(&mut world, player, &HashSet::new())
            .expect("Tank should take an action");
        assert_eq!(entity, tank);
        let target_pos = match command {
            AiCommand::Wait { target_pos } => target_pos,
            AiCommand::Attack { target_pos, .. } => target_pos,
            AiCommand::Capture { target_pos } => target_pos,
            other => panic!("位置を伴う行動を期待したが {:?} であった", other),
        };
        assert_ne!(
            target_pos,
            GridPosition { x: 1, y: 0 },
            "友軍が占領予約している敵工場 (1, 0) を戦闘ユニットが占位して塞いではならない"
        );
    }

    #[test]
    fn v4_healthy_combat_unit_holds_enemy_production_site_blockade() {
        let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
        let player = PlayerId(2);
        let enemy = PlayerId(1);
        let mut world = setup_v3_test_world(4, crate::ai::ai_version::AiVersion::V4);
        world.insert_resource(Map {
            width: 4,
            height: 2,
            tiles: vec![
                Terrain::Plains,
                Terrain::Factory,
                Terrain::Plains,
                Terrain::Capital,
                Terrain::Plains,
                Terrain::Plains,
                Terrain::Plains,
                Terrain::Plains,
            ],
            topology: crate::resources::GridTopology::Square,
        });
        world.insert_resource(DamageChart::new());
        world.spawn((
            GridPosition { x: 3, y: 0 },
            Property::new(Terrain::Capital, Some(enemy), 100),
        ));
        world.spawn((
            GridPosition { x: 1, y: 0 },
            Property::new(Terrain::Factory, Some(enemy), 100),
        ));
        // 自軍 (P2) の戦車が既に工場 (1, 0) 上にいる
        let stats = world
            .resource::<MasterDataRegistry>()
            .create_unit_stats(&crate::resources::master_data::UnitName(
                UnitType::Tank.as_str().to_owned(),
            ))
            .unwrap();
        let tank = spawn_v3_test_unit(&mut world, player, 0, 100, stats);
        *world.get_mut::<GridPosition>(tank).unwrap() = GridPosition { x: 1, y: 0 };

        let mut manager = crate::ai::squad::SquadManager::new();
        let squad = manager.create_owned_squad(crate::ai::squad::MissionType::Attack, player);
        squad.members.insert(tank);
        // 目標は生産施設ではない平地 (0, 1)。敵首都 (3, 0) も占位対象なので、
        // そちらへ引かれる目標にすると「二つの占位マスのどちらを選ぶか」になってしまい、
        // 封鎖を維持するかどうかを検証できない。占位マスから引き離す目標を与える。
        squad.target = Some(GridPosition { x: 0, y: 1 });
        squad.phase = crate::ai::squad::MissionPhase::MovingToTarget;
        world.insert_resource(manager);

        let (entity, command) = decide_squad_action_v4(&mut world, player, &HashSet::new())
            .expect("Tank should take an action");
        assert_eq!(entity, tank);
        // 戦車は目標へ不用意に前進して工場を明け渡すのではなく、工場 (1, 0) で待機して封鎖を維持すべき
        let AiCommand::Wait { target_pos } = command else {
            panic!("Wait行動を期待したが {:?} であった", command);
        };
        assert_eq!(
            target_pos,
            GridPosition { x: 1, y: 0 },
            "戦車は敵工場 (1, 0) の封鎖を維持すべき"
        );
    }
}

/// V4 Squad executorが同じ任務だけを連続実行しないための巡回位置。
///
/// Entity単位の全体順位へ戻さず、Squadを命令主体に保ったまま、攻勢・占領・防衛の
/// 進行を1アクションずつ交互にする。
#[derive(Resource, Debug, Default)]
pub(super) struct V4SquadExecutionCursor {
    last_squad: HashMap<PlayerId, crate::ai::squad::SquadId>,
}

/// 生産施設が1ターンに供給し得るユニットの平均コスト（補充阻止価値）。
/// 敵の生産マスを1つ占位することは、敵の補充を毎ターン1体分止めることであり、
/// その価値は止めた補充の価値に等しいという教義に基づく。
/// 魔法定数を避け、マスターデータのユニットカタログから導出する。
pub(super) fn production_facility_blockade_value(
    terrain: Terrain,
    registry: &MasterDataRegistry,
) -> u32 {
    let producible_costs: Vec<u32> = registry
        .unit_order
        .iter()
        .filter_map(|name| {
            let record = registry.units.get(name)?;
            let u_type = crate::resources::UnitType::from_str(&record.name.0)?;
            registry
                .can_produce_unit(terrain.as_str(), u_type)
                .then_some(record.cost)
        })
        .collect();
    if producible_costs.is_empty() {
        0
    } else {
        producible_costs.iter().sum::<u32>() / producible_costs.len() as u32
    }
}

/// 敵の補充依存度を分数 (分子, 分母) で返す。占位（blockade）の実効価値係数。
///
/// 占位が止められるのは「敵生産マス1つあたり毎ターン1体の補充」であり、
/// その価値は敵の次ターン戦力に占める補充の割合、すなわち
/// 生産能力 / (生産能力 + 現有戦力) に比例する。
///
/// - 敵が大軍を擁する序盤〜中盤はこの割合が小さい。補充1体を止める利得より、
///   自軍1体を占位に貼り付けて遊ばせる損のほうが大きいため、占位は事実上無効になる。
/// - 敵が消耗し、補充だけが生命線になった終盤で最大化し、全滅勝利を後押しする。
///
/// 魔法定数を持たず、盤面から導出した比率だけで判断する。
pub(super) fn enemy_replacement_dependence(
    production_capacity: usize,
    fielded_units: usize,
) -> (u32, u32) {
    let capacity = production_capacity as u32;
    let denominator = capacity.saturating_add(fielded_units as u32);
    if denominator == 0 {
        // 生産能力も現有戦力もない敵に対して、止めるべき補充は存在しない。
        (0, 1)
    } else {
        (capacity, denominator)
    }
}

/// Squad executorから渡されたmember集合だけを戦術採点する内部入口。
///
/// V2/V3の公開関数は従来どおり全unitを対象にする。V4だけはSquadが選んだ
/// candidate集合を渡すため、ここは戦略目標を新たに発明せず、Move/Attack/Captureの
/// 戦術順位だけを決める。
pub(super) fn decide_ai_action_v2_for_entities(
    world: &mut World,
    player_id: PlayerId,
    skip_entities: &std::collections::HashSet<Entity>,
    permitted_entities: Option<&HashSet<Entity>>,
) -> Option<(Entity, AiCommand)> {
    // V3 の戦術評価 (#44/#45/#50) を有効にするかどうか
    let ai_version = crate::ai::resolve_player_ai_version(world, player_id);
    let is_v3 = ai_version.uses_v3_tactics();
    // 敵生産マス占位（blockade）を有効にするかどうか。
    // V4限定。V3は対戦評価の基準線（ベースライン）として挙動を不変に維持する。
    let uses_blockade = ai_version.uses_enemy_production_blockade();

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

            if permitted_entities.is_none_or(|permitted| permitted.contains(&entity))
                && !skip_entities.contains(&entity)
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

    // 2. 正規化済みの所属を満たすSquadだけから、各unitの目標を取得する。
    // SquadManagerをそのまま全走査すると、再編途中に残った重複memberが別の目標を
    // 上書きできてしまう。UnitOperationRegistryの唯一のSquadIdを境界にする。
    let manager = world
        .get_resource::<crate::ai::squad::SquadManager>()
        .cloned()
        .unwrap_or_default();
    let operation_assignments =
        world.get_resource::<crate::ai::operation_assignment::UnitOperationRegistry>();
    let mut unit_squad_targets = HashMap::new();
    let mut unit_squad_missions = HashMap::new();
    let mut solo_fallbacks = HashSet::new();

    for squad in &manager.squads {
        for &member in &squad.members {
            if operation_assignments.is_some_and(|assignments| {
                assignments
                    .assignment(member)
                    .is_none_or(|assignment| assignment.squad_id != Some(squad.id))
            }) {
                continue;
            }
            unit_squad_missions.insert(member, squad.mission_type.clone());
            if let Some(target) =
                crate::ai::v4::capital_route_tactical_target(world, player_id, member)
                    .unwrap_or(squad.target)
            {
                unit_squad_targets.insert(member, target);
            }
        }
    }
    for &solo in &manager.solo_fallbacks {
        solo_fallbacks.insert(solo);
    }

    let map = world.resource::<Map>().clone();
    let registry = world.resource::<MasterDataRegistry>().clone();
    let tactical_snapshot = AiTacticalSnapshot::from_world(world, &registry, map.topology);
    let properties: Vec<(GridPosition, Terrain, Option<PlayerId>)> = {
        let mut q = world.query::<(&GridPosition, &Property)>();
        q.iter(world)
            .map(|(p, prop)| (*p, prop.terrain, prop.owner_id))
            .collect()
    };
    let capital_positions = properties
        .iter()
        .filter_map(|(position, terrain, owner)| {
            (*owner == Some(player_id) && *terrain == Terrain::Capital).then_some(*position)
        })
        .collect::<Vec<_>>();
    let owned_production_positions = properties
        .iter()
        .filter_map(|(position, terrain, owner)| {
            (*owner == Some(player_id)
                && registry.is_production_facility(terrain.as_str())
                && crate::systems::production::is_within_production_range(
                    &capital_positions,
                    position.x,
                    position.y,
                    map.topology,
                ))
            .then_some((position.x, position.y))
        })
        .collect::<HashSet<_>>();
    // 敵プレイヤーの有効な生産施設を収集する。
    // 敵首都からの生産可能範囲内にある敵所有の生産施設（工場・空港・港・首都）が該当する。
    // 自軍の生産口を塞がない知識を攻勢的に反転させ、敵の補充を物理的に遮断（占位）する対象を特定する。
    // 敵プレイヤーの有効な生産施設を収集する。
    // 敵首都からの生産可能範囲内にある敵所有の生産施設（工場・空港・港・首都）が該当する。
    // 自軍の生産口を塞がない知識を攻勢的に反転させ、敵の補充を物理的に遮断（占位）する対象を特定する。
    // V4限定（V3は対戦評価の基準線として挙動を不変に保つため収集をスキップする）。
    let enemy_production_positions: HashSet<GridPosition> = if uses_blockade {
        let enemy_capitals: Vec<(PlayerId, GridPosition)> = properties
            .iter()
            .filter_map(|(pos, terrain, owner)| match owner {
                Some(owner_id) if *owner_id != player_id && *terrain == Terrain::Capital => {
                    Some((*owner_id, *pos))
                }
                _ => None,
            })
            .collect();
        properties
            .iter()
            .filter_map(|(pos, terrain, owner)| match owner {
                Some(owner_id)
                    if *owner_id != player_id
                        && registry.is_production_facility(terrain.as_str()) =>
                {
                    let matching_capitals: Vec<GridPosition> = enemy_capitals
                        .iter()
                        .filter_map(|(c_owner, c_pos)| (*c_owner == *owner_id).then_some(*c_pos))
                        .collect();
                    if crate::systems::production::is_within_production_range(
                        &matching_capitals,
                        pos.x,
                        pos.y,
                        map.topology,
                    ) {
                        Some(*pos)
                    } else {
                        None
                    }
                }
                _ => None,
            })
            .collect()
    } else {
        HashSet::new()
    };
    // 一つでも空きがあれば通常の戦術行動を優先する。全枠閉塞時だけ、各action後に
    // 再評価されるこの契約で一体を押し出し、最低一つの後続生産口を回復する。
    let production_capacity_gridlocked = !owned_production_positions.is_empty()
        && owned_production_positions
            .iter()
            .all(|position| unit_positions.contains_key(position));
    let unit_costs: HashMap<Entity, u32> = {
        let mut query = world.query::<(Entity, &UnitStats)>();
        query
            .iter(world)
            .map(|(entity, stats)| (entity, stats.cost))
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
        let mut q = world.query::<(
            &GridPosition,
            &Faction,
            &UnitStats,
            &Health,
            Option<&crate::components::CargoCapacity>,
            Option<&crate::components::Transporting>,
        )>();
        q.iter(world)
            // 輸送中の兵は輸送ユニットの価値へ畳み込み、盤外の独立標的にはしない。
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
    // 敵の補充依存度。占位の価値は局面で決まるため、ここで一度だけ導出して全タイル評価で共有する。
    let blockade_dependence =
        enemy_replacement_dependence(enemy_production_positions.len(), enemy_units.len());
    let damage_chart = world.resource::<crate::resources::DamageChart>().clone();
    // 自軍の移動は距離探索上の通行可否を変えない。敵が撃破されて占有マスが
    // 変わるまで、同一手番の全ユニットでDijkstra結果を共有する。
    let mut enemy_blocker_positions = enemy_units
        .iter()
        .map(|(position, _, _, _, _, _, _)| (position.x, position.y))
        .collect::<Vec<_>>();
    enemy_blocker_positions.sort_unstable();
    let mut strategy_cache = world
        .remove_resource::<AiTurnStrategyCache>()
        .unwrap_or_default();
    let mut turn_cache =
        strategy_cache.take_action_distance_cache(player_id, &enemy_blocker_positions);
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
        let starts_on_owned_production_site = owned_production_positions.contains(&(pos.x, pos.y));
        let deployment_target = world
            .get_resource::<crate::ai::v4::deployment::V4DeploymentRegistry>()
            .and_then(|registry| registry.attack_target(unit_entity));
        // `Some(None)` はDAGのControl地域に入ったことを表す。ここで単なるNoneへ
        // 潰すと、下の古いSquad目標が再び地域外のanchorを指してしまう。
        let route_tactical_target =
            crate::ai::v4::capital_route_tactical_target(world, player_id, unit_entity);
        let route_target = route_tactical_target.flatten();
        // DAG再束縛前でも、RoadmapがCapture Squadへ渡した未所有物件は行動契約として
        // 有効にする。Squad.targetを捨てて集結待ちへ戻すと、複数の占領レーンを同時に
        // 開く小規模mapの初動そのものが失われる。
        let small_map_capture_target =
            (crate::ai::strategy_profile::current_profile(world, player_id)
                == crate::ai::strategy_profile::V4StrategyProfile::SmallMapExpansion
                && stats.can_capture)
                .then_some(route_target.or(unit_squad_targets.get(&unit_entity).copied()))
                .flatten()
                .filter(|target| {
                    has_capturable_property_at(world, *target, player_id)
                        && crate::ai::v4::capital_route_allows_capture_at(
                            world,
                            player_id,
                            unit_entity,
                            *target,
                        )
                });
        let route_recovering =
            crate::ai::v4::capital_route_is_recovering(world, player_id, unit_entity);
        let campaign_context =
            match campaign_action_context(world, player_id, unit_entity, &manager) {
                Ok(context) => context,
                // Campaign ownerに具体Squadが無いEntityへ汎用行動を許すと、再び別作戦へ
                // 漏れるため、この手番は作戦再構築へ戻す。ただしDAGの実行区間を持つ
                // Entityは、DAG自身が一意の行動先を持つのでここで落とさない。
                Err(()) if route_tactical_target.is_some() => None,
                Err(()) => continue,
            };

        // #44 (V3): 敵の脅威がこのユニットの近傍にあるか (森・山への退避を
        // 意味のある局面に限定するためのゲート)。敵の攻撃到達圏 (移動+射程) を
        // 少し余裕をもって見た半径で判定する
        const THREAT_PROXIMITY_RADIUS: u32 = 8;
        let enemy_threat_nearby = enemy_units.iter().any(|(e_pos, _, _, _, _, _, _)| {
            (e_pos.x.abs_diff(pos.x) + e_pos.y.abs_diff(pos.y)) as u32 <= THREAT_PROXIMITY_RADIUS
        });

        let capture_waits_for_local_escort = campaign_context.as_ref().is_some_and(|context| {
            context.mission_type == crate::ai::squad::MissionType::Capture
                && !context.departure_authorized
        }) && small_map_capture_target.is_none();
        let reachable = if capture_waits_for_local_escort {
            // 現在位置での攻撃・占領・Waitは残すが、護衛を待つ1手番に単独前進しない。
            std::iter::once((pos.x, pos.y)).collect()
        } else {
            calculate_reachable_tiles(
                &map,
                &unit_positions,
                (pos.x, pos.y),
                stats.movement_type,
                stats.max_movement,
                fuel,
                player_id,
                stats.unit_type,
                &registry,
            )
        };
        let route_advance_destination = crate::ai::v4::capital_route_advance_destination(
            world,
            player_id,
            unit_entity,
            &reachable,
        );
        // 進軍役は山を迂回するDAGセル列の次候補と、その場の戦闘・占領だけを比較する。
        // それ以外の横方向への通常探索を残すと、橋へ近づくスコアだけで経路から外れる。
        let mut candidate_tiles = if small_map_capture_target.is_some() {
            reachable.iter().copied().collect::<Vec<_>>()
        } else if let Some(destination) = route_advance_destination {
            let advance_dest_blocked = (destination.x == pos.x && destination.y == pos.y)
                || unit_positions.contains_key(&(destination.x, destination.y));
            if starts_on_owned_production_site && advance_dest_blocked {
                // 工場マス上にいてDAGの次マスが味方等で塞がっている場合、一本道に縛られて
                // 工場上でその場Waitすると後続の生産を永久に封鎖する。
                // 周囲の合法な非生産マスを候補に含めて、工場から脱出できるようにする。
                let mut tiles = vec![(pos.x, pos.y)];
                if reachable.contains(&(destination.x, destination.y)) {
                    tiles.push((destination.x, destination.y));
                }
                tiles.extend(
                    reachable
                        .iter()
                        .copied()
                        .filter(|(x, y)| !owned_production_positions.contains(&(*x, *y))),
                );
                tiles.sort_unstable();
                tiles.dedup();
                tiles
            } else {
                [(pos.x, pos.y), (destination.x, destination.y)]
                    .into_iter()
                    .filter(|tile| reachable.contains(tile))
                    .collect::<Vec<_>>()
            }
        } else {
            reachable
                .iter()
                .copied()
                .filter(|(x, y)| {
                    crate::ai::v4::capital_route_allows_tactical_position(
                        world,
                        player_id,
                        unit_entity,
                        GridPosition { x: *x, y: *y },
                    )
                    .unwrap_or(true)
                })
                .collect()
        };
        if let Some(planned_target) = deployment_target {
            // 生産計画が具体的な敵Entityとの初撃を契約した場合、DAGの一本道だけで
            // 移動候補を制限しない。実際にこの手番で射撃可能な全セルを加えることで、
            // 複数の快速直接戦闘unitが同じ入口へ直列化せず、先行unitが使った射撃位置を
            // 避けて別方向から同じblockerへ集中攻撃できる。
            candidate_tiles.extend(reachable.iter().copied().filter(|(x, y)| {
                tactical_snapshot
                    .action_targets_at(
                        unit_entity,
                        &stats,
                        GridPosition { x: *x, y: *y },
                        *x != pos.x || *y != pos.y,
                    )
                    .attackable_targets
                    .contains(&planned_target)
            }));
            candidate_tiles.sort_unstable();
            candidate_tiles.dedup();
        }
        // 非占領戦闘ユニットが自軍の未所有な占領予定物件を踏んで占領を物理的に塞ぐのを防ぐ。
        // （既に自軍が所有している物件は防衛・駐留すべき拠点なので除外しない）
        // 敵の生産マスも例外にしない。占位（blockade）は敵の補充を止めるだけだが、
        // 占領（capture）は補充を止めたうえで収入と生産力を自軍へ移すため常に上位互換であり、
        // 友軍の占領契約が既にあるマスを戦闘ユニットが塞ぐのは純損失になる。
        // 占位は「誰も占領しに行かないマス」に対する代替手段であり、占領の競合相手ではない。
        if !stats.can_capture {
            let non_reserved = candidate_tiles
                .iter()
                .copied()
                .filter(|&(x, y)| {
                    let target_grid = GridPosition { x, y };
                    let is_unowned = properties.iter().any(|(p_pos, _, p_owner)| {
                        p_pos.x == x && p_pos.y == y && *p_owner != Some(player_id)
                    });
                    if !is_unowned {
                        return true;
                    }
                    if !crate::ai::v4::is_capture_target_reserved(world, player_id, target_grid) {
                        return true;
                    }
                    false
                })
                .collect::<Vec<_>>();
            if !non_reserved.is_empty() {
                candidate_tiles = non_reserved;
            }
        }
        // 健全なunitが行動終了できる非生産セルを一つでも持つなら、Waitで自軍工場へ
        // 入る／残る必要はない。価格スコアで減点するだけではSquad接近点に負けて、
        // map_1のような小規模盤で翌手番の実生産slotを失うため、物理的な排他条件にする。
        // 攻撃・占領は下で別に評価するので、工場セルでしか成立しない即時戦闘は消さない。
        let has_non_production_wait_destination = candidate_tiles.iter().any(|(x, y)| {
            (*x != pos.x || *y != pos.y)
                && !owned_production_positions.contains(&(*x, *y))
                && tactical_snapshot
                    .action_targets_at(unit_entity, &stats, GridPosition { x: *x, y: *y }, true)
                    .can_wait
        });
        let can_end_turn_off_production = candidate_tiles.iter().any(|(x, y)| {
            let is_stationary = *x == pos.x && *y == pos.y;
            !owned_production_positions.contains(&(*x, *y))
                && tactical_snapshot
                    .action_targets_at(
                        unit_entity,
                        &stats,
                        GridPosition { x: *x, y: *y },
                        !is_stationary,
                    )
                    .can_wait
        });

        // 回復中のDAG Entityへ古いSquad目標を渡すと、修理ではなく横の拠点へ戻る。
        // 所属はDAG Registryに残したまま、ここだけ通常の回復探索を使う。
        let squad_target = if route_recovering {
            None
        } else {
            route_tactical_target
                .flatten()
                .or(unit_squad_targets.get(&unit_entity).copied())
        };
        let has_offensive_mission = !route_recovering
            && (route_tactical_target.is_some()
                || unit_squad_missions
                    .get(&unit_entity)
                    .is_some_and(|mission| {
                        matches!(
                            mission,
                            crate::ai::squad::MissionType::Attack
                                | crate::ai::squad::MissionType::Capture
                                | crate::ai::squad::MissionType::Transport
                        )
                    }));
        let is_capture_mission = campaign_context
            .as_ref()
            .is_some_and(|context| context.mission_type == crate::ai::squad::MissionType::Capture)
            || unit_squad_missions
                .get(&unit_entity)
                .is_some_and(|mission| *mission == crate::ai::squad::MissionType::Capture);
        // Captureは面制圧パッケージの後続工程であり、一般敵を探す攻勢任務ではない。
        // 目標物件を直接塞ぐ敵だけは下のAttack判定で戦略標的として扱う。
        let has_offensive_mission = has_offensive_mission && !is_capture_mission;
        let capture_mission_target = is_capture_mission.then_some(squad_target).flatten();
        let capture_distance_before = capture_mission_target.map(|target| {
            calculate_turn_distance(
                &map,
                &registry,
                &unit_positions,
                (pos.x, pos.y),
                (target.x, target.y),
                stats.movement_type,
                stats.max_movement,
                0,
                player_id,
                &mut turn_cache,
            )
        });
        let initial_is_solo = route_recovering
            || (solo_fallbacks.contains(&unit_entity) && route_tactical_target.is_none())
            || squad_target.is_none();

        // 評価ロジック（is_solo: initial_is_solo を直接使う）
        let is_solo = initial_is_solo;
        let mut best_unit_rank = (ActionPriority::Normal, i32::MIN);
        let mut best_unit_choice: Option<AiCommand> = None;
        // 現在位置から撃てる最良の攻撃候補（順位と対象）。V4の間接ユニットは
        // 移動すると同一手番で射撃できないため、移動候補との比較に使う。
        let mut best_stationary_attack: Option<((ActionPriority, i32), Entity)> = None;

        let support_destination = (ai_version == crate::ai::ai_version::AiVersion::V4
            && stats.min_range > 1
            && !is_combat_ineffective)
            .then(|| {
                coordinated_support_destination(
                    world,
                    player_id,
                    unit_entity,
                    &stats,
                    pos,
                    squad_target,
                    &candidate_tiles,
                    &unit_positions,
                    &manager,
                    &tactical_snapshot,
                    &map,
                    &registry,
                    &damage_chart,
                )
            })
            .flatten();

        for target_tile in &candidate_tiles {
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
            // 現在セルはDAG上の合法セルでも「前進」ではない。停止Waitへ
            // RouteAdvance優先度を与えると、満杯の生産施設上で作戦所属unitが
            // 毎手番待機し、後続生産を永久に封鎖する。
            let is_route_advance_cell =
                !is_stationary && route_advance_destination == Some(current_grid);
            let vacates_gridlocked_production_site = production_capacity_gridlocked
                && starts_on_owned_production_site
                && !is_combat_ineffective
                && !owned_production_positions.contains(&(current_grid.x, current_grid.y));

            let actions = tactical_snapshot.action_targets_at(
                unit_entity,
                &stats,
                current_grid,
                !is_stationary,
            );
            let advances_capture_contract = capture_mission_target
                .zip(capture_distance_before)
                .is_some_and(|(target, before)| {
                    current_grid == target
                        || calculate_turn_distance(
                            &map,
                            &registry,
                            &unit_positions,
                            (current_grid.x, current_grid.y),
                            (target.x, target.y),
                            stats.movement_type,
                            stats.max_movement,
                            0,
                            player_id,
                            &mut turn_cache,
                        ) < before
                });
            let mut base_tile_score = 0;
            let tile_def_bonus = map
                .get_terrain(current_grid.x, current_grid.y)
                .map(|t| registry.get_terrain_defense_bonus(t))
                .unwrap_or(0);
            base_tile_score += tile_def_bonus as i32 * 10;

            // 敵生産マスの占位（Blockade）評価。
            // V4限定の戦術行動であり、V3ではベースライン維持のため完全に無効化する。
            let (
                starts_on_enemy_production_site,
                is_on_enemy_production_site,
                blockade_value,
                is_survivable_blockade,
            ) = if uses_blockade {
                let starts_on = enemy_production_positions.contains(&pos);
                let is_on = enemy_production_positions.contains(&current_grid);
                let enemy_site_terrain = if is_on {
                    properties
                        .iter()
                        .find(|(p_pos, _, _)| *p_pos == current_grid)
                        .map(|(_, t, _)| *t)
                } else {
                    None
                };
                // 施設が供給し得る補充の価値を、敵がその補充にどれだけ依存しているかで割り引く。
                let val = enemy_site_terrain
                    .map(|t| production_facility_blockade_value(t, &registry))
                    .unwrap_or(0)
                    .saturating_mul(blockade_dependence.0)
                    / blockade_dependence.1;
                let incoming_damage = crate::ai::threat::expected_incoming_damage(
                    &map,
                    (current_grid.x, current_grid.y),
                    stats.unit_type,
                    tile_def_bonus,
                    &enemy_units,
                    &damage_chart,
                );
                // 敵生産口への突入・占位の生存性判定。
                // 序盤の自殺的突入を抑止するため、戦闘不能ユニットや次ターン即死圏（期待被ダメージ >= 現在HP）は占位行動から除外する。
                let survivable = !is_combat_ineffective && incoming_damage < atk_hp;
                (starts_on, is_on, val, survivable)
            } else {
                (false, false, 0, false)
            };

            if is_on_enemy_production_site && is_survivable_blockade {
                base_tile_score += blockade_value as i32;
            }

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

            // V4: 間接攻撃ユニットの支援追随ポジショニング。
            // 前衛支援目標を射程に収める射点へ加点し、射程外への前進を抑止する。
            if ai_version == crate::ai::ai_version::AiVersion::V4 && stats.min_range > 1 {
                if let Some(target) = squad_target {
                    let d = map.distance(target.x, target.y, current_grid.x, current_grid.y);
                    if d >= stats.min_range && d <= stats.max_range {
                        base_tile_score += 5000;
                    }
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

            // 2. 損耗・戦闘不能時の回復拠点退避・合流、または SoloFallback のインセンティブ
            let mut nearest_repair_turns: Option<u32> = None;
            if is_combat_ineffective {
                let mut min_score: Option<(crate::ai::turn_distance::TurnDistance, i32)> = None;
                for (p_pos, p_terrain, p_owner) in &properties {
                    if *p_owner == Some(player_id)
                        && registry.can_repair_on_terrain(stats.unit_type, *p_terrain)
                    {
                        // 健全な味方なら移動して道を譲れるが、既に他の損耗ユニットが
                        // 修理滞在中（HP < 100）の都市は空きがなく回復を受け入れられない
                        let city_occupied_by_repairing_ally = world.iter_entities().any(|e| {
                            e.id() != unit_entity
                                && e.get::<GridPosition>() == Some(p_pos)
                                && e.get::<Health>().is_some_and(|h| h.current < 100)
                        });
                        if city_occupied_by_repairing_ally {
                            continue;
                        }

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
                    nearest_repair_turns = Some(d.turns);
                    if d.turns < 99 {
                        let p = m as f32 / stats.max_movement as f32;
                        base_tile_score += (100 - d.turns as i32).max(0) * 1000;
                        base_tile_score += ((100.0 - p).max(0.0) * 2000.0) as i32;
                    }
                }
            } else if is_solo {
                if !stats.can_capture {
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
            if actions.can_capture
                && crate::ai::v4::capital_route_allows_capture_at(
                    world,
                    player_id,
                    unit_entity,
                    current_grid,
                )
                && !campaign_context.as_ref().is_some_and(|context| {
                    !campaign_mission_allows_capture(&context.mission_type)
                        || !campaign_position_is_on_target_island(world, context, current_grid)
                })
            {
                let score = base_tile_score + 10000;
                let rank = (
                    if vacates_gridlocked_production_site {
                        ActionPriority::ProductionSiteRelief
                    } else if advances_capture_contract {
                        ActionPriority::CaptureExecution
                    } else if is_route_advance_cell {
                        ActionPriority::RouteAdvance
                    } else {
                        ActionPriority::Normal
                    },
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
            // 工場上の砲撃も合法な戦闘候補。工場を空けられるという理由だけで
            // 候補から消すと、射撃による損害と退去の効果を比較できなくなる。
            if !actions.attackable_targets.is_empty() {
                for target_entity in actions.attackable_targets.iter().copied() {
                    if campaign_context.as_ref().is_some_and(|context| {
                        let target_position = world.get::<GridPosition>(target_entity).copied();
                        context.mission_type == crate::ai::squad::MissionType::Transport
                            || (deployment_target != Some(target_entity)
                                && !target_position.is_some_and(|position| {
                                    campaign_position_is_on_target_island(world, context, position)
                                }))
                    }) {
                        continue;
                    }
                    let Some(exchange) = crate::ai::pruning::evaluate_attack_exchange(
                        world,
                        unit_entity,
                        target_entity,
                        current_grid,
                        &damage_chart,
                    ) else {
                        continue;
                    };

                    // ターゲットの詳細を取得してスコアを加点
                    if let (Some(t_stats), Some(t_health), Some(t_pos), Some(t_faction)) = (
                        world.get::<UnitStats>(target_entity),
                        world.get::<Health>(target_entity),
                        world.get::<GridPosition>(target_entity),
                        world.get::<Faction>(target_entity),
                    ) {
                        // 撃破判定・ダメージ期待値は、移動後位置を渡した交換予測を正本とする。
                        let expected_actual_damage = exchange.expected_damage;

                        // 期待ダメージが0の場合は攻撃候補から外す
                        if expected_actual_damage == 0 {
                            continue;
                        }

                        let mut attack_score = 2000;
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
                        // 金額ROIを禁止条件にはしない。同じ作戦島で複数の敵を撃てる場合だけ、
                        // 双方のHP損耗率差を使って相性のよい対象へ戦術的に切り替える。
                        attack_score += exchange.matchup_margin() / 2;

                        if is_combat_ineffective && expected_actual_damage < t_health.current {
                            attack_score -= 3000;
                        }

                        if expected_actual_damage >= t_health.current {
                            attack_score += 5000;
                        }

                        let score = base_tile_score + attack_score;
                        // V4はEntity単位のdeployment target、V2/V3は攻勢Squad自体を
                        // 戦略上の必要性として扱う。上陸cargoはSquad再編の境界でCapture、
                        // Transport、Attackのいずれにもなり得るため、その差で必要攻撃を
                        // 非決定的に枝刈りしてはならない。
                        let blocks_capture_target = is_capture_mission
                            && capture_mission_target.is_some_and(|target| target == *t_pos);
                        let is_strategic_target = if is_capture_mission {
                            blocks_capture_target
                        } else {
                            deployment_target
                                .map_or(has_offensive_mission, |target| target == target_entity)
                        };
                        let has_strategic_mission = deployment_target.is_some()
                            || has_offensive_mission
                            || blocks_capture_target;
                        // 今ターンで対象を撃破し、かつ同時解決の反撃を受けても自身が
                        // 生存できる攻撃かどうか。相性やROIとは独立に、機会を失う
                        // 事故（瀕死の隣接敵をWaitで見逃す）だけを避けるための判定であり、
                        // 特定のマップ・兵種・HPへ分岐する条件ではない。
                        let is_survivable_lethal_strike = expected_actual_damage
                            >= t_health.current
                            && exchange.expected_counter_damage < atk_hp;
                        let priority = match (is_strategic_target, exchange.is_favorable_matchup())
                        {
                            (true, true) => ActionPriority::FavorableStrategicTarget,
                            (false, true) if has_strategic_mission => {
                                ActionPriority::FavorableLocalTarget
                            }
                            // 生産・集結を終えてExecuteとなった作戦Entityは、他に有利な敵が
                            // いない場合だけ不利交換も受容する。ROIだけで戦略を停止しない。
                            (true, false) => ActionPriority::StrategicTargetFallback,
                            // V4限定: 実際に攻勢任務・作戦を抱えるEntityは、戦略標的でも
                            // 有利相性でもない交換であっても、生存可能な致死攻撃までは
                            // 消してはならない。T13戦車が瀕死の隣接敵をWaitで見逃した事故の再発防止。
                            (false, false)
                                if ai_version == crate::ai::ai_version::AiVersion::V4
                                    && has_strategic_mission
                                    && is_survivable_lethal_strike =>
                            {
                                ActionPriority::Normal
                            }
                            // 作戦上の必要性がない不利交換は、従来どおり候補外とする。
                            (false, false) => continue,
                            (false, true) => ActionPriority::Normal,
                        };
                        let priority = if vacates_gridlocked_production_site {
                            ActionPriority::ProductionSiteRelief
                        } else if is_on_enemy_production_site
                            && is_survivable_blockade
                            && priority < ActionPriority::EnemyProductionBlockade
                        {
                            ActionPriority::EnemyProductionBlockade
                        } else {
                            priority
                        };
                        let rank = (priority, score);
                        if is_stationary
                            && best_stationary_attack.is_none_or(|(best, _)| rank > best)
                        {
                            best_stationary_attack = Some((rank, target_entity));
                        }
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
                let mut is_on_production_site = false;
                for (p_pos, p_terrain, p_owner) in &properties {
                    if p_pos.x == current_grid.x
                        && p_pos.y == current_grid.y
                        && *p_owner == Some(player_id)
                        && registry.can_repair_on_terrain(stats.unit_type, *p_terrain)
                    {
                        is_on_recovery_property = true;
                        is_on_production_site = registry.is_production_facility(p_terrain.as_str());
                        break;
                    }
                }

                let wait_occupies_owned_production_site =
                    owned_production_positions.contains(&(current_grid.x, current_grid.y));
                let enters_production_site_for_recovery = is_combat_ineffective
                    && wait_occupies_owned_production_site
                    && !starts_on_owned_production_site
                    && can_end_turn_off_production;
                let healthy_unit_blocks_production = !is_combat_ineffective
                    && wait_occupies_owned_production_site
                    && (has_non_production_wait_destination || !starts_on_owned_production_site);
                if enters_production_site_for_recovery || healthy_unit_blocks_production {
                    // 工場外で行動終了できる消耗unitを、回復点という理由だけで工場へ
                    // 逆走させない。回復量と固定scoreを比較する問題ではなく、次手番の
                    // 生産slotを物理的に失う行動を除く。既に工場上で損耗しているunitは
                    // 代替回復点が無い場合もあるため、ここでは退去を強制しない。
                    continue;
                }

                if is_on_recovery_property {
                    if is_combat_ineffective {
                        score += 8000;
                    } else if atk_hp < 100 || atk_ammo.0 < stats.max_ammo1 {
                        score += 1000;
                    } else if is_on_production_site {
                        // 回復も攻撃も不要なのに空港・工場上で待機すると、次の生産を
                        // 物理的に封鎖する。別タイルへ移動するWaitを十分に優先させる。
                        score -= 20_000;
                    } else {
                        // 生産施設ではない回復拠点も、用がなければ占有し続けない。
                        score -= 2000;
                    }
                } else if is_combat_ineffective {
                    score -= 5000;
                }

                let qualifies_enemy_production_blockade =
                    is_on_enemy_production_site && is_survivable_blockade;
                // 占位を維持するときの価値は base_tile_score で既に一度計上済みである。
                // ここで居座り加点を重ねると同じ一つの価値を二重計上することになり、
                // 占位マスが実力以上に固着してユニットが生産マスに貼り付いて遊ぶ。
                // 占位を続けるかどうかは、計上済みの占位価値と他の行動価値の素の比較に委ねる。
                if !qualifies_enemy_production_blockade
                    && starts_on_enemy_production_site
                    && !is_combat_ineffective
                {
                    // 健全なユニットが理由なく敵生産マスから退去して敵に生産口を明け渡す行動を抑止する。
                    // 自軍生産口を塞ぐのを避けるのと同様に、敵生産口を空けるのを避ける。
                    score -= (blockade_value as i32) * 2;
                }

                let violates_campaign_step = campaign_context.as_ref().is_some_and(|context| {
                    if is_stationary
                        || (is_on_recovery_property && is_combat_ineffective)
                        || qualifies_enemy_production_blockade
                    {
                        return false;
                    }
                    route_target.or(context.target).is_none_or(|target| {
                        map.distance(current_grid.x, current_grid.y, target.x, target.y)
                            > map.distance(pos.x, pos.y, target.x, target.y)
                    })
                });

                if !violates_campaign_step {
                    let rank = (
                        if vacates_gridlocked_production_site {
                            ActionPriority::ProductionSiteRelief
                        } else if advances_capture_contract {
                            ActionPriority::CaptureAdvance
                        } else if qualifies_enemy_production_blockade {
                            ActionPriority::EnemyProductionBlockade
                        } else if is_route_advance_cell {
                            ActionPriority::RouteAdvance
                        } else if support_destination == Some(current_grid) {
                            ActionPriority::SupportAdvance
                        } else {
                            ActionPriority::Normal
                        },
                        score,
                    );
                    if rank > best_unit_rank {
                        best_unit_rank = rank;
                        best_unit_choice = Some(AiCommand::Wait {
                            target_pos: current_grid,
                        });
                    }
                }
            }

            // (D) Merge
            if !actions.mergeable_targets.is_empty() {
                for target_entity in actions.mergeable_targets.iter().copied() {
                    if campaign_context.as_ref().is_some_and(|context| {
                        world
                            .get_resource::<crate::ai::operation_assignment::UnitOperationRegistry>(
                            )
                            .and_then(|assignments| assignments.assignment(target_entity))
                            .is_none_or(|assignment| assignment.owner != context.owner)
                    }) {
                        continue;
                    }
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

                        // 前線即時合流（TacticalMerge）の明確な判断基準:
                        // 1. 損耗: 自身または相手が損耗（HP < 70）しており、合流で戦力が実質的に回復する
                        // 2. 効率: HP切り捨て（Overheal）が少ない（total_hp <= 110）
                        // 3. 以下のいずれかの戦術的理由がある場合に都市帰還より合流を優先する:
                        //    (a) 生存: 敵の攻撃到達圏にあり、低HP（<=40）で逃げても撃破されるが、合流すれば耐えられる（>=60）
                        //    (b) ZoC維持: 前線（敵が4マス以内）にあり、退去すると戦線が崩壊する
                        //    (c) 帰還困難: 近く（2マス以内）に自軍の修理可能拠点がない
                        let heals_effectively = is_combat_ineffective || t_health.current < 70;
                        let low_waste = total_hp <= 110;
                        let should_tactical_merge = if heals_effectively && low_waste {
                            let in_threat =
                                enemy_units.iter().any(|(e_pos, _, _, e_hp, _, _, e_move)| {
                                    *e_hp > 0
                                        && map.distance(
                                            current_grid.x,
                                            current_grid.y,
                                            e_pos.x,
                                            e_pos.y,
                                        ) <= *e_move + 1
                                });
                            let survive_by_merging = in_threat && atk_hp <= 40 && total_hp >= 60;
                            let near_threatened_front =
                                enemy_units.iter().any(|(e_pos, _, _, e_hp, _, _, e_move)| {
                                    *e_hp > 0
                                        && map.distance(
                                            current_grid.x,
                                            current_grid.y,
                                            e_pos.x,
                                            e_pos.y,
                                        ) <= *e_move
                                });
                            let is_small_map = crate::ai::strategy_profile::current_profile(
                                world, player_id,
                            )
                                == crate::ai::strategy_profile::V4StrategyProfile::SmallMapExpansion
                                || (map.width * map.height) <= 200;
                            // 小規模マップでは1ターン圏外（2T以上）は戦線崩壊のため合流を優先。
                            // 広いマップなら2ターンまでの戦術的後退は許容し、3T以上なら合流。
                            let max_retreat_turns = if is_small_map { 1 } else { 2 };
                            let no_fast_city =
                                nearest_repair_turns.is_none_or(|turns| turns > max_retreat_turns);
                            survive_by_merging || near_threatened_front || no_fast_city
                        } else {
                            false
                        };

                        let priority = if vacates_gridlocked_production_site {
                            ActionPriority::ProductionSiteRelief
                        } else if should_tactical_merge {
                            ActionPriority::TacticalMerge
                        } else {
                            ActionPriority::Normal
                        };
                        let score = base_tile_score + merge_score;
                        let rank = (priority, score);
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

        // V4限定: 間接ユニットは移動すると同一手番で射撃できない。現在位置から
        // 合法な射撃があるのに、生産口の解放・前進・待機を選ぶと射撃機会そのものを
        // 失うため、定位置射撃を優先する。
        // ただし自軍生産口の上に立ち、工場外で手番を終えられるunitには強制しない。
        // 定位置射撃は次手番の生産枠を物理的に塞ぎ続けるため、射撃と生産口の解放の
        // どちらを取るかは通常の順位付け（閉塞時の生産口回復を含む）に委ねる。
        let fire_in_place_blocks_production =
            starts_on_owned_production_site && can_end_turn_off_production;
        if ai_version == crate::ai::ai_version::AiVersion::V4
            && stats.min_range > 1
            && !fire_in_place_blocks_production
        {
            if let Some((rank, target_entity)) = best_stationary_attack {
                let fires_in_place = matches!(
                    best_unit_choice,
                    Some(AiCommand::Attack { target_pos, .. })
                        if target_pos.x == pos.x && target_pos.y == pos.y
                );
                if !fires_in_place {
                    best_unit_rank = rank;
                    best_unit_choice = Some(AiCommand::Attack {
                        target_pos: GridPosition { x: pos.x, y: pos.y },
                        target_entity,
                    });
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

    strategy_cache.put_action_distance_cache(turn_cache);
    world.insert_resource(strategy_cache);

    best_overall_choice
}

/// V4の通常行動を、Squadを命令主体として選ぶ。
///
/// 先にRoadmap/Reconcilerが確定した唯一所属Squadを優先順に取り出し、そのmemberだけを
/// 既存の戦術採点器へ渡す。従ってこの層は別の島・別NodeへEntityを再配分せず、
/// `AiCommand` がEntity指定であるというエンジン境界だけを最後に解決する。
/// 台帳の作戦所有権を優先し、未登録の通常Squadだけ目標一致で協働する。
pub(super) fn shares_attack_operation(
    world: &World,
    left: Entity,
    right: Entity,
    same_target: bool,
) -> bool {
    world
        .get_resource::<crate::ai::v4::deployment::V4DeploymentRegistry>()
        .and_then(|registry| registry.shares_attack_operation(left, right))
        .unwrap_or(same_target)
}

/// 前衛の現在位置に追随する後方射点を選ぶ。前衛の将来位置を既成事実にしない。
/// 射撃可能なら移動しない。敵の合法な移動後攻撃が届くセルも追随先にしない。
#[allow(clippy::too_many_arguments)]
pub(super) fn coordinated_support_destination(
    world: &World,
    player: PlayerId,
    gun: Entity,
    gun_stats: &UnitStats,
    origin: GridPosition,
    objective: Option<GridPosition>,
    candidates: &[(usize, usize)],
    occupants: &HashMap<(usize, usize), OccupantInfo>,
    manager: &crate::ai::squad::SquadManager,
    snapshot: &AiTacticalSnapshot,
    map: &Map,
    master: &MasterDataRegistry,
    chart: &crate::resources::DamageChart,
) -> Option<GridPosition> {
    let objective = objective?;
    if !snapshot
        .action_targets_at(gun, gun_stats, origin, false)
        .attackable_targets
        .is_empty()
    {
        return None;
    }
    let attack_squads = manager
        .squads
        .iter()
        .filter(|squad| {
            squad.owner_id == Some(player)
                && squad.mission_type == crate::ai::squad::MissionType::Attack
        })
        .collect::<Vec<_>>();
    if !attack_squads
        .iter()
        .any(|squad| squad.members.contains(&gun))
    {
        return None;
    }
    let front_distance = attack_squads
        .iter()
        .flat_map(|squad| {
            squad.members.iter().filter(move |entity| {
                shares_attack_operation(world, gun, **entity, squad.target == Some(objective))
            })
        })
        .filter_map(|entity| {
            let stats = world.get::<UnitStats>(*entity)?;
            let hp = world.get::<Health>(*entity)?;
            let pos = world.get::<GridPosition>(*entity)?;
            // 占領担当を引き抜かず、この攻勢に所属する生存直接部隊だけを見る。
            (stats.min_range <= 1
                && hp.current > 0
                && world
                    .get::<crate::components::Transporting>(*entity)
                    .is_none())
            .then_some(map.distance(pos.x, pos.y, objective.x, objective.y))
        })
        .min()?;
    let target = snapshot
        .units
        .iter()
        .find(|unit| {
            unit.position == objective
                && unit.faction != player
                && !unit.is_transporting
                && world
                    .get::<Health>(unit.entity)
                    .is_some_and(|hp| hp.current > 0)
        })?
        .entity;
    let mut destinations = candidates
        .iter()
        .filter_map(|&(x, y)| {
            let pos = GridPosition { x, y };
            let distance = map.distance(x, y, objective.x, objective.y);
            (pos != origin
                && !occupants.contains_key(&(x, y))
                && distance > front_distance
                && distance < map.distance(origin.x, origin.y, objective.x, objective.y)
                && snapshot
                    .action_targets_at(gun, gun_stats, pos, false)
                    .attackable_targets
                    .contains(&target)
                && !world.iter_entities().any(|entity| {
                    entity.get::<GridPosition>().copied() == Some(pos)
                        && entity.get::<Property>().is_some_and(|property| {
                            master.is_production_facility(property.terrain.as_str())
                        })
                }))
            .then_some(pos)
        })
        .collect::<Vec<_>>();
    // 射程を満たす最後方のセルから検査する。同距離は移動量・座標で安定化する。
    destinations.sort_unstable_by_key(|pos| {
        (
            std::cmp::Reverse(map.distance(pos.x, pos.y, objective.x, objective.y)),
            map.distance(origin.x, origin.y, pos.x, pos.y),
            pos.y,
            pos.x,
        )
    });
    if destinations.is_empty() {
        return None;
    }
    // 砲の元の位置は空く。味方前衛・敵を含む他の占有はそのまま経路計算へ渡す。
    let mut future_occupants = occupants.clone();
    future_occupants.remove(&(origin.x, origin.y));
    let enemies = snapshot
        .units
        .iter()
        .filter_map(|unit| {
            if unit.faction == player || unit.is_transporting {
                return None;
            }
            let stats = world.get::<UnitStats>(unit.entity)?;
            let hp = world.get::<Health>(unit.entity)?.current;
            if hp == 0 {
                return None;
            }
            let fuel = world
                .get::<crate::components::Fuel>(unit.entity)
                .map_or(0, |f| f.current);
            let reachable = calculate_reachable_tiles(
                map,
                &future_occupants,
                (unit.position.x, unit.position.y),
                stats.movement_type,
                stats.max_movement,
                fuel,
                unit.faction,
                stats.unit_type,
                master,
            );
            Some((unit, stats, hp, reachable))
        })
        .collect::<Vec<_>>();
    destinations.into_iter().find(|pos| {
        let defense = map
            .get_terrain(pos.x, pos.y)
            .map_or(0, |terrain| master.get_terrain_defense_bonus(terrain));
        !enemies.iter().any(|(unit, stats, hp, reachable)| {
            reachable
                .iter()
                .chain(std::iter::once(&(unit.position.x, unit.position.y)))
                .any(|&(x, y)| {
                    if future_occupants.contains_key(&(x, y))
                        && (x, y) != (unit.position.x, unit.position.y)
                    {
                        return false;
                    }
                    let distance = map.distance(x, y, pos.x, pos.y);
                    // 実ゲームと同じく、移動後には遠距離射撃できない。
                    if (x, y) != (unit.position.x, unit.position.y) && distance > 1 {
                        return false;
                    }
                    get_expected_damage(
                        stats,
                        *hp,
                        (unit.ammo1, unit.ammo2),
                        gun_stats,
                        defense,
                        distance,
                        master,
                        chart,
                        false,
                    ) > 0
                })
        })
    })
}

pub(crate) fn decide_squad_action_v4(
    world: &mut World,
    player_id: PlayerId,
    skip_entities: &HashSet<Entity>,
) -> Option<(Entity, AiCommand)> {
    let manager = world
        .get_resource::<crate::ai::squad::SquadManager>()
        .cloned()
        .unwrap_or_default();
    let assignments = world
        .get_resource::<crate::ai::operation_assignment::UnitOperationRegistry>()
        .map(|registry| registry.player_assignments(player_id));
    let mut squads = manager
        .squads
        .iter()
        .filter(|squad| {
            squad.owner_id == Some(player_id)
                && squad.mission_type != crate::ai::squad::MissionType::Transport
        })
        .filter_map(|squad| {
            let members = squad
                .members
                .iter()
                .copied()
                .filter(|entity| {
                    assignments.as_ref().is_none_or(|assignments| {
                        assignments
                            .get(entity)
                            .is_none_or(|assignment| assignment.squad_id == Some(squad.id))
                    })
                })
                .collect::<HashSet<_>>();
            (!members.is_empty()).then_some((
                squad_executor_priority(&squad.mission_type),
                squad.id.0,
                members,
                squad.target,
            ))
        })
        .collect::<Vec<_>>();
    // 同一任務のSquad間はIDで安定化する。乱数を持たないため、評価試合のseedを跨いで
    // 意思決定順が揺れない。
    squads.sort_unstable_by_key(|(priority, squad_id, _, _)| (*priority, *squad_id));
    let start = world
        .get_resource::<V4SquadExecutionCursor>()
        .and_then(|cursor| cursor.last_squad.get(&player_id).copied())
        .and_then(|last_squad| {
            squads
                .iter()
                .position(|(_, squad_id, _, _)| *squad_id == last_squad.0)
                .map(|index| (index + 1) % squads.len())
        })
        .unwrap_or(0);
    let mut deferred_wait = None;
    for offset in 0..squads.len() {
        let index = (start + offset) % squads.len();
        let (_, squad_id, members, squad_target) = &squads[index];
        if let Some(action) =
            decide_ai_action_v2_for_entities(world, player_id, skip_entities, Some(members))
        {
            // 同じ攻略目標の前衛を行動済みにする前に、後方からの合法な砲撃を解決する。
            // 火力を仮想的に被害から差し引かず、次のAI stepで実際の敵HPを読み直す。
            // Capture/Defenseの担当は引き抜かず、攻勢Squad同士に限定する。
            if squads[index].0 == squad_executor_priority(&crate::ai::squad::MissionType::Attack)
                && world
                    .get::<UnitStats>(action.0)
                    .is_some_and(|stats| stats.min_range <= 1)
                && matches!(action.1, AiCommand::Attack { .. } | AiCommand::Wait { .. })
                && let Some(objective) = *squad_target
            {
                let support_target = match &action.1 {
                    AiCommand::Attack { target_entity, .. } => Some(*target_entity),
                    // 足止めされている前衛も、目的地の敵が削られる前にWaitを確定しない。
                    _ => world.iter_entities().find_map(|entity| {
                        (entity.get::<GridPosition>().copied() == Some(objective)
                            && entity.get::<Faction>().is_some_and(|f| f.0 != player_id)
                            && entity.get::<Health>().is_some_and(|h| h.current > 0)
                            && entity.get::<UnitStats>().is_some())
                        .then_some(entity.id())
                    }),
                };
                if let Some(support_target) = support_target {
                    for (priority, _, support_members, support_objective) in &squads {
                        if *priority != squads[index].0 {
                            continue;
                        }
                        let guns = support_members
                            .iter()
                            .copied()
                            .filter(|entity| {
                                world
                                    .get::<UnitStats>(*entity)
                                    .is_some_and(|stats| stats.min_range > 1)
                                    && world.get::<HasMoved>(*entity).is_some_and(|moved| !moved.0)
                                    && shares_attack_operation(
                                        world,
                                        action.0,
                                        *entity,
                                        *support_objective == Some(objective),
                                    )
                            })
                            .collect::<HashSet<_>>();
                        if guns.is_empty() {
                            continue;
                        }
                        if let Some((gun, command)) = decide_ai_action_v2_for_entities(
                            world,
                            player_id,
                            skip_entities,
                            Some(&guns),
                        ) {
                            // 通常評価でも選ばれた、その場からの同一目標への射撃だけを実行。
                            // 移動先での架空射撃や、別任務の砲の徴用はしない。
                            if let AiCommand::Attack { target_pos, .. } = &command
                                && world.get::<GridPosition>(gun) == Some(target_pos)
                                && crate::systems::action::get_tactical_action_targets_at(
                                    world,
                                    gun,
                                    *target_pos,
                                    false,
                                )
                                .attackable_targets
                                .contains(&support_target)
                            {
                                // 巡回位置を進めず、砲撃後は前衛から再評価する。
                                return Some((
                                    gun,
                                    AiCommand::Attack {
                                        target_pos: *target_pos,
                                        target_entity: support_target,
                                    },
                                ));
                            }
                        }
                    }
                }
            }
            // 目的方向の出口を、まだ行動できる友軍が塞いでいる場合だけ同位置Waitを
            // 一巡保留する。先に出口側Squadを動かした次のAI stepで同じEntityを再評価し、
            // 生産拠点内の後続を行動済みにしてから出口が空く順序逆転を防ぐ。
            if wait_is_temporarily_blocked_by_unacted_friendly(
                world,
                player_id,
                action.0,
                &action.1,
                *squad_target,
                skip_entities,
            ) {
                deferred_wait.get_or_insert((*squad_id, action));
                continue;
            }
            // 保留済みSquadがある間は巡回位置を進めない。退去行動を実行した次stepを
            // 同じ走査位置から始め、空いた出口を他のSquadに再占有される前に再評価する。
            if deferred_wait.is_none() {
                update_v4_squad_execution_cursor(world, player_id, *squad_id);
            }
            return Some(action);
        }
    }
    // 全Squadを一巡して退去・攻撃などを選べなければ、最初のWaitを確定する。
    // 保留だけを返さず再走査する構造にしないことで、相互閉塞時も無限deferしない。
    deferred_wait.map(|(squad_id, action)| {
        update_v4_squad_execution_cursor(world, player_id, squad_id);
        action
    })
}

/// 小規模の物件レースで、Squadの再編待ちが占領開始を遅らせないようにする。
///
/// Capture可能な未行動unitだけへ既存の戦術探索を限定し、実際に選ばれた命令がCaptureの
/// 場合だけ返す。よって攻撃・退避・目標選択の評価式を複製せず、物件契約を進められない
/// 状況で戦闘部隊の行動順を横取りしない。
pub(super) fn update_v4_squad_execution_cursor(
    world: &mut World,
    player_id: PlayerId,
    squad_id: u32,
) {
    let mut cursor = world
        .remove_resource::<V4SquadExecutionCursor>()
        .unwrap_or_default();
    cursor
        .last_squad
        .insert(player_id, crate::ai::squad::SquadId(squad_id));
    world.insert_resource(cursor);
}

/// 同位置Waitが、目的地へ近づく全ての隣接出口を未行動友軍に塞がれた一時停止か判定する。
///
/// 敵・行動済み友軍・地形による閉塞は同じ手番中に解消する保証がないため保留しない。
/// DAGが一点目標を外したControl中も、古いSquad目標を復活させず通常のWaitを確定する。
pub(super) fn wait_is_temporarily_blocked_by_unacted_friendly(
    world: &World,
    player_id: PlayerId,
    entity: Entity,
    command: &AiCommand,
    squad_target: Option<GridPosition>,
    skip_entities: &HashSet<Entity>,
) -> bool {
    let AiCommand::Wait { target_pos } = command else {
        return false;
    };
    let Some(position) = world.get::<GridPosition>(entity).copied() else {
        return false;
    };
    if *target_pos != position {
        return false;
    }
    let objective = match crate::ai::v4::capital_route_tactical_target(world, player_id, entity) {
        Some(target) => target,
        None => squad_target,
    };
    let Some(objective) = objective.filter(|target| *target != position) else {
        return false;
    };
    let Some(map) = world.get_resource::<Map>() else {
        return false;
    };
    let Some(master_data) = world.get_resource::<MasterDataRegistry>() else {
        return false;
    };
    let Some(stats) = world.get::<UnitStats>(entity) else {
        return false;
    };
    let current_distance = map.distance(position.x, position.y, objective.x, objective.y);
    let mut found_toward_exit = false;

    for (x, y) in map.get_adjacent(position.x, position.y) {
        if map.distance(x, y, objective.x, objective.y) >= current_distance {
            continue;
        }
        let Some(terrain) = map.get_terrain(x, y) else {
            continue;
        };
        if get_valid_movement_cost(master_data, stats.movement_type, terrain).is_none() {
            continue;
        }
        found_toward_exit = true;
        let blocker = world.iter_entities().find_map(|candidate| {
            if candidate.id() == entity
                || candidate.get::<crate::components::Transporting>().is_some()
                || candidate.get::<GridPosition>().copied() != Some(GridPosition { x, y })
            {
                return None;
            }
            candidate
                .get::<Faction>()
                .map(|faction| (candidate.id(), faction.0))
        });
        let Some((blocker, faction)) = blocker else {
            return false;
        };
        let blocker_unacted = faction == player_id
            && !skip_entities.contains(&blocker)
            && world.get::<HasMoved>(blocker).is_some_and(|moved| !moved.0)
            && world
                .get::<ActionCompleted>(blocker)
                .is_some_and(|completed| !completed.0);
        if !blocker_unacted {
            return false;
        }
    }

    found_toward_exit
}

/// 敵前のCapture Squadを単独で先走らせないよう、まずSuppress/Attackを扱い、残りは
/// Capture・Defense・Reserveの順にする。
///
/// これは戦術の点数を置換するものではなく、複数Squadのうちどの命令を次に展開するか
/// を決めるRoadmap投影順である。
pub(super) fn squad_executor_priority(mission: &crate::ai::squad::MissionType) -> u8 {
    match mission {
        crate::ai::squad::MissionType::Attack => 0,
        crate::ai::squad::MissionType::Capture => 1,
        crate::ai::squad::MissionType::Defense => 2,
        crate::ai::squad::MissionType::Reserve => 3,
        crate::ai::squad::MissionType::Transport => u8::MAX,
    }
}
