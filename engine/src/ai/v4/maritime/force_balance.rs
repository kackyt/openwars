//! 揚陸作戦の火力と地上戦力の需給。戦艦と戦車のどちらを買うかを決める。
//!
//! 島の首都を落とすには、敵の地上戦力を削る火力と、拠点に立って守り・占領を支える地上
//! 戦力の両方が要る。
//!
//! - 戦艦は輸送の枠を使わず、反撃を受けない距離から敵を削れる。ただし港の生産枠
//!   （1港で1手番1隻）と補給の往復で稼働率が下がり、拠点を押さえることはできない。
//! - 戦車は輸送船の積載枠を使って運ぶ。上陸後は橋頭堡の保持、占領役の護衛、敵の
//!   生産口の占位を担う。
//!
//! 判断は次のとおり。
//!
//! 1. 作戦地域（揚陸先の浜・橋頭堡・前線・敵優勢のNodeの周り）の敵地上戦力 Z と、
//!    毎手番そこへ届く増援 F（敵の生産拠点の数、敵の収入で頭打ち）を見積もる。
//! 2. 戦艦が1手番に削る見込み N = 弾のある戦艦 × 稼働率 × 1発の期待損害価値。
//! 3. N < F + Z / ATTRITION_TURNS なら戦艦が要る（敵の補充を上回って守りを削れない）。
//!    数は、作戦地域を射程に捉える海上セル（持ち場）の数と、作戦地域の近くの港で
//!    回せる補給（1港で1手番1隻）の量で頭打ちにする。補給の回らない戦艦は撃てない。
//! 4. 戦艦で削り切れない残り（Z − N）が上陸済みの味方地上戦力 A を上回るなら、戦車が要る。
//!    運べない戦車は戦局に寄与しないので、輸送船の積載枠に空きがある分だけ買う。
//!    戦車が要る間は、積載枠の半分を戦闘役のために空け、占領役の待機をその残りまでにする
//!    （[`super::production::rejects_ground_purchase`]）。
//! 5. 揚陸する戦闘部隊の種類は、輸送船の積載枠1つ当たりの戦力で選ぶ。島の作戦では資金より
//!    積載枠が足りないため、費用当たりではなく1台の強さ（作戦地域の敵へ与える損害価値と、
//!    受ける損害の少なさ）で中戦車・重戦車・重自走砲から選ぶ。

use super::landmass::{Landmasses, is_ground};
use super::sealift::AssaultPlans;
use super::sealift::landing_site_strength as strength;
use super::theater::{NAVAL_GUN_MAX_RANGE, cells_within_ring, is_navigable};
use crate::ai::context::board::{BoardUnit, PlayerBoard};
use crate::collections::HashSet;
use crate::components::{GridPosition, PlayerId, UnitStats};
use crate::resources::{DamageChart, Terrain, UnitType};
use crate::systems::combat::get_expected_damage;
use bevy_ecs::prelude::*;

/// 作戦地域とみなす、支援地点からの距離。
const ZONE_RADIUS: u32 = 6;
/// 作戦地域の敵を削り切るまでの目安の手番数。
const ATTRITION_TURNS: i64 = 4;
/// 戦艦の稼働率（%）。6発ごとに港へ戻る往復で撃てない手番がある。
const BATTLESHIP_AVAILABILITY_PERCENT: i64 = 65;
/// 作戦地域へ届く敵生産拠点1つあたりの増援の見込み（生産費×HP）。
const REINFORCEMENT_PER_SITE: i64 = 7_000;
/// 戦艦1隻に要る持ち場の数（隣り合って射線を塞がない間隔）。
const STATIONS_PER_BATTLESHIP: usize = 2;
/// 戦艦の補給に使える港とみなす、作戦地域の支援地点からの距離。
const SUPPLY_PORT_RADIUS: u32 = 16;
/// 港1つで補給を回せる戦艦の数（1港で1手番1隻が補給し、1隻は6発を約4手番で撃つ）。
pub(crate) const BATTLESHIPS_PER_PORT: usize = 4;
/// 揚陸する戦闘部隊の候補（積載枠1つに見合う強さのもの）。
const LANDING_COMBAT_CANDIDATES: [UnitType; 3] =
    [UnitType::MdTank, UnitType::TankZ, UnitType::HeavySpGun];

/// 揚陸作戦の火力と地上戦力の需給。
#[derive(Debug, Clone, Default)]
pub(crate) struct LandingForceBalance {
    /// 作戦地域の敵地上戦力（生産費×HP）。
    pub(crate) zone_enemy: i64,
    /// 毎手番、作戦地域へ届く敵の増援の見込み。
    pub(crate) enemy_flow: i64,
    /// 戦艦が1手番に削る見込み。
    pub(crate) naval_capacity: i64,
    /// 作戦地域に上陸済みの味方地上戦力。
    pub(crate) ashore: i64,
    /// 戦艦の数と、持ち場から決まる上限。
    pub(crate) battleships: u32,
    pub(crate) battleship_limit: u32,
    /// 作戦地域の敵（戦車の種類の評価に使う）。
    zone_enemies: Vec<BoardUnit>,
}

impl LandingForceBalance {
    /// 盤面から揚陸作戦の需給を見積もる。揚陸作戦が無ければNone。
    pub(crate) fn observe(
        world: &World,
        board: &PlayerBoard,
        landmasses: &Landmasses,
        player_id: PlayerId,
    ) -> Option<Self> {
        let plan = world.get_resource::<AssaultPlans>()?.0.get(&player_id)?;
        let map = &board.map;
        let mut anchors: Vec<GridPosition> = plan
            .stops
            .iter()
            .map(|stop| GridPosition {
                x: stop.0,
                y: stop.1,
            })
            .collect();
        anchors.extend(crate::ai::v4::capital_route::beachhead_support_anchors(
            world, player_id,
        ));
        let in_zone = |position: GridPosition| {
            landmasses.land.at(position) == Some(plan.landmass)
                && anchors.iter().any(|anchor| {
                    map.distance(anchor.x, anchor.y, position.x, position.y) <= ZONE_RADIUS
                })
        };
        let ground: Vec<&BoardUnit> = board
            .units
            .iter()
            .filter(|unit| unit.is_deployed() && is_ground(unit) && in_zone(unit.position))
            .collect();
        let zone_enemies: Vec<BoardUnit> = ground
            .iter()
            .filter(|unit| unit.owner != player_id)
            .map(|unit| (*unit).clone())
            .collect();
        let zone_enemy = zone_enemies.iter().map(strength).sum();
        let ashore = ground
            .iter()
            .filter(|unit| unit.owner == player_id)
            .map(|unit| strength(unit))
            .sum();
        // 増援: 上陸先の陸塊にある敵の生産拠点の数。敵の収入を超えては作れない。
        let enemy_sites = board
            .properties
            .iter()
            .filter(|property| {
                matches!(property.terrain, Terrain::Factory | Terrain::Capital)
                    && property.owner.is_some_and(|owner| owner != player_id)
                    && landmasses.land.at(property.position) == Some(plan.landmass)
            })
            .count() as i64;
        let enemy_income: i64 = board
            .properties
            .iter()
            .filter(|property| property.owner.is_some_and(|owner| owner != player_id))
            .map(|property| i64::from(board.registry.landscape_income(property.terrain.as_str())))
            .sum();
        let enemy_flow = (enemy_sites * REINFORCEMENT_PER_SITE).min(enemy_income);

        // 戦艦の削り量: 作戦地域の敵に対する1発の期待損害価値の平均。
        let chart = world
            .get_resource::<DamageChart>()
            .cloned()
            .unwrap_or_default();
        let own_battleships: Vec<&BoardUnit> = board
            .units
            .iter()
            .filter(|unit| {
                unit.owner == player_id
                    && unit.is_deployed()
                    && super::naval_combat::is_warship(&unit.stats)
            })
            .collect();
        let armed = own_battleships
            .iter()
            .filter(|unit| unit.ammo.is_none_or(|(ammo1, _)| ammo1 > 0))
            .count() as i64;
        let shot_value = own_battleships
            .first()
            .map(|battleship| {
                average_damage_value(board, &chart, &battleship.stats, &zone_enemies, 3)
            })
            .unwrap_or(REINFORCEMENT_PER_SITE / 2);
        let naval_capacity = armed * shot_value * BATTLESHIP_AVAILABILITY_PERCENT / 100;

        // 持ち場: 作戦地域の敵（居なければ支援地点）を射程に捉える海上セル。
        let mut stations = HashSet::default();
        let targets: Vec<GridPosition> = if zone_enemies.is_empty() {
            anchors.clone()
        } else {
            zone_enemies.iter().map(|unit| unit.position).collect()
        };
        for target in targets {
            stations.extend(
                cells_within_ring(map, target, 2, NAVAL_GUN_MAX_RANGE)
                    .filter(|cell| is_navigable(board, cell.0, cell.1)),
            );
        }
        // 補給: 作戦地域の近くにある自軍の港。
        let supply_ports = board
            .properties
            .iter()
            .filter(|property| {
                property.owner == Some(player_id)
                    && property.terrain == Terrain::Port
                    && anchors.iter().any(|anchor| {
                        map.distance(anchor.x, anchor.y, property.position.x, property.position.y)
                            <= SUPPLY_PORT_RADIUS
                    })
            })
            .count();
        let battleship_limit = (stations.len() / STATIONS_PER_BATTLESHIP)
            .min(supply_ports.max(1) * BATTLESHIPS_PER_PORT) as u32;
        Some(Self {
            zone_enemy,
            enemy_flow,
            naval_capacity,
            ashore,
            battleships: own_battleships.len() as u32,
            battleship_limit,
            zone_enemies,
        })
    }

    /// 戦艦が要るか（敵の補充を上回って作戦地域の守りを削る火力が足りない）。
    pub(crate) fn battleship_needed(&self) -> bool {
        self.battleships < self.battleship_limit
            && self.naval_capacity < self.enemy_flow + self.zone_enemy / ATTRITION_TURNS
    }

    /// 戦車が要るか（戦艦で削り切れない敵が、上陸済みの味方を上回る）。
    pub(crate) fn ground_combat_needed(&self) -> bool {
        self.zone_enemy - self.naval_capacity > self.ashore
    }

    /// 作戦地域の敵に対して、輸送船の積載枠1つ当たりの戦力が最も高い戦闘部隊（`budget` 以内）。
    ///
    /// 戦力は「与える損害価値 × 100 ÷（100 ＋ 受ける損害%）」。間接部隊は射程の内側から、
    /// 直接部隊は隣接から撃つとして見積もる。
    pub(crate) fn best_landing_combat_unit(
        &self,
        board: &PlayerBoard,
        chart: &DamageChart,
        stats_of: &dyn Fn(UnitType) -> Option<UnitStats>,
        budget: u32,
    ) -> Option<UnitType> {
        LANDING_COMBAT_CANDIDATES
            .iter()
            .filter_map(|unit_type| {
                let stats = stats_of(*unit_type)?;
                if stats.cost > budget {
                    return None;
                }
                let distance = stats.min_range.max(1);
                let dealt =
                    average_damage_value(board, chart, &stats, &self.zone_enemies, distance);
                let taken_value = average_damage_taken(board, chart, &stats, &self.zone_enemies);
                let taken_percent = taken_value * 100 / i64::from(stats.cost).max(1);
                let score = dealt * 100 / (100 + taken_percent);
                Some((score, *unit_type))
            })
            .max_by_key(|(score, unit_type)| (*score, std::cmp::Reverse(*unit_type as u32)))
            .map(|(_, unit_type)| unit_type)
    }
}

/// 攻撃側が敵の各部隊へ距離 `distance` から与える期待損害価値の平均（敵が居なければ0）。
fn average_damage_value(
    board: &PlayerBoard,
    chart: &DamageChart,
    attacker: &UnitStats,
    enemies: &[BoardUnit],
    distance: u32,
) -> i64 {
    if enemies.is_empty() {
        return 0;
    }
    let total: i64 = enemies
        .iter()
        .map(|enemy| {
            let damage = get_expected_damage(
                attacker,
                100,
                (attacker.max_ammo1, attacker.max_ammo2),
                &enemy.stats,
                terrain_defense(board, enemy.position),
                distance,
                &board.registry,
                chart,
                false,
            )
            .min(enemy.health.unwrap_or(100));
            i64::from(enemy.stats.cost) * i64::from(damage) / 100
        })
        .sum();
    total / enemies.len() as i64
}

/// 敵の各部隊から隣接で受ける期待損害価値の平均（間接部隊は射程で撃つ）。
fn average_damage_taken(
    board: &PlayerBoard,
    chart: &DamageChart,
    defender: &UnitStats,
    enemies: &[BoardUnit],
) -> i64 {
    if enemies.is_empty() {
        return 0;
    }
    let total: i64 = enemies
        .iter()
        .map(|enemy| {
            let distance = enemy.stats.min_range.max(1);
            let damage = get_expected_damage(
                &enemy.stats,
                enemy.health.unwrap_or(100),
                enemy
                    .ammo
                    .unwrap_or((enemy.stats.max_ammo1, enemy.stats.max_ammo2)),
                defender,
                0,
                distance,
                &board.registry,
                chart,
                false,
            )
            .min(100);
            i64::from(defender.cost) * i64::from(damage) / 100
        })
        .sum();
    total / enemies.len() as i64
}

fn terrain_defense(board: &PlayerBoard, position: GridPosition) -> u32 {
    board
        .map
        .get_terrain(position.x, position.y)
        .map(|terrain| board.registry.get_terrain_defense_bonus(terrain))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn balance(
        zone_enemy: i64,
        naval_capacity: i64,
        ashore: i64,
        battleships: u32,
    ) -> LandingForceBalance {
        LandingForceBalance {
            zone_enemy,
            enemy_flow: 14_000,
            naval_capacity,
            ashore,
            battleships,
            battleship_limit: 6,
            zone_enemies: Vec::new(),
        }
    }

    #[test]
    fn battleships_are_bought_until_they_outpace_enemy_reinforcement() {
        // 削り量が増援＋守り/4に届かない間は戦艦が要る。
        assert!(balance(40_000, 10_000, 0, 2).battleship_needed());
        assert!(!balance(40_000, 30_000, 0, 4).battleship_needed());
        // 持ち場が埋まるだけ居れば、それ以上は要らない。
        assert!(!balance(40_000, 10_000, 0, 6).battleship_needed());
    }

    #[test]
    fn tanks_are_bought_only_for_what_the_battleships_cannot_wear_down() {
        // 戦艦の削り残し（40,000−10,000）が上陸済みの味方を上回る。
        assert!(balance(40_000, 10_000, 20_000, 2).ground_combat_needed());
        // 味方が削り残しを受け止められるなら要らない。
        assert!(!balance(40_000, 10_000, 30_000, 2).ground_combat_needed());
        assert!(!balance(10_000, 12_000, 0, 2).ground_combat_needed());
    }
}
