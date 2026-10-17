//! 輸送命令の段階別解決。所属の更新はledger、実行計画の更新はSquadEditを使う。
use super::*;
use crate::ai::command::AiCommand;
use std::ops::ControlFlow;

type Action = Option<(Entity, AiCommand)>;
struct TransportStepContext {
    entity: Entity,
    position: GridPosition,
    stats: UnitStats,
    fuel: u32,
    player: PlayerId,
    phase: TransportPhase,
    loaded: Vec<Entity>,
}
struct TransportMoveSpace {
    occupants: HashMap<(usize, usize), crate::systems::movement::OccupantInfo>,
    reachable: BTreeSet<(usize, usize)>,
}

/// 輸送役の物理状態と所属cargoを取得する。命令の発行やphase変更は行わない。
fn transport_step_context(world: &World, squad: &Squad) -> Option<TransportStepContext> {
    let transport_entity = (*squad.transport_entity)?;

    let (t_pos, t_stats, t_fuel, t_faction) = {
        let t_pos = world.get::<GridPosition>(transport_entity).cloned()?;
        let t_stats = world.get::<UnitStats>(transport_entity).cloned()?;
        let t_fuel = world
            .get::<crate::components::Fuel>(transport_entity)
            .map(|f| f.current)?;
        let t_faction = world.get::<Faction>(transport_entity).cloned()?;
        (t_pos, t_stats, t_fuel, t_faction.0)
    };

    let phase = match squad.plan.phase {
        MissionPhase::Transport(p) => p,
        _ => return None,
    };
    let loaded_cargo = world
        .get::<crate::components::CargoCapacity>(transport_entity)
        .map(|capacity| capacity.loaded.clone())
        .unwrap_or_default();
    Some(TransportStepContext {
        entity: transport_entity,
        position: t_pos,
        stats: t_stats,
        fuel: t_fuel,
        player: t_faction,
        phase,
        loaded: loaded_cargo,
    })
}

/// 発進許可待ちと即時Loadを、移動候補の探索より先に解決する。
fn try_early_transport_action(
    world: &mut World,
    squad: &mut SquadEdit<'_>,
    skip_entities: &HashSet<Entity>,
    ctx: &mut TransportStepContext,
) -> ControlFlow<Action, bool> {
    let transport_entity = ctx.entity;
    let t_pos = ctx.position;
    let mut phase = ctx.phase;
    let loaded_cargo = &ctx.loaded;
    let all_assigned_cargo_loaded = !squad.cargo_entities.is_empty()
        && squad
            .cargo_entities
            .iter()
            .all(|cargo| loaded_cargo.contains(cargo));
    if phase == TransportPhase::Pickup && all_assigned_cargo_loaded {
        if squad.plan.departure_authorized {
            // plan_squads後に兵站gateが開いた手番も、次の手番を待たずTransitへ進める。
            squad.plan_mut().phase = MissionPhase::Transport(TransportPhase::Transit);
            ctx.phase = TransportPhase::Transit;
            phase = ctx.phase;
        } else {
            // 搭載済みの形成波は「命令が出ないForming」ではなく、発進許可待ちを
            // 明示する。生産拠点を塞がず、任務監査でも意図的な予備待機と判別できる。
            let transport_available = !skip_entities.contains(&transport_entity)
                && world
                    .get::<crate::components::HasMoved>(transport_entity)
                    .is_some_and(|moved| !moved.0)
                && world
                    .get::<crate::components::ActionCompleted>(transport_entity)
                    .is_some_and(|action| !action.0);
            return ControlFlow::Break(transport_available.then_some((
                transport_entity,
                crate::ai::command::AiCommand::Wait { target_pos: t_pos },
            )));
        }
    }
    // Transit / Drop / Return で命令を出す主体は輸送役だけである。
    // 行動済み輸送役について盤面全Entityの占有表と到達範囲を組み直しても、呼び出し側で
    // 同じEntityがcooldown除外されるだけなので、重い経路探索へ入る前に終了する。
    // Drop中にまだcargoが残る場合はActionCompletedがfalseのため、連続降車を妨げない。
    if phase != TransportPhase::Pickup
        && (skip_entities.contains(&transport_entity)
            || world
                .get::<crate::components::ActionCompleted>(transport_entity)
                .is_some_and(|action| action.0))
    {
        return ControlFlow::Break(None);
    }
    let must_vacate_production_site = must_vacate_production_site(world, squad, skip_entities, ctx);

    // Pickupではヘリを先に動かすと、今いるヘリへ到達可能なcargoとの合流を
    // 自分から崩してしまうため即時Loadを優先する。ただし生産施設上だけは、先に
    // 指定済みの非生産Pickupへ退避する。同じ手番の後続呼び出しでcargo側が移動Load
    // できるため、搭載ターンを遅らせずに次ターンの生産枠を空けられる。
    if phase == TransportPhase::Pickup
        && !must_vacate_production_site
        && let Some(cargo) = squad.cargo_entities.iter().copied().find(|cargo| {
            !loaded_cargo.contains(cargo)
                && !skip_entities.contains(cargo)
                && cargo_can_board_transport_this_turn(world, *cargo, transport_entity)
        })
    {
        return ControlFlow::Break(Some((
            cargo,
            crate::ai::command::AiCommand::Load {
                transport_entity,
                target_pos: t_pos,
            },
        )));
    }

    ControlFlow::Continue(must_vacate_production_site)
}

/// 現在phaseで操作するcargoを、登録順と行動可能性で選ぶ。
fn select_target_cargo(
    world: &World,
    squad: &Squad,
    skip_entities: &HashSet<Entity>,
    ctx: &TransportStepContext,
) -> Option<Entity> {
    let phase = ctx.phase;
    let loaded_cargo = &ctx.loaded;
    match phase {
        TransportPhase::Pickup => {
            let unloaded: Vec<_> = squad
                .cargo_entities
                .iter()
                .copied()
                .filter(|cargo| {
                    !loaded_cargo.contains(cargo)
                        && world
                            .get::<crate::components::Transporting>(*cargo)
                            .is_none()
                })
                .collect();
            // 先頭cargoが行動済みでも、後続cargoは同じ手番に合流点へ前進できる。
            // 全員行動済みの場合だけ先頭をfallbackにし、輸送役側の移動判定を継続する。
            unloaded
                .iter()
                .copied()
                .find(|cargo| {
                    !skip_entities.contains(cargo)
                        && world
                            .get::<crate::components::HasMoved>(*cargo)
                            .is_some_and(|moved| !moved.0)
                        && world
                            .get::<crate::components::ActionCompleted>(*cargo)
                            .is_some_and(|action| !action.0)
                })
                .or_else(|| unloaded.first().copied())
        }
        TransportPhase::Transit | TransportPhase::Drop => squad
            .cargo_entities
            .iter()
            .copied()
            .find(|cargo| loaded_cargo.contains(cargo)),
        TransportPhase::Return => None,
    }
}

/// 未搭乗ユニットの占有表を作り、輸送役の到達範囲を計算する。
fn transport_move_space(world: &mut World, ctx: &TransportStepContext) -> TransportMoveSpace {
    let t_pos = ctx.position;
    let t_stats = &ctx.stats;
    let t_fuel = ctx.fuel;
    let t_faction = ctx.player;
    let mut unit_positions = HashMap::default();
    let mut query = world.query::<(
        Entity,
        &GridPosition,
        &Faction,
        &UnitStats,
        Option<&crate::components::CargoCapacity>,
        Option<&crate::components::Transporting>,
    )>();
    for (_e, pos, faction, stats, cargo_opt, transporting_opt) in query.iter(world) {
        if transporting_opt.is_some() {
            continue;
        }
        let free_slots = cargo_opt
            .map(|c| c.max.saturating_sub(c.loaded.len() as u32))
            .unwrap_or(0);
        unit_positions.insert(
            (pos.x, pos.y),
            crate::systems::movement::OccupantInfo {
                player_id: faction.0,
                is_transport: stats.max_cargo > 0,
                unit_type: stats.unit_type,
                loadable_types: stats.loadable_unit_types.clone(),
                free_slots,
            },
        );
    }

    let reachable = {
        let map = world.resource::<Map>();
        let registry = world.resource::<MasterDataRegistry>();
        calculate_reachable_tiles(
            map,
            &unit_positions,
            (t_pos.x, t_pos.y),
            t_stats.movement_type,
            t_stats.max_movement,
            t_fuel,
            t_faction,
            t_stats.unit_type,
            registry,
        )
    };

    TransportMoveSpace {
        occupants: unit_positions,
        reachable,
    }
}

/// 同位置の積載、輸送役の接近、cargoの接近の順に試す。
fn step_pickup(
    world: &mut World,
    squad: &mut SquadEdit<'_>,
    skip_entities: &HashSet<Entity>,
    ctx: &TransportStepContext,
    space: &TransportMoveSpace,
    cargo_entity: Option<Entity>,
    must_vacate_production_site: bool,
) -> Action {
    let transport_entity = ctx.entity;
    let t_pos = ctx.position;
    let t_stats = &ctx.stats;
    let t_faction = ctx.player;
    let unit_positions = &space.occupants;
    let reachable = &space.reachable;
    let cargo_entity = cargo_entity?;
    let cargo_pos = world.get::<GridPosition>(cargo_entity).cloned()?;
    let pickup_position = squad.plan.pickup_position.unwrap_or(t_pos);
    let dist =
        (t_pos.x as i32 - cargo_pos.x as i32).abs() + (t_pos.y as i32 - cargo_pos.y as i32).abs();

    let transport_moved = world
        .get::<crate::components::HasMoved>(transport_entity)
        .map_or(true, |moved| moved.0);
    let transport_action_completed = world
        .get::<crate::components::ActionCompleted>(transport_entity)
        .map_or(true, |action| action.0);
    let cargo_action_completed = world
        .get::<crate::components::ActionCompleted>(cargo_entity)
        .map_or(true, |action| action.0);
    // 積載システムは同じ輸送役への複数cargo積載を許可している。輸送役は
    // 1体目の積載で行動済みになるが、同じマスの未行動cargoまで翌ターンへ
    // 送る必要はないため、積載可否はcargo自身の行動状態だけで判定する。
    if dist == 0 && !cargo_action_completed && !must_vacate_production_site {
        return Some((
            cargo_entity,
            crate::ai::command::AiCommand::Load {
                transport_entity,
                target_pos: t_pos,
            },
        ));
    }

    // 1. 輸送機がまだ行動していないなら、輸送機を歩兵へ近づける

    if !transport_moved && !transport_action_completed && !skip_entities.contains(&transport_entity)
    {
        let mut best_tile = t_pos;
        let mut min_score = None;

        let mut cache = crate::ai::turn_distance::TurnDistanceCache::default();
        let map = world.resource::<Map>();
        let registry = world.resource::<MasterDataRegistry>();

        for target_tile in reachable {
            let t_dist = crate::ai::turn_distance::calculate_turn_distance(
                map,
                registry,
                unit_positions,
                (target_tile.0, target_tile.1),
                (pickup_position.x, pickup_position.y),
                t_stats.movement_type,
                t_stats.max_movement,
                0, // 指定した合流地点を目指す
                t_faction,
                &mut cache,
            );

            let dx = target_tile.0 as i32 - pickup_position.x as i32;
            let dy = target_tile.1 as i32 - pickup_position.y as i32;
            let m_dist = dx.abs() + dy.abs();
            let score = (t_dist, m_dist, target_tile.0, target_tile.1);

            if min_score.map_or(true, |m| score < m) {
                min_score = Some(score);
                best_tile = GridPosition {
                    x: target_tile.0,
                    y: target_tile.1,
                };
            }
        }
        return Some((
            transport_entity,
            crate::ai::command::AiCommand::Wait {
                target_pos: best_tile,
            },
        ));
    }

    approach_pickup_cargo(
        world,
        skip_entities,
        ctx,
        space,
        PickupCargo {
            entity: cargo_entity,
            position: cargo_pos,
            pickup_position,
            action_completed: cargo_action_completed,
        },
    )
}

/// 上陸可能ならDropへ遷移し、それ以外は対象島へ前進する。
fn step_transit(
    world: &mut World,
    squad: &mut SquadEdit<'_>,
    skip_entities: &HashSet<Entity>,
    ctx: &TransportStepContext,
    space: &TransportMoveSpace,
    cargo_entity: Option<Entity>,
) -> Action {
    let transport_entity = ctx.entity;
    let t_pos = ctx.position;
    let t_stats = &ctx.stats;
    let t_faction = ctx.player;
    let unit_positions = &space.occupants;
    let reachable = &space.reachable;
    let cargo_entity = cargo_entity?;
    if skip_entities.contains(&transport_entity)
        || world
            .get::<crate::components::ActionCompleted>(transport_entity)
            .is_some_and(|action| action.0)
    {
        return None;
    }
    if let Some(target_island_id) = squad.plan.target_island {
        let (island_tiles, target_pos) = {
            if let Some(island_map) = world.get_resource::<crate::ai::islands::IslandMap>() {
                if let Some(island) = island_map.islands.iter().find(|i| i.id == target_island_id) {
                    let map = world.resource::<Map>();
                    let registry = world.resource::<MasterDataRegistry>();
                    let target_pos = get_target_position_for_island(
                        map,
                        registry,
                        island,
                        t_pos,
                        t_stats.movement_type,
                    );
                    (Some(island.tiles.clone()), target_pos)
                } else {
                    (None, None)
                }
            } else {
                (None, None)
            }
        };

        if let (Some(_), Some(target_pos)) = (island_tiles, target_pos) {
            if let Some((transport_target, drop_target)) = select_landing_candidate(
                world,
                transport_entity,
                cargo_entity,
                t_pos,
                reachable,
                squad.plan.target_island,
                squad.plan.target,
            ) {
                squad.plan_mut().drop_position = Some(drop_target);
                squad.plan_mut().phase = MissionPhase::Transport(TransportPhase::Drop);
                return Some((
                    transport_entity,
                    crate::ai::command::AiCommand::Drop {
                        transport_target_pos: transport_target,
                        cargo_drop_pos: drop_target,
                        cargo_entity,
                    },
                ));
            }

            let mut best_tile = t_pos;
            let mut min_score = None;

            let mut cache = crate::ai::turn_distance::TurnDistanceCache::default();
            let map = world.resource::<Map>();
            let registry = world.resource::<MasterDataRegistry>();

            for target_tile in reachable {
                let t_dist = crate::ai::turn_distance::calculate_turn_distance(
                    map,
                    registry,
                    unit_positions,
                    (target_tile.0, target_tile.1),
                    (target_pos.x, target_pos.y),
                    t_stats.movement_type,
                    t_stats.max_movement,
                    1, // 目標に隣接するマスを目指す
                    t_faction,
                    &mut cache,
                );

                let dx = target_tile.0 as i32 - target_pos.x as i32;
                let dy = target_tile.1 as i32 - target_pos.y as i32;
                let m_dist = dx.abs() + dy.abs();
                let score = (t_dist, m_dist, target_tile.0, target_tile.1);

                if min_score.map_or(true, |m| score < m) {
                    min_score = Some(score);
                    best_tile = GridPosition {
                        x: target_tile.0,
                        y: target_tile.1,
                    };
                }
            }

            return Some((
                transport_entity,
                crate::ai::command::AiCommand::Wait {
                    target_pos: best_tile,
                },
            ));
        }
    }

    None
}

/// 降車候補を優先し、見つからない場合は島への接近か現位置待機を選ぶ。
fn step_drop(
    world: &mut World,
    squad: &mut SquadEdit<'_>,
    skip_entities: &HashSet<Entity>,
    ctx: &TransportStepContext,
    space: &TransportMoveSpace,
    cargo_entity: Option<Entity>,
) -> Action {
    let transport_entity = ctx.entity;
    let t_pos = ctx.position;
    let t_stats = &ctx.stats;
    let t_faction = ctx.player;
    let unit_positions = &space.occupants;
    let reachable = &space.reachable;
    let cargo_entity = cargo_entity?;
    if skip_entities.contains(&transport_entity)
        || world
            .get::<crate::components::ActionCompleted>(transport_entity)
            .is_some_and(|action| action.0)
    {
        return None;
    }
    if let Some((transport_target, drop_target)) = select_landing_candidate(
        world,
        transport_entity,
        cargo_entity,
        t_pos,
        reachable,
        squad.plan.target_island,
        squad.plan.target,
    ) {
        squad.plan_mut().drop_position = Some(drop_target);
        return Some((
            transport_entity,
            crate::ai::command::AiCommand::Drop {
                transport_target_pos: transport_target,
                cargo_drop_pos: drop_target,
                cargo_entity,
            },
        ));
    }

    if let Some(target_island_id) = squad.plan.target_island {
        if let Some(island_map) = world.get_resource::<crate::ai::islands::IslandMap>() {
            if let Some(island) = island_map.islands.iter().find(|i| i.id == target_island_id) {
                let map = world.resource::<Map>();
                let registry = world.resource::<MasterDataRegistry>();
                if let Some(target_pos) = get_target_position_for_island(
                    map,
                    registry,
                    island,
                    t_pos,
                    t_stats.movement_type,
                ) {
                    let mut best_tile = t_pos;
                    let mut min_score = None;
                    let mut cache = crate::ai::turn_distance::TurnDistanceCache::default();
                    let map = world.resource::<Map>();
                    let registry = world.resource::<MasterDataRegistry>();

                    for target_tile in reachable {
                        let t_dist = crate::ai::turn_distance::calculate_turn_distance(
                            map,
                            registry,
                            unit_positions,
                            (target_tile.0, target_tile.1),
                            (target_pos.x, target_pos.y),
                            t_stats.movement_type,
                            t_stats.max_movement,
                            1, // 目標に隣接するマスを目指す
                            t_faction,
                            &mut cache,
                        );
                        let m_dist = (target_tile.0 as i32 - target_pos.x as i32).abs()
                            + (target_tile.1 as i32 - target_pos.y as i32).abs();
                        let score = (t_dist, m_dist, target_tile.0, target_tile.1);

                        if min_score.map_or(true, |m| score < m) {
                            min_score = Some(score);
                            best_tile = GridPosition {
                                x: target_tile.0,
                                y: target_tile.1,
                            };
                        }
                    }
                    return Some((
                        transport_entity,
                        crate::ai::command::AiCommand::Wait {
                            target_pos: best_tile,
                        },
                    ));
                }
            }
        }
    }
    Some((
        transport_entity,
        crate::ai::command::AiCommand::Wait { target_pos: t_pos },
    ))
}

/// 到達可能な補給施設を距離順に選び、既存の浮動小数点順位で接近する。
fn step_return(
    world: &mut World,
    ctx: &TransportStepContext,
    space: &TransportMoveSpace,
) -> Action {
    let transport_entity = ctx.entity;
    let t_pos = ctx.position;
    let t_stats = &ctx.stats;
    let t_faction = ctx.player;
    let unit_positions = &space.occupants;
    let reachable = &space.reachable;
    let map = world.resource::<Map>().clone();
    let registry = world.resource::<MasterDataRegistry>().clone();
    let mut return_targets = Vec::new();
    let mut query = world.query::<(&GridPosition, &Property)>();
    for (position, property) in query.iter(world) {
        if property.owner_id != Some(t_faction)
            || crate::systems::movement::get_valid_movement_cost(
                &registry,
                t_stats.movement_type,
                property.terrain,
            )
            .is_none()
            || t_stats.movement_type == crate::resources::MovementType::Ship
                && property.terrain != Terrain::Port
            || !is_terrain_reachable(
                &map,
                &registry,
                (t_pos.x, t_pos.y),
                (position.x, position.y),
                t_stats.movement_type,
            )
        {
            continue;
        }
        return_targets.push((
            t_pos.x.abs_diff(position.x) + t_pos.y.abs_diff(position.y),
            position.y,
            position.x,
            *position,
        ));
    }
    return_targets.sort_by_key(|target| (target.0, target.1, target.2));
    let Some(nearest_prop_pos) = return_targets.first().map(|target| target.3) else {
        return Some((
            transport_entity,
            crate::ai::command::AiCommand::Wait { target_pos: t_pos },
        ));
    };

    let mut best_tile = t_pos;
    let mut min_turn_dist = 999.0;
    let mut cache = crate::ai::turn_distance::TurnDistanceCache::default();
    let map = world.resource::<Map>();
    let registry = world.resource::<MasterDataRegistry>();

    for target_tile in reachable {
        let t_dist = crate::ai::turn_distance::calculate_turn_distance(
            map,
            registry,
            unit_positions,
            (target_tile.0, target_tile.1),
            (nearest_prop_pos.x, nearest_prop_pos.y),
            t_stats.movement_type,
            t_stats.max_movement,
            1,
            t_faction,
            &mut cache,
        );
        let dx = target_tile.0 as i32 - nearest_prop_pos.x as i32;
        let dy = target_tile.1 as i32 - nearest_prop_pos.y as i32;
        let m_dist = dx.abs() + dy.abs();
        let e_dist_sq = dx * dx + dy * dy;
        let score = t_dist.turns as f32
            + (m_dist as f32 / 1_000.0)
            + (e_dist_sq as f32 / 10_000_000.0)
            + (target_tile.0 as f32 / 100_000_000.0)
            + (target_tile.1 as f32 / 1_000_000_000.0);

        if score < min_turn_dist {
            min_turn_dist = score;
            best_tile = GridPosition {
                x: target_tile.0,
                y: target_tile.1,
            };
        }
    }
    Some((
        transport_entity,
        crate::ai::command::AiCommand::Wait {
            target_pos: best_tile,
        },
    ))
}

/// 輸送の共通準備を済ませて現在phaseの一段だけを実行する。
pub(super) fn execute(
    world: &mut World,
    squad: &mut SquadEdit<'_>,
    skip_entities: &HashSet<Entity>,
) -> Action {
    let mut ctx = transport_step_context(world, squad)?;
    let must_vacate = match try_early_transport_action(world, squad, skip_entities, &mut ctx) {
        ControlFlow::Break(action) => return action,
        ControlFlow::Continue(vacate) => vacate,
    };
    let cargo = select_target_cargo(world, squad, skip_entities, &ctx);
    let space = transport_move_space(world, &ctx);
    match ctx.phase {
        TransportPhase::Pickup => step_pickup(
            world,
            squad,
            skip_entities,
            &ctx,
            &space,
            cargo,
            must_vacate,
        ),
        TransportPhase::Transit => step_transit(world, squad, skip_entities, &ctx, &space, cargo),
        TransportPhase::Drop => step_drop(world, squad, skip_entities, &ctx, &space, cargo),
        TransportPhase::Return => step_return(world, &ctx, &space),
    }
}

/// 生産施設上にいる未行動輸送役だけを、指定済みの合流地点へ退避させる。
fn must_vacate_production_site(
    world: &World,
    squad: &Squad,
    skip_entities: &HashSet<Entity>,
    ctx: &TransportStepContext,
) -> bool {
    let phase = ctx.phase;
    let t_pos = ctx.position;
    let t_faction = ctx.player;
    let transport_entity = ctx.entity;
    phase == TransportPhase::Pickup
        && squad
            .plan
            .pickup_position
            .is_some_and(|pickup| pickup != t_pos)
        && active_production_positions(world, t_faction).contains(&(t_pos.x, t_pos.y))
        && world
            .get::<crate::components::HasMoved>(transport_entity)
            .is_some_and(|moved| !moved.0)
        && world
            .get::<crate::components::ActionCompleted>(transport_entity)
            .is_some_and(|action| !action.0)
        && !skip_entities.contains(&transport_entity)
}

/// 合流するcargoの状態。輸送役とcargoの行動済み判定を混同しない。
struct PickupCargo {
    entity: Entity,
    position: GridPosition,
    pickup_position: GridPosition,
    action_completed: bool,
}

/// 輸送役を動かせない場合だけ、cargoを搭乗または合流点へ前進させる。
fn approach_pickup_cargo(
    world: &mut World,
    skip_entities: &HashSet<Entity>,
    ctx: &TransportStepContext,
    space: &TransportMoveSpace,
    cargo: PickupCargo,
) -> Action {
    let cargo_entity = cargo.entity;
    let cargo_pos = cargo.position;
    let pickup_position = cargo.pickup_position;
    let cargo_action_completed = cargo.action_completed;
    let t_pos = ctx.position;
    let t_faction = ctx.player;
    let transport_entity = ctx.entity;
    let unit_positions = &space.occupants;
    // 2. 輸送機がすでに行動済みなら、歩兵を輸送機へ近づける
    let cargo_moved = world
        .get::<crate::components::HasMoved>(cargo_entity)
        .map_or(true, |moved| moved.0);

    if !cargo_moved && !cargo_action_completed && !skip_entities.contains(&cargo_entity) {
        let c_stats = world.get::<UnitStats>(cargo_entity).cloned()?;
        let c_fuel = world
            .get::<crate::components::Fuel>(cargo_entity)
            .map(|f| f.current)
            .unwrap_or(99);

        let cargo_reachable = {
            let map = world.resource::<Map>();
            let registry = world.resource::<MasterDataRegistry>();
            crate::systems::movement::calculate_reachable_tiles(
                map,
                unit_positions,
                (cargo_pos.x, cargo_pos.y),
                c_stats.movement_type,
                c_stats.max_movement,
                c_fuel,
                t_faction,
                c_stats.unit_type,
                registry,
            )
        };

        // 合流地点までこの手番に到達できるなら、同じ座標でWaitして翌手番に
        // Loadし直さず、移動と積載を1つのコマンドとして確定する。
        if cargo_reachable.contains(&(t_pos.x, t_pos.y)) {
            return Some((
                cargo_entity,
                crate::ai::command::AiCommand::Load {
                    transport_entity,
                    target_pos: t_pos,
                },
            ));
        }

        let mut best_tile = cargo_pos;
        let mut min_score = None;

        let mut cache = crate::ai::turn_distance::TurnDistanceCache::default();
        let map = world.resource::<Map>();
        let registry = world.resource::<MasterDataRegistry>();
        let cargo_goal = if t_pos == pickup_position {
            pickup_position
        } else {
            map.get_adjacent(pickup_position.x, pickup_position.y)
                .into_iter()
                .filter(|position| {
                    map.get_terrain(position.0, position.1)
                        .and_then(|terrain| {
                            crate::systems::movement::get_valid_movement_cost(
                                registry,
                                c_stats.movement_type,
                                terrain,
                            )
                        })
                        .is_some()
                        && is_terrain_reachable(
                            map,
                            registry,
                            (cargo_pos.x, cargo_pos.y),
                            *position,
                            c_stats.movement_type,
                        )
                })
                .min_by_key(|position| {
                    (
                        map.distance(cargo_pos.x, cargo_pos.y, position.0, position.1),
                        position.1,
                        position.0,
                    )
                })
                .map(|position| GridPosition {
                    x: position.0,
                    y: position.1,
                })
                .unwrap_or(cargo_pos)
        };

        for target_tile in &cargo_reachable {
            let t_dist = crate::ai::turn_distance::calculate_turn_distance(
                map,
                registry,
                unit_positions,
                (target_tile.0, target_tile.1),
                (cargo_goal.x, cargo_goal.y),
                c_stats.movement_type,
                c_stats.max_movement,
                0, // 輸送役到着前は合流点の隣接マスで待機する
                t_faction,
                &mut cache,
            );

            let dx = target_tile.0 as i32 - cargo_goal.x as i32;
            let dy = target_tile.1 as i32 - cargo_goal.y as i32;
            let m_dist = dx.abs() + dy.abs();
            let score = (t_dist, m_dist, target_tile.0, target_tile.1);

            if min_score.map_or(true, |m| score < m) {
                min_score = Some(score);
                best_tile = GridPosition {
                    x: target_tile.0,
                    y: target_tile.1,
                };
            }
        }

        return Some((
            cargo_entity,
            crate::ai::command::AiCommand::Wait {
                target_pos: best_tile,
            },
        ));
    }

    None
}
