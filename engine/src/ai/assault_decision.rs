//! 敵首都への全体前進を開始するか（GO／WAIT）を、盤面だけから判定する。
//!
//! 判定の考え方:
//! - 「敵首都の生産圏へ入る」か「生産圏を塞いでいる敵を攻撃できる位置へ着く」ことを、
//!   首都攻略への関与とみなす。敵が生産圏を塞いでいても、その敵を攻撃しに行ける部隊は数える。
//! - 部隊は兵種で重み付けして数える（歩兵を含む直接戦闘部隊1体、間接兵器0.2体）。
//! - 2自軍手番以内の関与戦力が前衛の基準以上、かつ3自軍手番以内の関与戦力が後続込みの基準以上で GO。
//! - 一度 GO にしたら、3自軍手番以内に関与できる部隊が直接・間接とも1体もいなくなったときだけ WAIT に戻す。
//!
//! 先読み対戦は行わず、地形・占有・ZOC・燃料を考慮した行動ターン距離だけを使う。
use crate::ai::turn_distance::{ActionTurnDistanceCache, calculate_action_distance_to_range};
use crate::collections::HashMap;
use crate::components::*;
use crate::resources::master_data::MasterDataRegistry;
use crate::resources::*;
use crate::systems::movement::OccupantInfo;
use crate::systems::production::PRODUCTION_RANGE;
use bevy_ecs::prelude::*;
use serde::Serialize;

/// 戦力は 0.1体単位の整数で扱う（小数の比較誤差を避ける）。
pub const UNIT_SCALE: u32 = 10;
/// 歩兵系以外の直接戦闘部隊の重み（1体）。
pub const DIRECT_WEIGHT: u32 = 10;
/// 歩兵系（占領できる部隊）の重み（1体）。
pub const INFANTRY_WEIGHT: u32 = 10;
/// 間接兵器の重み（0.2体）。
pub const INDIRECT_WEIGHT: u32 = 2;
/// GO を出す前衛戦力（2自軍手番以内、3体）。
pub const GO_FRONT: u32 = 3 * UNIT_SCALE;
/// GO を出す後続込みの戦力（3自軍手番以内、4体。1体失っても後続が引き継げる）。
pub const GO_SUSTAIN: u32 = 4 * UNIT_SCALE;
/// 前衛の到達期限（自軍手番数）。
pub const FRONT_TURNS: u32 = 2;
/// 後続の到達期限（自軍手番数）。
pub const SUSTAIN_TURNS: u32 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum AssaultDecision {
    Go,
    Wait,
}

/// 1部隊が首都攻略へ関与するまでの見積もり。
#[derive(Debug, Clone, Serialize)]
pub struct Engagement {
    pub unit_id: u64,
    pub unit_type: String,
    pub position: (usize, usize),
    /// 戦力としての重み（0.1体単位）。
    pub weight: u32,
    /// 関与できるまでの自軍手番数。0なら現在位置から関与できる。
    pub turns: u32,
    /// 生産圏へ入るのか、生産圏内の敵を攻撃するのか。
    pub mode: &'static str,
    /// 攻撃する場合の敵の位置。
    pub target: Option<(usize, usize)>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AssaultReport {
    pub decision: AssaultDecision,
    pub objective: (usize, usize),
    /// 2自軍手番以内に関与できる戦力（0.1体単位）。
    pub front_strength: u32,
    /// 3自軍手番以内に関与できる戦力（0.1体単位）。
    pub sustain_strength: u32,
    /// 生産圏内にいる敵部隊数。
    pub defenders_in_zone: usize,
    pub engagements: Vec<Engagement>,
}

/// 盤面上の地上部隊の占有表を作る。積載中の部隊は盤面を占有しない。
fn occupancy(world: &mut World) -> HashMap<(usize, usize), OccupantInfo> {
    let mut query = world.query_filtered::<(
        &GridPosition,
        &Faction,
        &UnitStats,
        Option<&CargoCapacity>,
    ), Without<Transporting>>();
    query
        .iter(world)
        .map(|(position, faction, stats, cargo)| {
            (
                (position.x, position.y),
                OccupantInfo {
                    player_id: faction.0,
                    is_transport: stats.max_cargo > 0,
                    unit_type: stats.unit_type,
                    loadable_types: stats.loadable_unit_types.clone(),
                    free_slots: cargo.map_or(stats.max_cargo, |c| {
                        c.max.saturating_sub(c.loaded.len() as u32)
                    }),
                },
            )
        })
        .collect()
}

/// 攻撃側が防御側へ損害を与えられるか（主・副武装のいずれか）。
fn can_damage(chart: &DamageChart, attacker: UnitType, defender: UnitType) -> bool {
    chart.get_base_damage(attacker, defender).unwrap_or(0) > 0
        || chart
            .get_base_damage_secondary(attacker, defender)
            .unwrap_or(0)
            > 0
}

/// 前回の判定と今回の戦力から GO／WAIT を決める。
/// GO の開始と維持で基準を分け、戦力の増減で判定が往復しないようにする。
/// GO 中は、3自軍手番以内に関与できる部隊（直接・間接とも）が残る限り攻勢を続ける。
pub fn decide(previous: AssaultDecision, front: u32, sustain: u32) -> AssaultDecision {
    let go = match previous {
        AssaultDecision::Wait => front >= GO_FRONT && sustain >= GO_SUSTAIN,
        AssaultDecision::Go => sustain > 0,
    };
    if go {
        AssaultDecision::Go
    } else {
        AssaultDecision::Wait
    }
}

/// 兵種ごとの戦力の重み。輸送・補給など攻撃できない部隊は数えない。
fn unit_weight(stats: &UnitStats) -> Option<u32> {
    if stats.max_range == 0 {
        None
    } else if stats.min_range > 1 {
        Some(INDIRECT_WEIGHT)
    } else if stats.can_capture {
        Some(INFANTRY_WEIGHT)
    } else {
        Some(DIRECT_WEIGHT)
    }
}

/// 指定勢力が敵首都への全体前進を開始すべきかを判定する。盤面は変更しない。
pub fn judge_assault(
    world: &mut World,
    player: PlayerId,
    objective: GridPosition,
    previous: AssaultDecision,
) -> AssaultReport {
    let occupied = occupancy(world);
    let map = world.resource::<Map>();
    let registry = world.resource::<MasterDataRegistry>();
    let chart = world.resource::<DamageChart>();
    let zone = PRODUCTION_RANGE as u32;

    // 生産圏内の敵部隊。これらは首都攻略を塞ぐ障害として攻撃対象になる。
    let mut units = Vec::new();
    for entity in world.iter_entities() {
        if entity.get::<Transporting>().is_some() {
            continue;
        }
        let (Some(position), Some(faction), Some(stats), Some(id)) = (
            entity.get::<GridPosition>(),
            entity.get::<Faction>(),
            entity.get::<UnitStats>(),
            entity.get::<UnitId>(),
        ) else {
            continue;
        };
        let fuel = entity.get::<Fuel>().map_or(0, |f| f.current);
        units.push((*position, faction.0, stats, id.0.as_u128() as u64, fuel));
    }
    let defenders = units
        .iter()
        .filter(|(p, owner, ..)| {
            *owner != player && map.distance(p.x, p.y, objective.x, objective.y) <= zone
        })
        .map(|(p, _, stats, ..)| (*p, stats.unit_type))
        .collect::<Vec<_>>();

    let mut cache = ActionTurnDistanceCache::default();
    let mut engagements = Vec::new();
    // 診断用: 関与できるまでの手番数の分布（0,1,2,3,4-5,6-8,9以上,到達不能）。既定では無効。
    let distance_diag = std::env::var_os("OPENWARS_GO_DIAG").is_some();
    let mut distance_buckets = [0_u32; 8];
    // 到達不能の内訳（間接兵器・燃料不足・敵の占有やZOCに塞がれる・地形だけで届かない）。
    let mut unreachable_kinds = [0_u32; 4];
    for (position, owner, stats, id, fuel) in &units {
        if *owner != player {
            continue;
        }
        let Some(weight) = unit_weight(stats) else {
            continue;
        };
        let indirect = stats.min_range > 1;
        let start = (position.x, position.y);
        // 生産圏へ入る（空きマスに限る）までの手番数。間接兵器は射撃で関与するため数えない。
        let mut best = if indirect {
            None
        } else {
            calculate_action_distance_to_range(
                map,
                registry,
                &occupied,
                start,
                (objective.x, objective.y),
                stats.movement_type,
                stats.max_movement,
                *fuel,
                0,
                zone,
                player,
                &mut cache,
            )
            .map(|d| (d.turns, "enter_zone", None))
        };
        // 生産圏を塞ぐ敵を攻撃できる位置までの手番数。
        for (target, target_type) in &defenders {
            if !can_damage(chart, stats.unit_type, *target_type) {
                continue;
            }
            let Some(distance) = calculate_action_distance_to_range(
                map,
                registry,
                &occupied,
                start,
                (target.x, target.y),
                stats.movement_type,
                stats.max_movement,
                *fuel,
                stats.min_range.max(1),
                stats.max_range,
                player,
                &mut cache,
            ) else {
                continue;
            };
            if best.is_none_or(|(turns, ..)| distance.turns < turns) {
                best = Some((distance.turns, "attack_blocker", Some((target.x, target.y))));
            }
        }
        if distance_diag {
            let bucket = match best {
                None => 7,
                Some((turns, ..)) => match turns {
                    0..=3 => turns as usize,
                    4..=5 => 4,
                    6..=8 => 5,
                    _ => 6,
                },
            };
            distance_buckets[bucket] += 1;
            if best.is_none() {
                let kind = if indirect {
                    0
                } else {
                    // 診断用の別cacheで、燃料と占有を外した距離を測り直して理由を切り分ける。
                    let mut probe_cache = ActionTurnDistanceCache::default();
                    let reaches =
                        |occupancy: &HashMap<(usize, usize), OccupantInfo>,
                         fuel: u32,
                         cache: &mut ActionTurnDistanceCache| {
                            calculate_action_distance_to_range(
                                map,
                                registry,
                                occupancy,
                                start,
                                (objective.x, objective.y),
                                stats.movement_type,
                                stats.max_movement,
                                fuel,
                                0,
                                zone,
                                player,
                                cache,
                            )
                            .is_some()
                        };
                    if reaches(&occupied, 1000, &mut probe_cache) {
                        1
                    } else if reaches(&HashMap::default(), 1000, &mut probe_cache) {
                        2
                    } else {
                        3
                    }
                };
                unreachable_kinds[kind] += 1;
            }
        }
        if let Some((turns, mode, target)) = best
            && turns <= SUSTAIN_TURNS
        {
            engagements.push(Engagement {
                unit_id: *id,
                unit_type: stats.unit_type.as_str().to_owned(),
                position: start,
                weight,
                turns,
                mode,
                target,
            });
        }
    }
    if distance_diag {
        eprintln!(
            "GO-DIST P{player:?} buckets(0,1,2,3,4-5,6-8,9+,unreachable)={distance_buckets:?} unreachable_kinds(indirect,fuel,blocked,terrain)={unreachable_kinds:?}"
        );
    }
    engagements.sort_by_key(|e| (e.turns, e.unit_id));
    let front_strength = engagements
        .iter()
        .filter(|e| e.turns <= FRONT_TURNS)
        .map(|e| e.weight)
        .sum();
    let sustain_strength = engagements.iter().map(|e| e.weight).sum();
    AssaultReport {
        decision: decide(previous, front_strength, sustain_strength),
        objective: (objective.x, objective.y),
        front_strength,
        sustain_strength,
        defenders_in_zone: defenders.len(),
        engagements,
    }
}

/// 勢力ごとの攻勢判定。前回の判定を次の手番へ引き継ぐ。
#[derive(Resource, Debug, Default)]
pub struct AssaultPostures {
    by_player: HashMap<PlayerId, AssaultPosture>,
}

#[derive(Debug, Clone, Copy)]
struct AssaultPosture {
    turn: u32,
    decision: AssaultDecision,
    objective: GridPosition,
}

/// 指定勢力から見た敵首都（座標順で最初のもの）。
pub(crate) fn enemy_capital(world: &World, player: PlayerId) -> Option<GridPosition> {
    world
        .iter_entities()
        .filter_map(|entity| {
            let property = entity.get::<Property>()?;
            (property.terrain == Terrain::Capital
                && property.owner_id.is_some_and(|owner| owner != player))
            .then(|| entity.get::<GridPosition>().copied())
            .flatten()
        })
        .min_by_key(|position| (position.y, position.x))
}

/// 手番の最初に1回だけ攻勢判定を更新する。同じ手番の2回目以降は何もしない。
pub fn refresh_assault_posture(world: &mut World, player: PlayerId, turn: u32) {
    world.init_resource::<AssaultPostures>();
    let previous = world
        .resource::<AssaultPostures>()
        .by_player
        .get(&player)
        .copied();
    if previous.is_some_and(|posture| posture.turn == turn) {
        return;
    }
    let Some(objective) = enemy_capital(world, player) else {
        world
            .resource_mut::<AssaultPostures>()
            .by_player
            .remove(&player);
        return;
    };
    // 目標の首都が変わったら、前回の判定は引き継がない。
    let previous_decision = previous
        .filter(|posture| posture.objective == objective)
        .map_or(AssaultDecision::Wait, |posture| posture.decision);
    let report = judge_assault(world, player, objective, previous_decision);
    if std::env::var_os("OPENWARS_GO_DIAG").is_some() {
        // 攻勢判定の内訳を記録する診断（既定では無効）。距離別の関与数で、GOに届かない理由を見る。
        let by_turns = |limit: u32| {
            report
                .engagements
                .iter()
                .filter(|e| e.turns <= limit)
                .count()
        };
        let modes = report
            .engagements
            .iter()
            .fold((0, 0), |(enter, attack), e| match e.mode {
                "enter_zone" => (enter + 1, attack),
                _ => (enter, attack + 1),
            });
        eprintln!(
            "GO-DIAG T{turn} P{player:?} decision={:?} front={} sustain={} defenders_in_zone={} engaged<=2={} engaged<=3={} enter_zone={} attack_blocker={}",
            report.decision,
            report.front_strength,
            report.sustain_strength,
            report.defenders_in_zone,
            by_turns(FRONT_TURNS),
            by_turns(SUSTAIN_TURNS),
            modes.0,
            modes.1,
        );
    }
    world.resource_mut::<AssaultPostures>().by_player.insert(
        player,
        AssaultPosture {
            turn,
            decision: report.decision,
            objective,
        },
    );
}

/// 攻勢中（GO）なら、その目標の敵首都を返す。
pub fn assault_objective(world: &World, player: PlayerId) -> Option<GridPosition> {
    world
        .get_resource::<AssaultPostures>()?
        .by_player
        .get(&player)
        .filter(|posture| posture.decision == AssaultDecision::Go)
        .map(|posture| posture.objective)
}

#[cfg(test)]
mod tests {
    use super::*;
    use AssaultDecision::{Go, Wait};

    #[test]
    fn 前衛3体と後続1体が揃えばgoを出す() {
        assert_eq!(decide(Wait, 30, 40), Go);
    }

    #[test]
    fn 待機中は前衛が3体未満ならwaitのまま() {
        assert_eq!(decide(Wait, 29, 60), Wait);
    }

    #[test]
    fn 待機中は後続がいなければwaitのまま() {
        assert_eq!(decide(Wait, 30, 30), Wait);
    }

    #[test]
    fn go中は間接兵器1基でも関与できればgoを維持する() {
        assert_eq!(decide(Go, 0, INDIRECT_WEIGHT), Go);
    }

    #[test]
    fn go中に関与できる部隊が1体もいなければwaitへ戻す() {
        assert_eq!(decide(Go, 0, 0), Wait);
    }
}
