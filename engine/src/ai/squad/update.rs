//! Squadの手番更新。処理順・一覧からの削除順・降車handoff順を保つ。
use super::*;

struct UnitStatusScan {
    existing: HashSet<Entity>,
    needs_fallback: Vec<Entity>,
    recovered: Vec<Entity>,
}
struct DeliveredCargo {
    source: SquadId,
    entity: Entity,
    island: Option<crate::ai::islands::IslandId>,
    target: Option<GridPosition>,
}
struct DeferredPickup {
    player: PlayerId,
    cargo: Vec<Entity>,
    island: Option<crate::ai::islands::IslandId>,
    target: Option<GridPosition>,
    allow_partial: bool,
}
#[derive(Default)]
struct TransportAdvance {
    delivered: Vec<DeliveredCargo>,
    deferred: Vec<DeferredPickup>,
}

fn scan_unit_status(world: &mut World, perspective_player: PlayerId) -> UnitStatusScan {
    // 存在しなくなったエンティティの削除
    let mut existing_entities = HashSet::default();
    let mut units_needing_fallback = Vec::new();
    let mut units_recovered = Vec::new();

    let mut faction_query = world.query::<(Entity, &Faction)>();
    for (entity, _) in faction_query.iter(world) {
        existing_entities.insert(entity);
    }
    let mut status_query = world.query::<(Entity, &Faction, &Health, Option<&Ammo>)>();
    for (entity, faction, health, ammo_opt) in status_query.iter(world) {
        if faction.0 != perspective_player {
            continue;
        }
        // SoloFallback の判定 (HP < 60 または 弾薬切れ)
        let mut no_ammo = false;
        if let Some(ammo) = ammo_opt {
            no_ammo =
                (ammo.max_ammo1 > 0 && ammo.ammo1 == 0) && (ammo.max_ammo2 > 0 && ammo.ammo2 == 0);
        }

        if health.current < 60 || no_ammo {
            units_needing_fallback.push(entity);
        } else if health.current >= 70 && !no_ammo {
            // 回復条件を満たした
            units_recovered.push(entity);
        }
    }

    UnitStatusScan {
        existing: existing_entities,
        needs_fallback: units_needing_fallback,
        recovered: units_recovered,
    }
}

fn release_vanished_entities(
    world: &mut World,
    manager: &mut SquadManager,
    perspective_player: PlayerId,
    existing_entities: &HashSet<Entity>,
) {
    // 変更できるSquadのIDを先に集め、消滅Entityの提案を全役割から解放する。
    let mutable_ids = manager
        .proposal_view()
        .squads
        .iter()
        .filter(|squad| squad_is_mutable_by_player(world, squad, perspective_player))
        .map(|squad| squad.id)
        .collect::<Vec<_>>();
    {
        let mut proposals = OperationLedger::proposals(manager);
        for id in &mutable_ids {
            proposals.retain_live(*id, existing_entities);
        }
    }
}

fn update_solo_fallbacks(
    world: &mut World,
    manager: &mut SquadManager,
    perspective_player: PlayerId,
    status: &UnitStatusScan,
) {
    // SoloFallback の更新
    manager
        .solo_fallbacks
        .retain(|e| status.existing.contains(e));

    for &e in &status.needs_fallback {
        manager.solo_fallbacks.insert(e);
        // foreign/mixed Squadの保護判定はcleanup後の現状で行う。
        let ids = manager
            .proposal_view()
            .squads
            .iter()
            .filter(|squad| squad_is_mutable_by_player(world, squad, perspective_player))
            .map(|squad| squad.id)
            .collect::<Vec<_>>();
        let mut proposals = OperationLedger::proposals(manager);
        for id in ids {
            proposals.leave(id, e);
        }
    }

    for &e in &status.recovered {
        manager.solo_fallbacks.remove(&e);
    }
}

fn return_escorts_to_transport(
    world: &mut World,
    manager: &mut SquadManager,
    perspective_player: PlayerId,
) {
    // 輸送ヘリの一時護衛は、対象軽歩兵が消えた時点で輸送任務へ戻す。
    // Attack Squadを完了させて遊兵化せず、次の占領波へ再利用できる状態まで帰還させる。
    for mut squad in manager.edit_each() {
        if !squad.plan.return_after_combat
            || !squad_is_mutable_by_player(world, &squad, perspective_player)
        {
            continue;
        }
        let next_target = squad
            .transport_entity
            .zip(squad.plan.target_island)
            .and_then(|(transport, island)| {
                light_infantry_target_for_transport(world, transport, perspective_player, island)
            });
        if let Some(target) = next_target {
            squad.plan_mut().target = Some(target);
        } else {
            squad.plan_mut().mission_type = MissionType::Transport;
            squad.plan_mut().target = None;
            squad.plan_mut().phase = MissionPhase::Transport(TransportPhase::Return);
            squad.plan_mut().return_after_combat = false;
        }
    }
}

fn disband_ownerless_empty_forming(manager: &mut SquadManager) {
    // 所有者を推定できない空Formingは購入作戦のidentityとして扱わず、任意playerへ継承しない。
    OperationLedger::squad_list(manager).retain(|squad| {
        squad.plan.owner_id.is_some()
            || squad.plan.mission_type != MissionType::Transport
            || squad.plan.phase != MissionPhase::Forming
            || squad.transport_entity.is_some()
            || !squad.members.is_empty()
            || !squad.cargo_entities.is_empty()
            || !squad.delivered_cargo.is_empty()
            || squad.plan.target_island.is_none()
            || squad.plan.target.is_none()
    });
}

fn advance_transport_squad(
    world: &mut World,
    squad: &mut SquadEdit<'_>,
    perspective_player: PlayerId,
    temporary_escort_exists: bool,
    changes: &mut TransportAdvance,
) -> bool {
    let preserve_purchase_placeholder = is_purchase_campaign_placeholder(squad, perspective_player);
    if let Some(deferred) = detach_deferred_pickup_cargo(world, squad) {
        changes.deferred.push(DeferredPickup {
            player: squad.plan.owner_id.unwrap_or(perspective_player),
            cargo: deferred,
            island: squad.plan.target_island,
            target: squad.plan.target,
            allow_partial: squad.plan.allow_partial_departure,
        });
    }
    let should_remove = update_transport_squad_phase(world, squad);
    if matches!(
        squad.plan.phase,
        MissionPhase::Transport(TransportPhase::Return)
    ) && !temporary_escort_exists
        && let (Some(transport), Some(target_island)) =
            (*squad.transport_entity, squad.plan.target_island)
        && let Some(enemy_target) =
            light_infantry_target_for_transport(world, transport, perspective_player, target_island)
    {
        // 最後の降車後も歩兵脅威が残るなら、武装輸送ヘリをただちに局地護衛へ
        // 転用する。対空・重装甲相手にはこの転用をせず、通常どおり帰還させる。
        squad.plan_mut().mission_type = MissionType::Attack;
        squad.plan_mut().target = Some(enemy_target);
        squad.plan_mut().pickup_position = None;
        squad.plan_mut().drop_position = None;
        squad.plan_mut().phase = MissionPhase::MovingToTarget;
        squad.plan_mut().return_after_combat = true;
    }
    let preserve_targetless_delivery = squad.transport_entity.is_none()
        && squad.plan.target_island.is_none()
        && squad.plan.target.is_none()
        && !squad.delivered_cargo.is_empty();
    // 降車済みcargoのbindingを輸送Squadから外し、引渡し候補へ移す。
    let delivered = squad.delivered_cargo.to_vec();
    OperationLedger::roster(squad).clear_delivered();
    changes
        .delivered
        .extend(delivered.into_iter().map(|cargo| DeliveredCargo {
            source: squad.id,
            entity: cargo,
            island: squad.plan.target_island,
            target: squad.plan.target,
        }));
    should_remove && !preserve_targetless_delivery && !preserve_purchase_placeholder
}

fn advance_transport_squads(
    world: &mut World,
    manager: &mut SquadManager,
    perspective_player: PlayerId,
) -> TransportAdvance {
    // 輸送部隊のフェーズ更新と完了判定
    let mut changes = TransportAdvance::default();
    let mut i = 0;
    while i < manager.proposal_view().squads.len() {
        if manager.proposal_view().squads[i].plan.mission_type == MissionType::Transport
            && squad_is_mutable_by_player(
                world,
                &manager.proposal_view().squads[i],
                perspective_player,
            )
        {
            let id = manager.proposal_view().squads[i].id;
            let temporary_escort_exists =
                manager.proposal_view().squads[i]
                    .plan
                    .target_island
                    .is_some_and(|target_island| {
                        manager.proposal_view().squads.iter().enumerate().any(
                            |(other_index, other)| {
                                other_index != i
                                    && other.plan.owner_id == Some(perspective_player)
                                    && other.plan.target_island == Some(target_island)
                                    && other.plan.return_after_combat
                                    && other.plan.mission_type == MissionType::Attack
                            },
                        )
                    });
            let remove = OperationLedger::proposals(manager)
                .edit(id, |squad| {
                    advance_transport_squad(
                        world,
                        squad,
                        perspective_player,
                        temporary_escort_exists,
                        &mut changes,
                    )
                })
                .expect("更新対象の輸送提案");
            if remove {
                OperationLedger::squad_list(manager).remove(i);
            } else {
                i += 1;
            }
        } else {
            i += 1;
        }
    }

    changes
}

fn hold_deferred_pickups(manager: &mut SquadManager, deferred_pickups: Vec<DeferredPickup>) {
    // 部分搭載便から切り離したcargoは遊兵化させず、同じ作戦の後続便として保持する。
    // 後段のcampaign reconcileが利用可能な別輸送役へ再partitionする。
    {
        let mut proposals = OperationLedger::proposals(manager);
        for DeferredPickup {
            player: owner_id,
            cargo,
            island: target_island,
            target,
            allow_partial: allow_partial_departure,
        } in deferred_pickups
        {
            let mut deferred = proposals.create_owned(
                MissionType::Transport,
                owner_id,
                BTreeSet::new(),
                None,
                cargo,
            );
            deferred.plan_mut().target_island = target_island;
            deferred.plan_mut().target = target;
            deferred.plan_mut().allow_partial_departure = allow_partial_departure;
            deferred.plan_mut().phase = MissionPhase::Forming;
        }
    }
}

fn hand_off_delivered_cargo(
    world: &mut World,
    manager: &mut SquadManager,
    perspective_player: PlayerId,
    delivered_units: Vec<DeliveredCargo>,
) {
    // 降車したユニットを輸送部隊から解放し、通常の占領・攻撃部隊へ引き渡す。
    for DeliveredCargo {
        source: source_id,
        entity: cargo,
        island: target_island,
        target: preferred_target,
    } in delivered_units
    {
        let handed_off = handoff_delivered_cargo(
            world,
            manager,
            perspective_player,
            cargo,
            target_island,
            preferred_target,
        );
        if !handed_off && target_island.is_none() {
            let source_index = manager
                .proposal_view()
                .squads
                .iter()
                .position(|squad| squad.id == source_id);
            let hold_index = source_index.or_else(|| {
                manager.proposal_view().squads.iter().position(|squad| {
                    squad.plan.mission_type == MissionType::Transport
                        && squad.transport_entity.is_none()
                        && squad.members.is_empty()
                        && squad.plan.target_island.is_none()
                        && squad.plan.target.is_none()
                        && squad.plan.phase == MissionPhase::Forming
                        && squad_is_mutable_by_player(world, squad, perspective_player)
                })
            });
            let hold_id = if let Some(index) = hold_index {
                manager.proposal_view().squads[index].id
            } else {
                manager
                    .create_owned_squad(MissionType::Transport, perspective_player)
                    .id
            };
            let mut proposals = OperationLedger::proposals(manager);
            if proposals
                .squad_mut(hold_id)
                .unwrap()
                .transport_entity
                .is_none()
            {
                // 輸送役が無いholdだけを空にする。進行中のsource輸送は保持する。
                proposals.clear_members(hold_id);
                proposals.clear_cargo(hold_id);
                let mut hold = proposals.squad_mut(hold_id).unwrap();
                hold.plan_mut().owner_id = Some(perspective_player);
                hold.plan_mut().mission_type = MissionType::Transport;
                hold.plan_mut().target_island = None;
                hold.plan_mut().target = None;
                hold.plan_mut().pickup_position = None;
                hold.plan_mut().drop_position = None;
                hold.plan_mut().phase = MissionPhase::Forming;
            }
            proposals.deliver(hold_id, cargo);
        }
    }
}

fn disband_completed_captures(
    world: &mut World,
    manager: &mut SquadManager,
    perspective_player: PlayerId,
) -> HashMap<GridPosition, Option<PlayerId>> {
    // 占領完了した Capture 部隊の解散
    let mut properties_ownership = HashMap::default();
    {
        let mut q_props = world.query::<(&GridPosition, &Property)>();
        for (pos, prop) in q_props.iter(world) {
            properties_ownership.insert(*pos, prop.owner_id);
        }
    }

    OperationLedger::squad_list(manager).retain(|squad| {
        if squad_is_mutable_by_player(world, squad, perspective_player)
            && squad.plan.mission_type == MissionType::Capture
        {
            if let Some(target_pos) = squad.plan.target {
                if properties_ownership.get(&target_pos) == Some(&Some(perspective_player)) {
                    // 自軍の所有になったので解散
                    return false;
                }
            }
        }
        true
    });

    properties_ownership
}

fn release_cleared_island_bindings(
    world: &mut World,
    manager: &mut SquadManager,
    perspective_player: PlayerId,
    properties_ownership: &HashMap<GridPosition, Option<PlayerId>>,
) {
    // 侵攻島の敵・未占領拠点が無くなった通常部隊は島拘束を解除する。
    if let Some(island_map) = world
        .get_resource::<crate::ai::islands::IslandMap>()
        .cloned()
    {
        let mut active_islands = HashSet::default();
        for (position, owner) in properties_ownership {
            if *owner != Some(perspective_player)
                && let Some(island) = island_map.get_island_at(position)
            {
                active_islands.insert(island.id);
            }
        }
        let mut enemy_query = world.query::<(&GridPosition, &Faction)>();
        for (position, faction) in enemy_query.iter(world) {
            if faction.0 != perspective_player
                && let Some(island) = island_map.get_island_at(position)
            {
                active_islands.insert(island.id);
            }
        }
        for mut squad in manager.edit_each() {
            if squad_is_mutable_by_player(world, &squad, perspective_player)
                && matches!(
                    squad.plan.mission_type,
                    MissionType::Attack | MissionType::Defense
                )
                && squad
                    .plan
                    .target_island
                    .is_some_and(|island| !active_islands.contains(&island))
            {
                squad.plan_mut().target_island = None;
            }
        }
    }
}

fn disband_empty_squads(manager: &mut SquadManager) {
    // メンバーが0になった部隊の解散
    OperationLedger::squad_list(manager).retain(|s| {
        !s.members.is_empty()
            || s.plan.phase == MissionPhase::Completed
            || s.plan.phase == MissionPhase::Forming
                && (s.transport_entity.is_some()
                    || !s.cargo_entities.is_empty()
                    || !s.delivered_cargo.is_empty()
                    || s.plan.target_island.is_some()
                    || s.plan.target.is_some())
    });
}

/// 一覧は輸送phaseの処理中に削除されるため、advance内の添字whileを維持する。
pub(super) fn update(world: &mut World, perspective_player: PlayerId) {
    let mut manager = world.remove_resource::<SquadManager>().unwrap_or_default();
    adopt_legacy_squad_owners(world, &mut manager);
    let status = scan_unit_status(world, perspective_player);
    release_vanished_entities(world, &mut manager, perspective_player, &status.existing);
    update_solo_fallbacks(world, &mut manager, perspective_player, &status);
    return_escorts_to_transport(world, &mut manager, perspective_player);
    disband_ownerless_empty_forming(&mut manager);
    let changes = advance_transport_squads(world, &mut manager, perspective_player);
    hold_deferred_pickups(&mut manager, changes.deferred);
    hand_off_delivered_cargo(world, &mut manager, perspective_player, changes.delivered);
    let ownership = disband_completed_captures(world, &mut manager, perspective_player);
    release_cleared_island_bindings(world, &mut manager, perspective_player, &ownership);
    disband_empty_squads(&mut manager);
    world.insert_resource(manager);
}
