//! 進軍路作戦（標準V4）。
//!
//! 進軍路の配分案（[`RouteOrder`]）を、要衝と所属部隊を持つ持続的な作戦として遂行する。
//! 作戦の所属は毎手番配り直さない。前の手番から作戦の要衝へ向かっている戦闘部隊は
//! そのまま所属を保ち、手の空いた部隊だけを、戦力の不足が最も大きい作戦へ加える。
//! 所属部隊は島の占領・戦闘の割当や首都ルートDAGの割当へ引き抜かせない。
//!
//! 動き出しは状況で決める。守りの作戦は直ちに要衝へ向かう。攻めの作戦は、敵より先に
//! 要衝へ着けるか、所属の戦力が要衝周りの敵を上回るなら直ちに進出し、どちらでもなければ
//! 集結点（敵より先に押さえられる最も前の地点）へ集まって戦力が揃うのを待つ。

use super::{CampaignResponsibility, MissionType, SquadManager};
use crate::ai::v4::capital_route::force_demand::RouteOrder;
use crate::components::{Faction, GridPosition, Health, PlayerId, Property, UnitStats};
use crate::resources::Map;
use bevy_ecs::prelude::*;

/// 攻めの作戦で、要衝の周りの敵とみなす距離（移動ターン）。
const KEY_POINT_ENEMY_TURNS: u32 = 2;

/// 手の空いた部隊を、最寄りの作戦より何手番分まで遠い作戦へ送ってよいか。
const FAR_FRONT_TURNS: u32 = 2;

/// 作戦の部隊が攻撃に向かってよい敵の、作戦の目標からの距離（マス）。目標の地域と、
/// その隣の地域まで。
const ENGAGEMENT_RADIUS: u32 = 2 * crate::ai::v4::capital_route::model::CAPITAL_ROUTE_REGION_RADIUS;

/// 今手番の進軍路作戦の所属。
pub(super) struct RouteOperationPlan {
    /// 作戦ごとの所属部隊（前の手番から引き継いだ戦闘部隊）。
    members: Vec<Vec<Entity>>,
    /// どの作戦にも属していない部隊。
    free: Vec<Entity>,
    /// 同じ進軍路で既に攻勢を開始した部隊が残る。損耗のたびに集結へ巻き戻さない。
    launched: Vec<bool>,
}

/// 部隊の戦力価値（価格×HP）。
fn unit_value(world: &World, entity: Entity) -> u32 {
    let Some(stats) = world.get::<UnitStats>(entity) else {
        return 0;
    };
    let hp = world.get::<Health>(entity).map_or(100, |health| {
        health.current.saturating_mul(100) / health.max.max(1)
    });
    stats.cost.saturating_mul(hp) / 100
}

/// 部隊の所属を前の手番から引き継ぐ。所属は作戦の要衝で記録してあり（任務の目標が敵unitへ
/// 変わっても保つ）、要衝が今の作戦に残っていればその作戦の所属のままにする。`units` は
/// 戦闘部隊と、要衝の周りに占領先の無い占領役。
pub(super) fn carry_over(
    world: &World,
    manager: &SquadManager,
    orders: &[RouteOrder],
    units: &[Entity],
) -> RouteOperationPlan {
    let map = world.get_resource::<Map>();
    let mut members = vec![Vec::new(); orders.len()];
    let mut free = Vec::new();
    let mut launched = vec![false; orders.len()];
    let attacking = manager
        .squads
        .iter()
        .filter(|squad| squad.plan.mission_type == MissionType::Attack)
        .flat_map(|squad| squad.members.iter().copied())
        .collect::<crate::collections::HashSet<_>>();
    let mut units = units.to_vec();
    units.sort_by_key(|entity| entity.to_bits());
    units.dedup();
    for entity in units {
        if world.get::<UnitStats>(entity).is_none() {
            continue;
        }
        // 作戦を見直して要衝が変わっても、同じ進軍路の作戦（前の要衝に最も近い要衝・集結点を
        // 持つ作戦）へ所属を移し、見直しのたびに部隊を別の進軍路へ動かさない。
        let kept = manager
            .route_operation_members
            .get(&entity)
            .and_then(|previous| {
                orders
                    .iter()
                    .enumerate()
                    .min_by_key(|(index, order)| {
                        let distance = |point: GridPosition| {
                            map.map_or(0, |map| {
                                map.distance(point.x, point.y, previous.x, previous.y)
                            })
                        };
                        (
                            distance(order.key_point).min(distance(order.staging)),
                            *index,
                        )
                    })
                    .map(|(index, _)| index)
            });
        match kept {
            Some(index) => {
                launched[index] |= attacking.contains(&entity);
                members[index].push(entity);
            }
            None => free.push(entity),
        }
    }
    RouteOperationPlan {
        members,
        free,
        launched,
    }
}

/// 今手番の所属を記録する。今手番の候補（`units`）のうち作戦に属さなくなった部隊
/// （占領へ回った等）は外す。SquadManagerは全プレイヤーで共有なので、候補外は触らない。
pub(super) fn record_membership(
    world: &World,
    manager: &mut SquadManager,
    orders: &[RouteOrder],
    units: &[Entity],
    plan_members: &[Vec<Entity>],
) {
    // 撃破済みのEntityはプレイヤーを問わず所属台帳から取り除く。
    manager.route_operation_members.retain(|entity, _| {
        world.get::<Faction>(*entity).is_some() && world.get::<UnitStats>(*entity).is_some()
    });
    for unit in units {
        manager.route_operation_members.remove(unit);
    }
    for (order, members) in orders.iter().zip(plan_members) {
        for member in members {
            manager
                .route_operation_members
                .insert(*member, order.key_point);
        }
    }
}

/// 進軍路作戦の責務を作る。手の空いた部隊を、戦力の不足が最も大きい作戦から
/// 順に、その要衝に最も近い部隊で埋めていく。作戦ごとに、守りなら要衝（防衛任務）、攻めなら
/// 進出の条件を満たせば要衝（攻撃任務）・満たさなければ集結点（防衛任務）を目標にする。
/// どの任務も集結を待たずに即座に向かわせる（[`authorize_departures`]）。
pub(super) fn responsibilities(
    world: &World,
    player: PlayerId,
    island: crate::ai::islands::IslandId,
    orders: &[RouteOrder],
    plan: RouteOperationPlan,
    connectivity: &mut crate::ai::turn_distance::TerrainConnectivity,
) -> (Vec<CampaignResponsibility>, Vec<Vec<Entity>>) {
    let Some(map) = world.get_resource::<Map>() else {
        return (Vec::new(), Vec::new());
    };
    let RouteOperationPlan {
        mut members,
        free,
        launched: previous_launch,
    } = plan;
    let mut pool = free;
    // 防衛の所要戦力を満たした後の余剰まで固定所属に残すと、配分案の集中が
    // 実行されず、主攻勢には火力が届かない。現在の要衝へ近い部隊を所要分だけ残す。
    for (index, order) in orders.iter().enumerate().filter(|(_, order)| order.defend) {
        members[index].sort_by_key(|entity| {
            let turns = world
                .get::<GridPosition>(*entity)
                .zip(world.get::<UnitStats>(*entity))
                .map_or(u32::MAX, |(at, stats)| {
                    map.distance(at.x, at.y, order.key_point.x, order.key_point.y)
                        .div_ceil(stats.max_movement.max(1))
                });
            (turns, unit_value(world, *entity), entity.to_bits())
        });
        let mut retained = 0_u64;
        members[index].retain(|entity| {
            if retained < u64::from(order.force) {
                retained = retained.saturating_add(u64::from(unit_value(world, *entity)));
                true
            } else {
                pool.push(*entity);
                false
            }
        });
    }
    let mut deficits = orders
        .iter()
        .zip(&members)
        .map(|(order, members)| {
            i64::from(order.force)
                - members
                    .iter()
                    .map(|member| i64::from(unit_value(world, *member)))
                    .sum::<i64>()
        })
        .collect::<Vec<_>>();
    let position = |entity: Entity| world.get::<GridPosition>(entity).copied();
    // 手の空いた部隊を、不足の大きい作戦から順に、その要衝に近い部隊で埋める。ただし最寄りの
    // 作戦より大きく遠い作戦（移動2手番分以上）へは送らず、遠い戦線への逐次投入を避ける。
    let distance_to = |entity: Entity, point: GridPosition| {
        position(entity).map_or(u32::MAX, |at| map.distance(at.x, at.y, point.x, point.y))
    };
    let reach = |entity: Entity| {
        world
            .get::<UnitStats>(entity)
            .map_or(1, |stats| stats.max_movement.max(1))
    };
    // 自軍首都の生産圏にいる部隊（生産直後など）はどの作戦へも送れる。前線へ出た部隊だけ、
    // 遠い戦線への付け替えを制限する。
    let capital = world
        .iter_entities()
        .find(|entity| {
            entity.get::<Property>().is_some_and(|property| {
                property.owner_id == Some(player)
                    && property.terrain == crate::resources::Terrain::Capital
            })
        })
        .and_then(|entity| entity.get::<GridPosition>().copied());
    let at_home = |entity: Entity| {
        capital.is_some_and(|capital| {
            distance_to(entity, capital) <= crate::systems::production::PRODUCTION_RANGE as u32
        })
    };
    // 充足済みの防衛は、候補にも「最寄り作戦」の比較対象にも戻さない。
    // 近くの満員の守備隊を基準にすると、遠方の唯一の攻勢へ余剰を送れなくなる。
    let accepts = |index: usize, deficits: &[i64]| !orders[index].defend || deficits[index] > 0;
    let eligible = |entity: Entity, index: usize, deficits: &[i64]| {
        if at_home(entity) {
            return true;
        }
        let nearest = orders
            .iter()
            .enumerate()
            .filter(|(index, _)| accepts(*index, deficits))
            .map(|(_, order)| distance_to(entity, order.key_point))
            .min()
            .unwrap_or(u32::MAX);
        distance_to(entity, orders[index].key_point)
            <= nearest.saturating_add(reach(entity).saturating_mul(FAR_FRONT_TURNS))
    };
    while !pool.is_empty() {
        let mut by_deficit = (0..orders.len())
            .filter(|index| accepts(*index, &deficits))
            .collect::<Vec<_>>();
        by_deficit.sort_by_key(|index| (std::cmp::Reverse(deficits[*index]), *index));
        let choice = by_deficit.into_iter().find_map(|index| {
            let key = orders[index].key_point;
            pool.iter()
                .enumerate()
                .filter(|(_, entity)| eligible(**entity, index, &deficits))
                .min_by_key(|(_, entity)| (distance_to(**entity, key), entity.to_bits()))
                .map(|(slot, _)| (index, slot))
        });
        let Some((index, slot)) = choice else {
            break;
        };
        let entity = pool.remove(slot);
        deficits[index] -= i64::from(unit_value(world, entity));
        members[index].push(entity);
    }
    // 作戦ごとに、所属部隊だけで目標（守りの要衝、攻めは進出するなら要衝・待つなら集結点）
    // を妨げる敵へ有効な部隊を当て、残りを目標へ前進させる。
    let enemies = enemy_units(world, player);
    let mut responsibilities = Vec::new();
    for (index, (order, members)) in orders.iter().zip(members.iter().cloned()).enumerate() {
        if members.is_empty() {
            continue;
        }
        // 進出した攻めの作戦は攻撃任務にし、不利な交換も受け入れて要衝を取りに行く。
        // 守りの作戦と、集結点で待つ攻めの作戦は防衛任務（有利な交換だけ攻撃）にする。
        let launched = !order.defend
            && (previous_launch.get(index).copied().unwrap_or(false)
                || launches(world, map, player, order, &members));
        let target = if order.defend {
            order.key_point
        } else if launched {
            advanced_offensive_target(world, map, player, order, orders, &members, &enemies)
        } else {
            order.staging
        };
        let mission_type = if launched {
            MissionType::Attack
        } else {
            MissionType::Defense
        };
        // 前面（敵が要衝へ入ってくる側）を決め、直接攻撃の部隊は前面で壁になり、間接攻撃の
        // 部隊は前面を射程に収める自軍側の陣地から撃つ。攻め・守りともに前面へ展開する。
        let front = front_of(world, map, &enemies, target);
        let (indirect, direct): (Vec<Entity>, Vec<Entity>) = members
            .iter()
            .copied()
            .partition(|member| is_indirect(world, *member));
        let mut taken = crate::collections::HashSet::default();
        for gun in indirect {
            let found = firing_position(world, map, player, &enemies, gun, target, front, &taken);
            if std::env::var_os("OPENWARS_ROUTE_OPS_DEBUG").is_some() {
                let stats = world.get::<UnitStats>(gun);
                eprintln!(
                    "FIRE gun={:?} type={:?} range={:?} target=({},{}) front=({},{}) launched={launched} -> {found:?}",
                    gun,
                    stats.map(|s| s.unit_type),
                    stats.map(|s| (s.min_range, s.max_range)),
                    target.x,
                    target.y,
                    front.x,
                    front.y
                );
            }
            // 進出先の周りに安全な陣地が無ければ、集結点の前面を撃てる陣地、それも無ければ
            // 現在地（前進中なら維持）、それも無ければ集結点に留め、間接攻撃の部隊を敵の只中へ出さない。
            let at = world.get::<GridPosition>(gun).copied();
            let position = found
                .or_else(|| {
                    launched
                        .then(|| {
                            let staging_front = front_of(world, map, &enemies, order.staging);
                            firing_position(
                                world,
                                map,
                                player,
                                &enemies,
                                gun,
                                order.staging,
                                staging_front,
                                &taken,
                            )
                        })
                        .flatten()
                })
                .or(at)
                .unwrap_or(if launched { order.staging } else { target });
            taken.insert(position);
            responsibilities.push(CampaignResponsibility {
                // 射撃位置は参照点であり、防衛任務を意味しない。攻勢を開始した
                // 作戦の砲兵もAttackとし、地域制圧・輸送へ攻勢の意図を引き継ぐ。
                mission_type: mission_type.clone(),
                target: position,
                members: vec![gun],
            });
        }
        if direct.is_empty() {
            continue;
        }
        // 作戦の目標から離れた敵を追うと、作戦の部隊が要衝から散る。前面の周りの敵への
        // 攻撃だけを残し、それ以外は前面へ向かわせる。
        let assigned = super::objective_allocation::route_operation_responsibilities(
            world,
            player,
            island,
            &direct,
            front,
            connectivity,
        )
        .into_iter()
        .filter(|responsibility| {
            responsibility.mission_type != MissionType::Attack
                || map.distance(
                    responsibility.target.x,
                    responsibility.target.y,
                    front.x,
                    front.y,
                ) <= ENGAGEMENT_RADIUS
        })
        .collect::<Vec<_>>();
        // 割当から漏れた所属部隊（目標へ届かない等）も、作戦から外さず前面へ向かわせる。
        let covered = assigned
            .iter()
            .flat_map(|responsibility| responsibility.members.iter().copied())
            .collect::<crate::collections::HashSet<_>>();
        let rest = direct
            .into_iter()
            .filter(|member| !covered.contains(member))
            .collect::<Vec<_>>();
        responsibilities.extend(assigned);
        if !rest.is_empty() {
            responsibilities.push(CampaignResponsibility {
                mission_type,
                target: front,
                members: rest,
            });
        }
    }
    (responsibilities, members)
}

/// 攻めの作戦を要衝へ進出させるか。敵より先に要衝へ着けるか、所属の戦力が要衝の周りの
/// 敵を上回るなら進出する。
fn launches(
    world: &World,
    map: &Map,
    player: PlayerId,
    order: &RouteOrder,
    members: &[Entity],
) -> bool {
    let key = order.key_point;
    let eta = |entity: Entity| -> Option<u32> {
        let at = world.get::<GridPosition>(entity)?;
        let stats = world.get::<UnitStats>(entity)?;
        Some(
            map.distance(at.x, at.y, key.x, key.y)
                .div_ceil(stats.max_movement.max(1)),
        )
    };
    let our_arrival = members.iter().filter_map(|member| eta(*member)).min();
    let enemies = world
        .iter_entities()
        .filter(|entity| {
            entity
                .get::<Faction>()
                .is_some_and(|faction| faction.0 != player)
        })
        .map(|entity| entity.id())
        .collect::<Vec<_>>();
    let enemy_arrival = enemies.iter().filter_map(|enemy| eta(*enemy)).min();
    let enemy_near = enemies
        .iter()
        .filter(|enemy| eta(**enemy).is_some_and(|turns| turns <= KEY_POINT_ENEMY_TURNS))
        .map(|enemy| unit_value(world, *enemy))
        .sum::<u32>();
    let our_force = members
        .iter()
        .map(|member| unit_value(world, *member))
        .sum::<u32>();
    let wins_race = match (our_arrival, enemy_arrival) {
        (Some(ours), Some(theirs)) => ours <= theirs,
        (Some(_), None) => true,
        _ => false,
    };
    if std::env::var_os("OPENWARS_ROUTE_OPS_DEBUG").is_some() {
        let turn = world
            .get_resource::<crate::resources::MatchState>()
            .map_or(0, |state| state.current_turn_number.0);
        eprintln!(
            "ROUTE_OP T{turn} P{} key=({},{}) staging=({},{}) members={} our_force={our_force} enemy_near={enemy_near} our_eta={our_arrival:?} enemy_eta={enemy_arrival:?} launch={}",
            player.0,
            key.x,
            key.y,
            order.staging.x,
            order.staging.y,
            members.len(),
            wins_race || our_force > enemy_near
        );
    }
    wins_race || our_force > enemy_near
}

/// 進軍路作戦の部隊のSquadは、集結を待たずに出発させる。作戦の進出・待機は要衝と集結点の
/// 選択で決めてあり、Squadの出発許可で二重に止めない。
pub(super) fn authorize_departures(
    manager: &mut SquadManager,
    player: PlayerId,
    members: &[Vec<Entity>],
) {
    let members = members
        .iter()
        .flatten()
        .copied()
        .collect::<crate::collections::HashSet<_>>();
    for mut squad in manager.edit_each().filter(|squad| {
        squad.plan.owner_id == Some(player)
            && matches!(
                squad.plan.mission_type,
                MissionType::Attack | MissionType::Defense
            )
            && squad.members.iter().any(|member| members.contains(member))
    }) {
        squad.plan_mut().departure_authorized = true;
        if squad.plan.phase == crate::ai::squad::MissionPhase::Forming {
            squad.plan_mut().phase = crate::ai::squad::MissionPhase::MovingToTarget;
        }
    }
}

/// 敵の地上・航空部隊の位置と移動力。
fn enemy_units(world: &World, player: PlayerId) -> Vec<(GridPosition, u32)> {
    world
        .iter_entities()
        .filter_map(|entity| {
            let faction = entity.get::<Faction>()?;
            let position = *entity.get::<GridPosition>()?;
            let stats = entity.get::<UnitStats>()?;
            (faction.0 != player).then_some((position, stats.max_movement.max(1)))
        })
        .collect()
}

/// 敵が最も早く着く手番（直線距離÷移動力の概算）。敵がいなければNone。
fn enemy_eta(map: &Map, enemies: &[(GridPosition, u32)], cell: GridPosition) -> Option<u32> {
    enemies
        .iter()
        .map(|(position, movement)| {
            map.distance(position.x, position.y, cell.x, cell.y)
                .div_ceil(*movement)
        })
        .min()
}

/// 間接攻撃の部隊か（移動後に撃てない最小射程2以上の兵器）。
fn is_indirect(world: &World, entity: Entity) -> bool {
    world
        .get::<UnitStats>(entity)
        .is_some_and(|stats| stats.min_range > 1)
}

/// 攻めの作戦で、現在の要衝が既に確保されたか味方が占領中であり十分な戦力がある場合、
/// 進軍路の先にある未確保の施設、または敵部隊へ前哨目標を前進させる。
fn advanced_offensive_target(
    world: &World,
    map: &Map,
    player: PlayerId,
    order: &RouteOrder,
    orders: &[RouteOrder],
    members: &[Entity],
    enemies: &[(GridPosition, u32)],
) -> GridPosition {
    let key = order.key_point;
    let key_owned = world.iter_entities().any(|entity| {
        entity.get::<GridPosition>() == Some(&key)
            && entity
                .get::<Property>()
                .is_some_and(|p| p.owner_id == Some(player))
    });
    let key_capturing = world.iter_entities().any(|entity| {
        entity.get::<GridPosition>() == Some(&key)
            && entity.get::<Faction>().is_some_and(|f| f.0 == player)
            && entity.get::<UnitStats>().is_some_and(|s| s.can_capture)
    });

    let our_force_at_key = members
        .iter()
        .filter(|m| {
            world
                .get::<GridPosition>(**m)
                .is_some_and(|pos| map.distance(pos.x, pos.y, key.x, key.y) <= ENGAGEMENT_RADIUS)
        })
        .map(|m| unit_value(world, *m))
        .sum::<u32>();
    let enemy_force_at_key = enemies
        .iter()
        .filter(|(pos, _)| map.distance(pos.x, pos.y, key.x, key.y) <= ENGAGEMENT_RADIUS)
        .map(|(pos, _)| {
            world
                .iter_entities()
                .filter(|e| {
                    e.get::<GridPosition>() == Some(pos)
                        && e.get::<Faction>().is_some_and(|f| f.0 != player)
                })
                .map(|e| unit_value(world, e.id()))
                .sum::<u32>()
        })
        .sum::<u32>();

    // 要衝周辺の敵が掃討された（enemy_force_at_key == 0）か、自軍が圧倒的優勢（味方戦力が敵戦力の3倍以上）なら、
    // 要衝そのものに全員が立ち止まる必要はなく、戦闘部隊・間接部隊は次の未確保施設へ前進する。
    let cleared = enemy_force_at_key == 0
        || (our_force_at_key >= 20_000 && our_force_at_key >= enemy_force_at_key.saturating_mul(3));

    // 要衝が未所有で、占領中でもなく、かつ周辺の敵も掃討されていないなら、要衝そのものを目指す
    if !key_owned && !cleared && (!key_capturing || enemy_force_at_key > our_force_at_key) {
        return key;
    }

    // 要衝が確保された、または自軍優勢で占領中なら、要衝の先（未所有の施設）を探す
    let mut next_properties = world
        .iter_entities()
        .filter_map(|entity| {
            let pos = *entity.get::<GridPosition>()?;
            let property = entity.get::<Property>()?;
            (property.owner_id != Some(player)).then_some(pos)
        })
        .collect::<Vec<_>>();

    // key から離れており、守りの進軍路の先ではない未所有施設
    next_properties.retain(|pos| {
        map.distance(pos.x, pos.y, key.x, key.y) > 0
            && !super::objective_allocation::beyond_defended_route(world, map, *pos, orders)
    });

    if let Some(&next) = next_properties
        .iter()
        .min_by_key(|pos| map.distance(pos.x, pos.y, key.x, key.y))
    {
        return next;
    }

    // 未所有施設がなければ、最も近い敵部隊へ
    if let Some(&(nearest_enemy, _)) = enemies
        .iter()
        .min_by_key(|(pos, _)| map.distance(pos.x, pos.y, key.x, key.y))
    {
        return nearest_enemy;
    }

    key
}

/// 要衝の前面。要衝の周囲（DAG地域の半径+1）のうち、敵が最も早く着くマス。
/// これにより、要衝に殺到するのではなく敵が来る隘路・侵入路の出口（前哨境界）へ壁を張る。
fn front_of(
    world: &World,
    map: &Map,
    enemies: &[(GridPosition, u32)],
    target: GridPosition,
) -> GridPosition {
    let Some(registry) = world.get_resource::<crate::resources::master_data::MasterDataRegistry>()
    else {
        return target;
    };
    if enemies.is_empty() {
        return target;
    }
    let radius = crate::ai::v4::capital_route::model::CAPITAL_ROUTE_REGION_RADIUS + 1;
    cells_around(map, target, radius)
        .into_iter()
        .filter(|cell| {
            map.get_terrain(cell.x, cell.y).is_some_and(|terrain| {
                crate::systems::movement::get_valid_movement_cost(
                    registry,
                    crate::resources::MovementType::Infantry,
                    terrain,
                )
                .is_some()
            })
        })
        .min_by_key(|cell| {
            (
                enemy_eta(map, enemies, *cell).unwrap_or(u32::MAX),
                map.distance(cell.x, cell.y, target.x, target.y),
                cell.y,
                cell.x,
            )
        })
        .unwrap_or(target)
}

/// `center` から距離 `radius` 以内の盤面内のマス。
fn cells_around(map: &Map, center: GridPosition, radius: u32) -> Vec<GridPosition> {
    let r = radius as i64;
    let mut cells = Vec::new();
    for dy in -r..=r {
        for dx in -r..=r {
            let x = center.x as i64 + dx;
            let y = center.y as i64 + dy;
            if x < 0 || y < 0 || x >= map.width as i64 || y >= map.height as i64 {
                continue;
            }
            let cell = GridPosition {
                x: x as usize,
                y: y as usize,
            };
            if map.distance(cell.x, cell.y, center.x, center.y) <= radius {
                cells.push(cell);
            }
        }
    }
    cells
}

/// 間接攻撃の部隊の射撃陣地。
///
/// 安全なマス（敵が次の手番に隣接できない、または敵との間に味方の直接攻撃の部隊が
/// 壁として立つ）だけを候補にし、次の順で選ぶ。
/// 1. 今いる敵を射程に収めるマス（撃てる敵の戦力価値が大きいほど良い）
/// 2. 前面を射程に収めるマス
/// 3. 前面へ最も近いマス（射程外でも、後方の集結点へ下がらず前面の後ろへ詰める）
///
/// 同じ順位では自軍の拠点（補給と防御）、敵から遠いマス、部隊に近いマスを選ぶ。
/// 他の部隊が使う陣地と敵のいるマスは除く。安全なマスが無ければNone。
#[allow(clippy::too_many_arguments)]
fn firing_position(
    world: &World,
    map: &Map,
    player: PlayerId,
    enemies: &[(GridPosition, u32)],
    gun: Entity,
    target: GridPosition,
    front: GridPosition,
    taken: &crate::collections::HashSet<GridPosition>,
) -> Option<GridPosition> {
    let stats = world.get::<UnitStats>(gun)?;
    let at = *world.get::<GridPosition>(gun)?;
    let registry = world.get_resource::<crate::resources::master_data::MasterDataRegistry>()?;
    let own_properties = world
        .iter_entities()
        .filter_map(|entity| {
            let position = *entity.get::<GridPosition>()?;
            let property = entity.get::<Property>()?;
            (property.owner_id == Some(player)).then_some(position)
        })
        .collect::<crate::collections::HashSet<_>>();
    let unowned_properties = world
        .iter_entities()
        .filter_map(|entity| {
            let position = *entity.get::<GridPosition>()?;
            let property = entity.get::<Property>()?;
            (property.owner_id != Some(player)).then_some(position)
        })
        .collect::<crate::collections::HashSet<_>>();
    // 壁になる味方の直接攻撃の部隊の位置。
    let screen = world
        .iter_entities()
        .filter_map(|entity| {
            let faction = entity.get::<Faction>()?;
            let position = *entity.get::<GridPosition>()?;
            let unit_stats = entity.get::<UnitStats>()?;
            (faction.0 == player && unit_stats.min_range <= 1 && entity.id() != gun)
                .then_some(position)
        })
        .collect::<Vec<_>>();
    // 撃てる敵の戦力価値（価格×HP）。
    let enemy_values = world
        .iter_entities()
        .filter_map(|entity| {
            let faction = entity.get::<Faction>()?;
            let position = *entity.get::<GridPosition>()?;
            let unit_stats = entity.get::<UnitStats>()?;
            let health = entity.get::<Health>()?;
            (faction.0 != player).then_some((
                position,
                unit_stats.cost.saturating_mul(health.current) / health.max.max(1),
            ))
        })
        .collect::<Vec<_>>();
    let enemy_cells = enemies
        .iter()
        .map(|(position, _)| *position)
        .collect::<crate::collections::HashSet<_>>();
    // 敵の直接攻撃が届くまでの手番（隣接するまで）と、最も近い敵の位置。
    let nearest_threat = |cell: GridPosition| {
        enemies
            .iter()
            .map(|(position, movement)| {
                (
                    map.distance(position.x, position.y, cell.x, cell.y)
                        .saturating_sub(1)
                        .div_ceil(*movement),
                    *position,
                )
            })
            .min_by_key(|(turns, position)| (*turns, position.y, position.x))
    };
    let threat = |cell: GridPosition| nearest_threat(cell).map(|(turns, _)| turns);
    // 敵が次の手番に隣接できないか、敵との間に味方の壁が立つマスを安全とみなす。
    let safe = |cell: GridPosition| match nearest_threat(cell) {
        None => true,
        Some((0, _)) => false,
        Some((turns, _)) if turns >= 2 => true,
        Some((_, enemy)) => {
            let reach = map.distance(cell.x, cell.y, enemy.x, enemy.y);
            screen.iter().any(|wall| {
                let dist_wall_enemy = map.distance(wall.x, wall.y, enemy.x, enemy.y);
                let dist_wall_cell = map.distance(wall.x, wall.y, cell.x, cell.y);
                dist_wall_enemy < reach && dist_wall_cell <= 3
            })
        }
    };
    let in_range = |cell: GridPosition, aim: GridPosition| {
        let range = map.distance(cell.x, cell.y, aim.x, aim.y);
        range >= stats.min_range.max(1) && range <= stats.max_range.max(1)
    };
    let hittable = |cell: GridPosition| {
        enemy_values
            .iter()
            .filter(|(position, _)| in_range(cell, *position))
            .map(|(_, value)| *value)
            .max()
            .unwrap_or(0)
    };
    let search_radius = stats.max_range.max(ENGAGEMENT_RADIUS);
    let mut candidates = cells_around(map, target, search_radius);
    if front != target {
        candidates.extend(cells_around(map, front, search_radius));
        candidates.sort_unstable_by_key(|cell| (cell.y, cell.x));
        candidates.dedup();
    }
    candidates
        .into_iter()
        .filter(|cell| {
            !taken.contains(cell)
                && !enemy_cells.contains(cell)
                && !unowned_properties.contains(cell)
                && map.get_terrain(cell.x, cell.y).is_some_and(|terrain| {
                    crate::systems::movement::get_valid_movement_cost(
                        registry,
                        stats.movement_type,
                        terrain,
                    )
                    .is_some()
                })
                && safe(*cell)
        })
        .min_by_key(|cell| {
            let target_value = hittable(*cell);
            let class = if target_value > 0 {
                0
            } else if in_range(*cell, front) {
                1
            } else {
                2
            };
            (
                class,
                std::cmp::Reverse(target_value),
                // 射程外のマスは前面へ近いほど良い（射程内の順位には影響しない）。
                if class == 2 {
                    map.distance(cell.x, cell.y, front.x, front.y)
                } else {
                    0
                },
                !own_properties.contains(cell),
                std::cmp::Reverse(threat(*cell).unwrap_or(u32::MAX)),
                map.distance(cell.x, cell.y, at.x, at.y),
                cell.y,
                cell.x,
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indirect_fire_post_preserves_the_launched_operation_mission() {
        let mut world = World::new();
        world.insert_resource(Map::new(
            20,
            5,
            crate::resources::Terrain::Plains,
            crate::resources::GridTopology::Square,
        ));
        let data = crate::resources::master_data::MasterDataRegistry::load().unwrap();
        let stats = data
            .create_unit_stats(&crate::resources::master_data::UnitName(
                crate::resources::UnitType::Artillery.as_str().to_owned(),
            ))
            .unwrap();
        world.insert_resource(data);
        world.insert_resource(crate::resources::DamageChart::new());
        let gun = world
            .spawn((
                Faction(PlayerId(1)),
                GridPosition { x: 3, y: 2 },
                stats,
                Health {
                    current: 100,
                    max: 100,
                },
            ))
            .id();
        let mut order = RouteOrder {
            key_point: GridPosition { x: 15, y: 2 },
            staging: GridPosition { x: 5, y: 2 },
            defend: false,
            force: unit_value(&world, gun),
        };
        for expected in [MissionType::Attack, MissionType::Defense] {
            order.defend = expected == MissionType::Defense;
            let plan = RouteOperationPlan {
                members: vec![vec![gun]],
                free: Vec::new(),
                launched: vec![false],
            };
            let (assigned, _) = responsibilities(
                &world,
                PlayerId(1),
                crate::ai::islands::IslandId(0),
                &[order],
                plan,
                &mut Default::default(),
            );
            let role = assigned
                .iter()
                .find(|role| role.members.contains(&gun))
                .unwrap();
            assert_eq!(
                role.mission_type, expected,
                "射撃位置と攻勢・防衛の目的を混同しない"
            );
            assert_ne!(role.target, order.key_point, "砲台へは射撃位置を渡す");
        }
    }

    #[test]
    fn launched_operation_does_not_restage_after_attrition_without_a_defend_order() {
        let mut world = World::new();
        let map = Map::new(
            20,
            5,
            crate::resources::Terrain::Plains,
            crate::resources::GridTopology::Square,
        );
        world.insert_resource(map.clone());
        let data = crate::resources::master_data::MasterDataRegistry::load().unwrap();
        let stats = data
            .create_unit_stats(&crate::resources::master_data::UnitName(
                crate::resources::UnitType::Artillery.as_str().to_owned(),
            ))
            .unwrap();
        let enemy_stats = data
            .create_unit_stats(&crate::resources::master_data::UnitName(
                crate::resources::UnitType::MdTank.as_str().to_owned(),
            ))
            .unwrap();
        world.insert_resource(data);
        world.insert_resource(crate::resources::DamageChart::new());
        let gun = world
            .spawn((
                Faction(PlayerId(1)),
                GridPosition { x: 3, y: 2 },
                stats,
                Health {
                    current: 100,
                    max: 100,
                },
            ))
            .id();
        for at in [
            GridPosition { x: 15, y: 2 },
            GridPosition { x: 16, y: 2 },
            GridPosition { x: 15, y: 3 },
        ] {
            world.spawn((
                Faction(PlayerId(2)),
                at,
                enemy_stats.clone(),
                Health {
                    current: 100,
                    max: 100,
                },
            ));
        }
        let mut manager = SquadManager::default();
        manager
            .create_owned_squad(MissionType::Attack, PlayerId(1))
            .members
            .insert(gun);
        let mut order = RouteOrder {
            key_point: GridPosition { x: 15, y: 2 },
            staging: GridPosition { x: 5, y: 2 },
            defend: false,
            force: unit_value(&world, gun),
        };
        manager.route_operation_members.insert(gun, order.key_point);
        assert!(!launches(&world, &map, PlayerId(1), &order, &[gun]));
        for expected in [MissionType::Attack, MissionType::Defense] {
            order.defend = expected == MissionType::Defense;
            let orders = [order];
            let plan = carry_over(&world, &manager, &orders, &[gun]);
            let (assigned, _) = responsibilities(
                &world,
                PlayerId(1),
                crate::ai::islands::IslandId(0),
                &orders,
                plan,
                &mut Default::default(),
            );
            let role = assigned
                .iter()
                .find(|role| role.members.contains(&gun))
                .unwrap();
            assert_eq!(
                role.mission_type, expected,
                "明示的な防衛変更があるときだけ攻勢を止める"
            );
        }
    }

    /// 防衛の余剰と新規部隊が、充足済みの近い守備隊へ再吸収されない。
    #[test]
    fn defense_keeps_required_force_and_releases_surplus_to_the_offensive() {
        let mut world = World::new();
        world.insert_resource(Map::new(
            20,
            5,
            crate::resources::Terrain::Plains,
            crate::resources::GridTopology::Square,
        ));
        let data = crate::resources::master_data::MasterDataRegistry::load().unwrap();
        let stats = data
            .create_unit_stats(&crate::resources::master_data::UnitName(
                crate::resources::UnitType::Tank.as_str().to_owned(),
            ))
            .unwrap();
        let cost = stats.cost;
        world.insert_resource(data);
        world.insert_resource(crate::resources::DamageChart::new());
        let units = (0..4)
            .map(|_| {
                world
                    .spawn((
                        Faction(PlayerId(1)),
                        GridPosition { x: 2, y: 2 },
                        stats.clone(),
                        Health {
                            current: 100,
                            max: 100,
                        },
                    ))
                    .id()
            })
            .collect::<Vec<_>>();
        for kept in [0_usize, 1] {
            let orders = [
                RouteOrder {
                    key_point: GridPosition { x: 3, y: 2 },
                    staging: GridPosition { x: 3, y: 2 },
                    defend: true,
                    force: cost * kept as u32,
                },
                RouteOrder {
                    key_point: GridPosition { x: 17, y: 2 },
                    staging: GridPosition { x: 10, y: 2 },
                    defend: false,
                    force: cost * (units.len() - kept) as u32,
                },
            ];
            let plan = RouteOperationPlan {
                members: vec![units[..3].to_vec(), Vec::new()],
                free: vec![units[3]],
                launched: vec![false, true],
            };
            let (roles, members) = responsibilities(
                &world,
                PlayerId(1),
                crate::ai::islands::IslandId(0),
                &orders,
                plan,
                &mut Default::default(),
            );
            assert_eq!(members[0].len(), kept, "守備に必要な戦力を残す");
            assert_eq!(
                members[1].len(),
                units.len() - kept,
                "余剰と新規戦力は主攻勢へ送る"
            );
            let unique = members
                .iter()
                .flatten()
                .copied()
                .collect::<crate::collections::HashSet<_>>();
            assert_eq!(
                unique.len(),
                units.len(),
                "同じ部隊を二重配属・消失させない"
            );
            for member in &members[1] {
                assert_eq!(
                    roles
                        .iter()
                        .find(|role| role.members.contains(member))
                        .unwrap()
                        .mission_type,
                    MissionType::Attack,
                    "遠方でも余剰を防衛待機へ戻さない"
                );
            }
        }
    }

    #[test]
    fn membership_removes_despawned_entities_but_keeps_other_players() {
        let mut world = World::new();
        let removed = world.spawn((Faction(PlayerId(1)), UnitStats::mock())).id();
        let other = world.spawn((Faction(PlayerId(2)), UnitStats::mock())).id();
        let mut manager = SquadManager::default();
        let target = GridPosition { x: 1, y: 1 };
        manager.route_operation_members.insert(removed, target);
        manager.route_operation_members.insert(other, target);
        world.despawn(removed);

        record_membership(&world, &mut manager, &[], &[], &[]);

        assert!(!manager.route_operation_members.contains_key(&removed));
        assert_eq!(manager.route_operation_members.get(&other), Some(&target));
    }
}
