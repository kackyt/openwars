//! 標準経路V4の、同一手番内の「撃破→進入」実行順序。
//!
//! `tactics/v2` は未行動の全unitを横断して最良の1手だけを選ぶため、
//! 「この敵を倒すのに必要な火力を先に撃ち切り、別の1体をそのセルへ入れる」という
//! 複数unitにまたがる順序を表現できない。結果として、撃破に必要な火力を出した後に
//! 進入できるunitが残らない。
//!
//! この段は採点・候補集合・優先度を一切変えない。DAGの前進区間（Advance）が
//! 占領・進入すべきセルを敵地上unitが塞いでいる場合だけ、
//! 1. 現在位置から静止射撃で撃てる未行動unitの期待ダメージを降順に足し、
//!    敵HP以上になる最小集合を「撃破担当」とし、
//! 2. 撃破担当と重ならない、そのセルへ今手番に到達できる直接戦闘・占領役を「進入役」として予約し、
//! 3. 撃破担当の射撃を通常行動より先に1件ずつ発行する。
//!
//! 注文は毎ステップ盤面から作り直す（状態を持ち越さない）。敵が倒れる・撃破担当が
//! 行動済みになる・火力が足りなくなると注文は自然に消え、進入役は通常行動へ戻る。
//! したがって進入役が待機で詰まることはない（デッドロックを作らない）。

use crate::ai::command::AiCommand;
use crate::ai::v4::capital_route::model::{CapitalRouteExecutionPhase, CapitalRoutePathRegistry};
use crate::ai::v4::{
    Entity, Faction, GridPosition, HashSet, Health, Map, MasterDataRegistry, MovementType,
    PlayerId, Transporting, UnitStats, World,
};
use bevy_ecs::prelude::Resource;
use serde_json::{Value, json};

#[cfg(test)]
mod tests;

/// 1体の敵を排除してセルへ入るための、今手番の実行順序。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FireFollowOrder {
    /// 排除対象の敵と、そのセル。
    pub(crate) target: Entity,
    pub(crate) cell: GridPosition,
    /// この敵を倒すために今手番で撃つ担当（期待ダメージ降順。合計期待ダメージ >= 敵HP）。
    pub(crate) fire_tasks: Vec<(Entity, u32)>,
    /// 排除後にそのセルへ入る役。撃破担当とは重複させない。
    pub(crate) entry: Entity,
}

/// 手番ごとの注文と、診断用の追跡記録。
///
/// 注文自体は毎ステップ作り直す。`known` は「この手番に一度でも注文を作った敵」で、
/// 排除・破棄を診断イベントとして一度だけ出すためだけに使う（判断には使わない）。
#[derive(Resource, Debug, Default)]
pub struct FireFollowOrderRegistry {
    player: Option<PlayerId>,
    turn: u32,
    orders: Vec<FireFollowOrder>,
    known: Vec<(Entity, GridPosition, Entity)>,
    trace_enabled: Option<bool>,
    trace: Vec<Value>,
}

impl FireFollowOrderRegistry {
    /// 現在有効な注文の進入役。通常行動の候補から外す（既存のskip機構へ渡す）。
    pub(crate) fn active_entries(&self, player_id: PlayerId) -> impl Iterator<Item = Entity> + '_ {
        self.orders
            .iter()
            .filter(move |_| self.player == Some(player_id))
            .map(|order| order.entry)
    }
}

/// 診断出力の有無。`OPENWARS_FIRE_FOLLOW_TRACE` を設定したプロセスだけ記録する。
fn trace_enabled_from_env() -> bool {
    std::env::var_os("OPENWARS_FIRE_FOLLOW_TRACE").is_some()
}

/// MCPがstepごとに取り出す診断イベント。ゲーム状態には影響しない。
pub fn take_trace(world: &mut World) -> Vec<Value> {
    world
        .get_resource_mut::<FireFollowOrderRegistry>()
        .map(|mut registry| std::mem::take(&mut registry.trace))
        .unwrap_or_default()
}

/// 撃破担当の静止射撃を1件返す。注文が無ければNone（通常行動へ委ねる）。
///
/// `skip` は通常行動と同じ除外集合（行動済み・輸送Squad・砲台フェリー）で、
/// ここに含まれるunitは撃破担当にも進入役にもしない。
pub(crate) fn decide_fire_follow_action(
    world: &mut World,
    player_id: PlayerId,
    skip: &HashSet<Entity>,
) -> Option<(Entity, AiCommand)> {
    // 排除直後の空きセルへの進入は、新しい注文や通常採点より先に確定する。
    // 診断用knownではなく前ステップの実注文を使うため、traceのON/OFFに依存しない。
    if let Some(action) = released_entry_action(world, player_id, skip) {
        world
            .resource_mut::<FireFollowOrderRegistry>()
            .orders
            .retain(|order| order.entry != action.0);
        return Some(action);
    }
    let orders = observe(world, player_id, skip);
    let turn = world
        .get_resource::<crate::resources::MatchState>()
        .map_or(0, |state| state.current_turn_number.0);
    // 注文の最初の撃破担当（最も期待ダメージの大きいunit）から撃つ。重い一撃で敵HPを
    // 先に削ると、後続の直接攻撃が受ける反撃も小さくなる。
    let action = orders.first().and_then(|order| {
        let (shooter, damage) = *order.fire_tasks.first()?;
        let at = *world.get::<GridPosition>(shooter)?;
        Some((
            order,
            shooter,
            damage,
            AiCommand::Attack {
                target_pos: at,
                target_entity: order.target,
            },
        ))
    });
    let trace_event = action.as_ref().map(|(order, shooter, damage, _)| {
        json!({
            "kind": "fire",
            "turn": turn,
            "player_id": player_id.0,
            "shooter": shooter.to_bits(),
            "expected_damage": damage,
            "target": order.target.to_bits(),
            "cell": [order.cell.x, order.cell.y],
            "entry": order.entry.to_bits(),
        })
    });
    let command = action.map(|(_, shooter, _, command)| (shooter, command));
    let mut registry = world.get_resource_or_insert_with(FireFollowOrderRegistry::default);
    registry.orders = orders;
    if let Some(event) = trace_event
        && registry.trace_enabled == Some(true)
    {
        registry.trace.push(event);
    }
    command
}

/// 敵の排除後に予約した進入役へ実命令を渡す。再占有・行動済み・到達不能なら解放する。
fn released_entry_action(
    world: &mut World,
    player_id: PlayerId,
    skip: &HashSet<Entity>,
) -> Option<(Entity, AiCommand)> {
    let turn = world
        .get_resource::<crate::resources::MatchState>()
        .map_or(0, |state| state.current_turn_number.0);
    let previous = world
        .get_resource::<FireFollowOrderRegistry>()
        .filter(|registry| registry.player == Some(player_id) && registry.turn == turn)?
        .orders
        .clone();
    if previous.is_empty() {
        return None;
    }
    let (movable, occupancy) =
        crate::ai::tactics::board::movable_units_and_occupancy(world, player_id, skip, None);
    for order in previous {
        if world.get::<GridPosition>(order.target).is_some()
            || occupancy.contains_key(&(order.cell.x, order.cell.y))
            || !movable.contains(&order.entry)
        {
            continue;
        }
        let Some(entry) = select_entry(
            world,
            player_id,
            &[order.entry],
            &occupancy,
            &HashSet::default(),
            &HashSet::default(),
            order.cell,
        ) else {
            continue;
        };
        let captures = world
            .get::<UnitStats>(entry)
            .is_some_and(|stats| stats.can_capture)
            && world.iter_entities().any(|entity| {
                entity.get::<GridPosition>() == Some(&order.cell)
                    && entity
                        .get::<crate::components::Property>()
                        .is_some_and(|property| property.owner_id != Some(player_id))
            });
        let command = if captures {
            AiCommand::Capture {
                target_pos: order.cell,
            }
        } else {
            AiCommand::Wait {
                target_pos: order.cell,
            }
        };
        if let Some(mut registry) = world.get_resource_mut::<FireFollowOrderRegistry>()
            && registry.trace_enabled == Some(true)
        {
            registry
                .trace
                .push(json!({"kind":"enter", "turn":turn, "player_id":player_id.0,
                "entry":entry.to_bits(), "cell":[order.cell.x,order.cell.y]}));
        }
        return Some((entry, command));
    }
    None
}

/// 盤面から今ステップの注文を作る。決定的に同じ盤面からは同じ注文になる。
pub(crate) fn observe(
    world: &mut World,
    player_id: PlayerId,
    skip: &HashSet<Entity>,
) -> Vec<FireFollowOrder> {
    let turn = world
        .get_resource::<crate::resources::MatchState>()
        .map_or(0, |state| state.current_turn_number.0);
    {
        // 手番・プレイヤーが変わったら追跡記録を捨てる。
        let mut registry = world.get_resource_or_insert_with(FireFollowOrderRegistry::default);
        if registry.trace_enabled.is_none() {
            registry.trace_enabled = Some(trace_enabled_from_env());
        }
        if registry.player != Some(player_id) || registry.turn != turn {
            registry.player = Some(player_id);
            registry.turn = turn;
            registry.orders.clear();
            registry.known.clear();
        }
    }
    let cells = blocked_route_cells(world, player_id);
    let mut orders = Vec::new();
    if !cells.is_empty() {
        let (movable, occupancy) =
            crate::ai::tactics::board::movable_units_and_occupancy(world, player_id, skip, None);
        // 射撃可能な敵集合は各部隊につき1回だけ観測する。
        let attackable = movable
            .iter()
            .filter_map(|&shooter| {
                let at = *world.get::<GridPosition>(shooter)?;
                Some((
                    shooter,
                    crate::systems::combat::get_attackable_targets_at(world, shooter, at, true),
                ))
            })
            .collect::<crate::collections::HashMap<_, _>>();
        // 1体は1注文にだけ使う（同じ手番に二度撃てない・二か所へ入れない）。
        let mut used: HashSet<Entity> = HashSet::default();
        for (target, cell) in cells {
            let Some(target_hp) = world.get::<Health>(target).map(|hp| hp.current) else {
                continue;
            };
            let Some(fire_tasks) =
                select_fire_tasks(world, &movable, &attackable, &used, target, target_hp)
            else {
                continue;
            };
            let tasks: HashSet<Entity> = fire_tasks.iter().map(|(entity, _)| *entity).collect();
            let Some(entry) =
                select_entry(world, player_id, &movable, &occupancy, &used, &tasks, cell)
            else {
                continue;
            };
            used.extend(tasks);
            used.insert(entry);
            orders.push(FireFollowOrder {
                target,
                cell,
                fire_tasks,
                entry,
            });
        }
    }
    record_trace(world, player_id, turn, &orders);
    orders
}

/// DAGの前進区間が占領・進入すべきセルのうち、敵地上unitが塞いでいるもの。
///
/// 対象はAdvanceの `capture_targets` と `execution_target` だけで、地域外の敵は扱わない。
/// 地域外の有利な射撃は従来どおり通常行動の採点に任せる。
fn blocked_route_cells(world: &World, player_id: PlayerId) -> Vec<(Entity, GridPosition)> {
    let Some(paths) = world.get_resource::<CapitalRoutePathRegistry>() else {
        return Vec::new();
    };
    let mut cells = std::collections::BTreeSet::new();
    for (squad_id, commitment) in &paths.commitments {
        if commitment.player_id != player_id
            || commitment.phase != CapitalRouteExecutionPhase::Advance
        {
            continue;
        }
        cells.extend(
            commitment
                .capture_targets
                .iter()
                .chain(std::iter::once(&commitment.execution_target))
                .map(|cell| (cell.y, cell.x)),
        );
        // 終点だけでなく、この小隊の最前から実行終点までの進路上の障害も排除する。
        // 別方面・後方の敵へ火力を奪われないよう、実際の小隊位置で区間を限定する。
        let squad = world
            .get_resource::<crate::ai::squad::SquadManager>()
            .and_then(|manager| manager.squads.iter().find(|squad| squad.id == *squad_id));
        let front = squad.and_then(|squad| {
            squad
                .members
                .iter()
                .filter_map(|entity| {
                    let at = world.get::<GridPosition>(*entity)?;
                    commitment.path.iter().position(|cell| cell == at)
                })
                .max()
        });
        let end = commitment
            .path
            .iter()
            .rposition(|cell| *cell == commitment.execution_target);
        if let (Some(front), Some(end)) = (front, end)
            && front < end
        {
            cells.extend(
                commitment.path[front + 1..=end]
                    .iter()
                    .map(|cell| (cell.y, cell.x)),
            );
        }
    }
    if cells.is_empty() {
        return Vec::new();
    }
    let mut blocked = world
        .iter_entities()
        .filter_map(|unit| {
            let at = *unit.get::<GridPosition>()?;
            let owner = unit.get::<Faction>()?.0;
            let stats = unit.get::<UnitStats>()?;
            (owner != player_id
                && unit.get::<Transporting>().is_none()
                && is_ground(stats)
                && cells.contains(&(at.y, at.x)))
            .then_some((unit.id(), at))
        })
        .collect::<Vec<_>>();
    // セル座標（行→列）の順で処理し、HashMapの走査順に依存しない。
    blocked.sort_unstable_by_key(|(entity, at)| (at.y, at.x, entity.to_bits()));
    blocked
}

fn is_ground(stats: &UnitStats) -> bool {
    !matches!(stats.movement_type, MovementType::Air | MovementType::Ship)
}

/// 現在位置から静止射撃で対象を撃てる未行動unitを、期待ダメージ降順（同値はEntity順）に
/// 足し上げ、合計が敵HP以上になる最小集合を返す。足りなければNone。
///
/// 期待ダメージは通常行動と同じ交換予測（`evaluate_attack_exchange`）をそのまま使う。
fn select_fire_tasks(
    world: &World,
    movable: &[Entity],
    attackable: &crate::collections::HashMap<Entity, Vec<Entity>>,
    used: &HashSet<Entity>,
    target: Entity,
    target_hp: u32,
) -> Option<Vec<(Entity, u32)>> {
    let mut shooters = Vec::new();
    for &shooter in movable {
        if used.contains(&shooter) {
            continue;
        }
        let Some(at) = world.get::<GridPosition>(shooter).copied() else {
            continue;
        };
        // 移動しない射撃なので間接攻撃も許す（移動後の間接射撃は不可のため移動射撃は扱わない）。
        if !attackable
            .get(&shooter)
            .is_some_and(|targets| targets.contains(&target))
        {
            continue;
        }
        let chart = world.get_resource::<crate::resources::DamageChart>()?;
        let damage =
            crate::ai::pruning::evaluate_attack_exchange(world, shooter, target, at, chart)
                .map_or(0, |exchange| exchange.expected_damage);
        if damage > 0 {
            shooters.push((shooter, damage));
        }
    }
    shooters
        .sort_unstable_by_key(|(entity, damage)| (std::cmp::Reverse(*damage), entity.to_bits()));
    let mut total = 0u32;
    let mut tasks = Vec::new();
    for (shooter, damage) in shooters {
        tasks.push((shooter, damage));
        total = total.saturating_add(damage);
        if total >= target_hp {
            return Some(tasks);
        }
    }
    None
}

/// 排除後のセルへ今手番に入れる1体を選ぶ。撃破担当とは重ならない。
///
/// 直接戦闘役（最小射程1以下で武器を持つ）か占領役だけを対象にし、敵を除いた占有で
/// 到達できるかを既存の到達判定で調べる。複数いれば近い順（同値はEntity順）。
fn select_entry(
    world: &World,
    player_id: PlayerId,
    movable: &[Entity],
    occupancy: &crate::collections::HashMap<(usize, usize), crate::systems::movement::OccupantInfo>,
    used: &HashSet<Entity>,
    fire_tasks: &HashSet<Entity>,
    cell: GridPosition,
) -> Option<Entity> {
    // 専用占領役の予約を、通過・封鎖のための進入役で奪わない。
    if crate::ai::v4::capital_route::tactical::is_capture_target_reserved(world, player_id, cell) {
        return None;
    }
    let map = world.get_resource::<Map>()?;
    let registry = world.get_resource::<MasterDataRegistry>()?;
    // 排除後の盤面: 対象の敵が居たセルを空ける。
    let mut cleared = occupancy.clone();
    cleared.remove(&(cell.x, cell.y));
    movable
        .iter()
        .copied()
        .filter(|entity| !used.contains(entity) && !fire_tasks.contains(entity))
        .filter_map(|entity| {
            let stats = world.get::<UnitStats>(entity)?;
            let at = *world.get::<GridPosition>(entity)?;
            let fuel = world.get::<crate::components::Fuel>(entity)?.current;
            let can_enter = is_ground(stats)
                && stats.min_range <= 1
                && (stats.max_range > 0 || stats.can_capture);
            if !can_enter {
                return None;
            }
            let reachable = crate::systems::movement::calculate_reachable_tiles(
                map,
                &cleared,
                (at.x, at.y),
                stats.movement_type,
                stats.max_movement,
                fuel,
                player_id,
                stats.unit_type,
                registry,
            );
            reachable
                .contains(&(cell.x, cell.y))
                .then(|| (map.distance(at.x, at.y, cell.x, cell.y), entity))
        })
        .min_by_key(|(distance, entity)| (*distance, entity.to_bits()))
        .map(|(_, entity)| entity)
}

/// 注文の作成・排除・破棄を診断イベントとして記録する（既定OFF）。
fn record_trace(world: &mut World, player_id: PlayerId, turn: u32, orders: &[FireFollowOrder]) {
    if world
        .get_resource::<FireFollowOrderRegistry>()
        .is_none_or(|registry| registry.trace_enabled != Some(true))
    {
        return;
    }
    let describe = |world: &World, entity: Entity| {
        json!({
            "id": entity.to_bits(),
            "unit_type": world.get::<UnitStats>(entity).map(|s| s.unit_type.as_str()),
            "at": world.get::<GridPosition>(entity).map(|p| [p.x, p.y]),
            "hp": world.get::<Health>(entity).map(|h| h.current),
        })
    };
    let known = world
        .get_resource::<FireFollowOrderRegistry>()
        .map(|registry| registry.known.clone())
        .unwrap_or_default();
    let mut events = Vec::new();
    let mut still_known = Vec::new();
    for (target, cell, entry) in known {
        if orders.iter().any(|order| order.target == target) {
            still_known.push((target, cell, entry));
            continue;
        }
        // 注文が消えた理由: 敵の消滅（排除）か、火力・進入役の不足（破棄）。
        let removed = world.get::<GridPosition>(target).is_none();
        let entry_acted = world
            .get_resource::<crate::ai::runtime::state::AiActionCooldown>()
            .is_some_and(|cooldown| cooldown.0.contains(&entry));
        events.push(json!({
            "kind": if removed { "released" } else { "dropped" },
            "turn": turn,
            "player_id": player_id.0,
            "target": target.to_bits(),
            "cell": [cell.x, cell.y],
            "entry": describe(world, entry),
            "entry_acted": entry_acted,
            "target_hp": world.get::<Health>(target).map(|h| h.current),
        }));
    }
    for order in orders {
        if still_known
            .iter()
            .any(|(target, _, _)| *target == order.target)
        {
            continue;
        }
        events.push(json!({
            "kind": "order",
            "turn": turn,
            "player_id": player_id.0,
            "target": describe(world, order.target),
            "cell": [order.cell.x, order.cell.y],
            "fire_tasks": order.fire_tasks.iter().map(|(entity, damage)| {
                let mut unit = describe(world, *entity);
                unit["expected_damage"] = json!(damage);
                unit
            }).collect::<Vec<_>>(),
            "entry": describe(world, order.entry),
        }));
        still_known.push((order.target, order.cell, order.entry));
    }
    if let Some(mut registry) = world.get_resource_mut::<FireFollowOrderRegistry>() {
        registry.known = still_known;
        registry.trace.extend(events);
    }
}
