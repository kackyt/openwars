//! 陸塊（地上部隊が陸路で行き来できるセルのまとまり）ごとの作戦需要。
//!
//! 島で隔てられた盤面では、陸塊ごとに「まだ獲る拠点があるか」「敵が居るか」が
//! 異なる。仕事の無い陸塊に居る地上部隊（例: 全拠点を確保済みの本拠島）は、
//! 輸送しない限り戦局に寄与しない。これを「取り残された部隊」と呼ぶ。
//!
//! 首都同士が歩兵しか通れない隘路（山道など）でしか陸続きでない盤面では、主力の
//! 車両がその隘路を通れず、歩兵も山道を長く歩くより輸送船で渡る方が早い。この場合は
//! 車両の通行可否で陸塊を塗り分け、海で隔てられた陸塊と同じく揚陸作戦の対象にする
//! （[`label_landmasses`]）。

use super::front::NarrowFront;
use super::theater::Components;
use crate::ai::context::board::{BoardUnit, PlayerBoard};
use crate::collections::{HashMap, HashSet};
use crate::components::{GridPosition, PlayerId};
use crate::resources::{MovementType, Terrain};
use bevy_ecs::entity::Entity;
use std::cmp::Reverse;
use std::collections::BinaryHeap;

/// 本拠へ戦闘役を送り込むのを止める、敵の上陸部隊に対する自軍の戦力の比（%）。
const HOME_DEFENSE_PERCENT: i64 = 150;

/// 首都同士を結ぶ歩兵しか通れない区間を、歩兵の陸路として扱う移動コストの上限。
/// 軽歩兵の移動力（3）の2手番分。これを超える山道は、輸送船で渡る方が早い。
const FOOT_PASS_MAX_COST: u32 = 6;

/// 1つの陸塊の事実。
#[derive(Debug, Clone, Default)]
pub(crate) struct LandmassInfo {
    /// 自軍が所有していない拠点（中立・敵）の座標。
    pub(crate) unowned: Vec<GridPosition>,
    /// 敵が所有する拠点の数。
    pub(crate) enemy_owned: u32,
    /// 敵首都があるか。
    pub(crate) enemy_capital: bool,
    /// 陸塊上の敵地上部隊の数。
    pub(crate) enemy_ground: u32,
    /// 陸塊上の自軍の占領可能な部隊の数。
    pub(crate) own_capturers: u32,
    /// 陸塊上の自軍の地上部隊の数。
    pub(crate) own_ground: u32,
    /// 自軍の首都があるか。
    pub(crate) own_capital: bool,
    /// 陸塊上の自軍の地上部隊の戦力（生産費×HP）。
    pub(crate) own_strength: i64,
    /// 陸塊上の敵の地上部隊の戦力（生産費×HP）。
    pub(crate) enemy_strength: i64,
}

impl LandmassInfo {
    /// 占領役をまだ受け入れられる数。
    pub(crate) fn capture_need(&self) -> u32 {
        (self.unowned.len() as u32).saturating_sub(self.own_capturers)
    }

    /// 戦闘役を送る理由（敵部隊・敵拠点）があるか。
    pub(crate) fn needs_combat(&self) -> bool {
        self.enemy_ground > 0 || self.enemy_owned > 0
    }

    /// 輸送で戦闘役を送り込む価値があるか。
    ///
    /// 自軍の首都の陸塊（本拠）は、そこに居る自軍の戦力が敵の上陸部隊を
    /// [`HOME_DEFENSE_PERCENT`] まで上回る間は送らない。少数の敵が本拠へ上陸するたびに、
    /// 積んだ部隊を本拠へ降ろし直して往復すると、敵の本土への揚陸が進まない。
    pub(crate) fn wants_combat(&self) -> bool {
        self.needs_combat()
            && !(self.own_capital
                && self.own_strength * 100 >= self.enemy_strength * HOME_DEFENSE_PERCENT)
    }
}

/// 陸塊の塗り分けと、陸塊ごとの事実。
pub(crate) struct Landmasses {
    pub(crate) land: Components,
    pub(crate) info: HashMap<u32, LandmassInfo>,
    /// 自軍の首都と敵の首都を結ぶ隘路（[`super::front`]）。隘路で陸塊を分けた盤面だけ。
    pub(crate) front: Option<NarrowFront>,
    /// 隘路を押し進む陸路の部隊。本拠に居ても取り残された部隊として扱わない。
    pub(crate) land_force: HashSet<Entity>,
    /// 陸路の部隊の割当数に対する不足。
    pub(crate) land_force_shortfall: usize,
}

/// 地上を移動する部隊か（航空・艦船を除く）。
pub(crate) fn is_ground(unit: &BoardUnit) -> bool {
    !matches!(
        unit.stats.movement_type,
        MovementType::Air | MovementType::Ship
    )
}

/// 陸塊を塗り分ける。
///
/// 通常は歩兵の通行可否で塗り分ける。別の勢力の首都同士が歩兵の陸路でつながるのに
/// 戦車の陸路でつながらず、その間の歩兵しか通れない区間（山道など）を歩兵が抜けるのに
/// [`FOOT_PASS_MAX_COST`] を超える移動コストがかかる盤面では、戦車の通行可否で塗り分ける。
/// 短い峠（山1〜2マス）なら歩兵が陸路で攻め込める方が早いので、歩兵の陸塊のままにする。
///
/// 首都同士を結ぶ戦車の陸路が細い隘路だけの盤面は、[`super::front`] が隘路で分ける。
pub(crate) fn label_landmasses(board: &PlayerBoard) -> Components {
    let foot = Components::label(board, MovementType::Infantry);
    let capitals: Vec<(PlayerId, GridPosition)> = board
        .properties
        .iter()
        .filter(|property| property.terrain == Terrain::Capital)
        .filter_map(|property| Some((property.owner?, property.position)))
        .collect();
    // 歩兵の陸路でつながる、別の勢力の首都の組。
    let linked_by_foot: Vec<(GridPosition, GridPosition)> = capitals
        .iter()
        .enumerate()
        .flat_map(|(index, (owner, position))| {
            capitals[index + 1..]
                .iter()
                .filter(move |(other, _)| other != owner)
                .map(move |(_, other)| (*position, *other))
        })
        .filter(|(a, b)| foot.at(*a).is_some() && foot.at(*a) == foot.at(*b))
        .collect();
    if linked_by_foot.is_empty() {
        return foot;
    }
    let vehicle = Components::label(board, MovementType::Tank);
    let long_pass = linked_by_foot.iter().any(|(a, b)| {
        vehicle.at(*a) != vehicle.at(*b)
            && foot_pass_cost(board, &vehicle, *a, *b).is_some_and(|cost| cost > FOOT_PASS_MAX_COST)
    });
    if long_pass {
        return vehicle;
    }
    // 戦車の陸路が細い隘路だけなら、隘路を外して両側を別の陸塊にする。
    super::front::split_narrow_fronts(board, foot, &linked_by_foot)
}

/// `from` から `to` へ歩兵が歩くときの、戦車の通れないセルでかかる移動コストの最小値。
///
/// 戦車の通れるセルの移動コストは数えない（車両と同じく陸路で進める区間）。
fn foot_pass_cost(
    board: &PlayerBoard,
    vehicle: &Components,
    from: GridPosition,
    to: GridPosition,
) -> Option<u32> {
    let map = &board.map;
    let foot_cost = |x: usize, y: usize| {
        map.get_terrain(x, y)
            .and_then(|terrain| {
                board
                    .registry
                    .get_movement_cost(MovementType::Infantry, terrain.as_str())
            })
            .filter(|cost| *cost < 99)
    };
    // 0のコストの辺を含むため、ダイクストラ法で求める。
    let mut best: HashMap<(usize, usize), u32> = HashMap::default();
    let mut queue = BinaryHeap::from([Reverse((0_u32, (from.x, from.y)))]);
    best.insert((from.x, from.y), 0);
    while let Some(Reverse((cost, (x, y)))) = queue.pop() {
        if (x, y) == (to.x, to.y) {
            return Some(cost);
        }
        if best.get(&(x, y)).is_some_and(|known| *known < cost) {
            continue;
        }
        for (nx, ny) in map.get_adjacent(x, y) {
            let Some(step) = foot_cost(nx, ny) else {
                continue;
            };
            let by_vehicle = vehicle.at(GridPosition { x: nx, y: ny }).is_some();
            let next = cost + if by_vehicle { 0 } else { step };
            if best.get(&(nx, ny)).is_none_or(|known| next < *known) {
                best.insert((nx, ny), next);
                queue.push(Reverse((next, (nx, ny))));
            }
        }
    }
    None
}

/// 隘路を押し進む陸路の部隊と、その割当数に対する不足。
///
/// 隘路に居る自軍の地上部隊は陸路の部隊。残りの枠は本拠側の地上部隊から、間接射撃の部隊
/// （移動後に撃てず、揚陸しても浜で戦えない）、隘路に近い部隊の順に割り当てる。割り当てない
/// 部隊は海路の部隊として揚陸作戦へ回る。
fn assign_land_force(
    board: &PlayerBoard,
    player_id: PlayerId,
    front: &NarrowFront,
) -> (HashSet<Entity>, usize) {
    let map = &board.map;
    let own_ground = || {
        board
            .units
            .iter()
            .filter(|unit| unit.owner == player_id && unit.is_deployed() && is_ground(unit))
    };
    let cell = |unit: &BoardUnit| (unit.position.x, unit.position.y);
    let mut land_force: HashSet<Entity> = own_ground()
        .filter(|unit| front.corridor.contains(&cell(unit)))
        .map(|unit| unit.entity)
        .collect();
    let distance_to_corridor = |unit: &BoardUnit| {
        front
            .corridor
            .iter()
            .map(|corridor| map.distance(corridor.0, corridor.1, unit.position.x, unit.position.y))
            .min()
            .unwrap_or(u32::MAX)
    };
    let mut candidates: Vec<&BoardUnit> = own_ground()
        .filter(|unit| front.home_side.contains(&cell(unit)))
        .collect();
    candidates.sort_by_key(|unit| {
        (
            unit.stats.min_range <= 1,
            distance_to_corridor(unit),
            unit.entity,
        )
    });
    let quota = front.land_force_quota();
    let open = quota.saturating_sub(land_force.len());
    land_force.extend(candidates.iter().take(open).map(|unit| unit.entity));
    let shortfall = quota.saturating_sub(land_force.len());
    (land_force, shortfall)
}

impl Landmasses {
    /// 盤面から陸塊ごとの事実を集める。
    pub(crate) fn observe(board: &PlayerBoard, player_id: PlayerId) -> Self {
        let land = label_landmasses(board);
        let mut info: HashMap<u32, LandmassInfo> = HashMap::default();
        for property in &board.properties {
            let Some(landmass) = land.at(property.position) else {
                continue;
            };
            let entry = info.entry(landmass).or_default();
            if property.owner != Some(player_id) {
                entry.unowned.push(property.position);
            }
            if property.owner == Some(player_id) && property.terrain == Terrain::Capital {
                entry.own_capital = true;
            }
            if property.owner.is_some_and(|owner| owner != player_id) {
                entry.enemy_owned += 1;
                if property.terrain == Terrain::Capital {
                    entry.enemy_capital = true;
                }
            }
        }
        for unit in board
            .units
            .iter()
            .filter(|unit| unit.is_deployed() && is_ground(unit))
        {
            let Some(landmass) = land.at(unit.position) else {
                continue;
            };
            let entry = info.entry(landmass).or_default();
            let strength = i64::from(unit.stats.cost) * i64::from(unit.health.unwrap_or(100)) / 100;
            if unit.owner == player_id {
                entry.own_ground += 1;
                entry.own_strength += strength;
                if unit.stats.can_capture {
                    entry.own_capturers += 1;
                }
            } else {
                entry.enemy_ground += 1;
                entry.enemy_strength += strength;
            }
        }
        // 拠点の並びを座標順に固定し、走査順に依存しない判断にする。
        for entry in info.values_mut() {
            entry
                .unowned
                .sort_by_key(|position| (position.y, position.x));
        }
        // 隘路で陸塊を分けた盤面では、陸路と海路へ戦力を割り当てる。
        let front = NarrowFront::observe_split(board, player_id, &land);
        let (land_force, land_force_shortfall) = front
            .as_ref()
            .map(|front| assign_land_force(board, player_id, front))
            .unwrap_or_default();
        Self {
            land,
            info,
            front,
            land_force,
            land_force_shortfall,
        }
    }

    /// 座標の陸塊の事実。
    pub(crate) fn at(&self, position: GridPosition) -> Option<(u32, &LandmassInfo)> {
        let landmass = self.land.at(position)?;
        self.info.get(&landmass).map(|info| (landmass, info))
    }

    /// 自軍の地上部隊が今の陸塊で仕事を持たないか。
    ///
    /// 占領役は未所有拠点も敵部隊も無い陸塊で、戦闘役は敵部隊も敵拠点も無い陸塊で
    /// 取り残されている。隘路を押し進む陸路の部隊は取り残されていない。
    pub(crate) fn is_stranded(&self, unit: &BoardUnit) -> bool {
        if !is_ground(unit) || !unit.is_deployed() || self.land_force.contains(&unit.entity) {
            return false;
        }
        let Some((_, info)) = self.at(unit.position) else {
            return false;
        };
        if unit.stats.can_capture {
            info.unowned.is_empty() && info.enemy_ground == 0
        } else {
            !info.needs_combat()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::context::board::BoardUnit;
    use crate::ai::v4::maritime::test_support::board_from_rows;
    use crate::resources::UnitType;
    use crate::resources::master_data::UnitName;

    fn unit(board: &PlayerBoard, owner: u32, unit_type: UnitType, x: usize, y: usize) -> BoardUnit {
        BoardUnit {
            entity: bevy_ecs::entity::Entity::from_raw((x * 100 + y) as u32),
            owner: PlayerId(owner),
            position: GridPosition { x, y },
            stats: board
                .registry
                .create_unit_stats(&UnitName(unit_type.as_str().to_owned()))
                .unwrap(),
            transported: false,
            cargo: None,
            health: Some(100),
            ammo: None,
            fuel: None,
            action_completed: Some(false),
        }
    }

    #[test]
    fn units_on_a_fully_owned_island_are_stranded() {
        // 左の島は自軍の首都だけ。右の島に中立都市。
        let mut board = board_from_rows(&["H.~~C."], &[(0, 0, 1), (4, 0, 0)]);
        let infantry = unit(&board, 1, UnitType::Infantry, 1, 0);
        let tank = unit(&board, 1, UnitType::Tank, 0, 0);
        board.units = vec![infantry.clone(), tank.clone()];
        let landmasses = Landmasses::observe(&board, PlayerId(1));
        assert!(landmasses.is_stranded(&infantry));
        assert!(landmasses.is_stranded(&tank));
        let city = landmasses.at(GridPosition { x: 4, y: 0 }).unwrap().1;
        assert_eq!(city.capture_need(), 1);
        assert!(!city.needs_combat());
    }

    #[test]
    fn capturer_with_unowned_property_on_its_island_is_not_stranded() {
        let mut board = board_from_rows(&["HC~~C."], &[(0, 0, 1), (1, 0, 0), (4, 0, 0)]);
        let infantry = unit(&board, 1, UnitType::Infantry, 0, 0);
        board.units = vec![infantry.clone()];
        let landmasses = Landmasses::observe(&board, PlayerId(1));
        assert!(!landmasses.is_stranded(&infantry));
    }

    #[test]
    fn capitals_linked_only_by_a_mountain_pass_are_split_into_landmasses() {
        // 首都同士は左端の長い山道（歩兵の移動コスト8）でしか陸続きでない。戦車は山を越えられない。
        let rows = ["H...", "^~~~", "^~~~", "^~~~", "^~~~", "H..."];
        let mut board = board_from_rows(&rows, &[(0, 0, 1), (0, 5, 2)]);
        let tank = unit(&board, 1, UnitType::Tank, 1, 0);
        board.units = vec![tank.clone()];
        let landmasses = Landmasses::observe(&board, PlayerId(1));
        let (home, _) = landmasses.at(GridPosition { x: 0, y: 0 }).unwrap();
        let (enemy, info) = landmasses.at(GridPosition { x: 0, y: 5 }).unwrap();
        assert_ne!(home, enemy);
        assert!(info.enemy_capital);
        // 本拠に仕事が無い戦車は、揚陸作戦へ回る。
        assert!(landmasses.is_stranded(&tank));
    }

    #[test]
    fn capitals_linked_by_open_ground_share_a_landmass() {
        // 山があっても、平地で陸続きなら同じ陸塊として扱う。
        let rows = ["H...", "^..~", "^..~", "H..."];
        let board = board_from_rows(&rows, &[(0, 0, 1), (0, 3, 2)]);
        let landmasses = Landmasses::observe(&board, PlayerId(1));
        assert_eq!(
            landmasses.at(GridPosition { x: 0, y: 0 }).map(|(id, _)| id),
            landmasses.at(GridPosition { x: 0, y: 3 }).map(|(id, _)| id)
        );
        // 山のセルも歩兵の陸塊に含まれる。
        assert!(landmasses.at(GridPosition { x: 0, y: 1 }).is_some());
    }

    #[test]
    fn capitals_linked_by_a_short_mountain_pass_share_a_landmass() {
        // 山1マスの峠は歩兵が1手番で越えられるので、陸路で攻め込む。
        let rows = ["H...", "^~~~", "H..."];
        let board = board_from_rows(&rows, &[(0, 0, 1), (0, 2, 2)]);
        let landmasses = Landmasses::observe(&board, PlayerId(1));
        assert_eq!(
            landmasses.at(GridPosition { x: 0, y: 0 }).map(|(id, _)| id),
            landmasses.at(GridPosition { x: 0, y: 2 }).map(|(id, _)| id)
        );
    }

    #[test]
    fn narrow_front_sends_nearby_and_indirect_units_by_land_and_the_rest_by_sea() {
        // 首都同士は1マス幅の地峡（x=3, y=3..6）でしかつながらず、両側に港がある。
        // 陸路の部隊は経路1本あたり6部隊: 間接射撃の自走砲と、地峡に近い戦車から割り当てる。
        let rows = [
            "~~~~~~~", "~HP...~", "~.....~", "~.....~", "~~~.~~~", "~~~.~~~", "~.....~", "~HP...~",
            "~~~~~~~",
        ];
        let mut board = board_from_rows(&rows, &[(1, 1, 1), (2, 1, 1), (1, 7, 2), (2, 7, 2)]);
        let artillery = unit(&board, 1, UnitType::Artillery, 1, 2);
        let near: Vec<BoardUnit> = [(2, 3), (4, 3), (3, 2)]
            .into_iter()
            .map(|(x, y)| unit(&board, 1, UnitType::Tank, x, y))
            .collect();
        let middle: Vec<BoardUnit> = [(2, 2), (4, 2)]
            .into_iter()
            .map(|(x, y)| unit(&board, 1, UnitType::Tank, x, y))
            .collect();
        let far: Vec<BoardUnit> = [(5, 1), (4, 1)]
            .into_iter()
            .map(|(x, y)| unit(&board, 1, UnitType::Tank, x, y))
            .collect();
        board.units = std::iter::once(artillery.clone())
            .chain(near.iter().cloned())
            .chain(middle.iter().cloned())
            .chain(far.iter().cloned())
            .collect();
        let landmasses = Landmasses::observe(&board, PlayerId(1));
        assert!(landmasses.front.is_some());
        assert_eq!(landmasses.land_force.len(), 6);
        assert!(!landmasses.is_stranded(&artillery));
        for tank in near.iter().chain(&middle) {
            assert!(
                !landmasses.is_stranded(tank),
                "陸路の部隊: {:?}",
                tank.position
            );
        }
        for tank in &far {
            assert!(
                landmasses.is_stranded(tank),
                "海路の部隊: {:?}",
                tank.position
            );
        }
    }
}
