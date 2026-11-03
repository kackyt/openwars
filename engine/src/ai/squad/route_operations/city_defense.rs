//! 都市防衛における直接部隊の通路封鎖と間接部隊の都市支援。
//!
//! 防衛対象の都市周辺で、敵の地上侵入経路を遮断する封鎖地点（チョークポイント）を
//! 地上歩行ステップ距離の輪（BFS等高線）から選定し、直接部隊を配置する。
//! 間接部隊はその封鎖線を射程に収める都市から支援射撃を行う。

use crate::collections::{HashMap, HashSet};
use crate::components::GridPosition;
use crate::resources::master_data::MasterDataRegistry;
use crate::resources::{Map, MovementType};
use std::collections::VecDeque;

/// 都市防衛の計画結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CityDefensePlan {
    /// 直接部隊が配置されるべき封鎖地点（優先度順）。
    pub chokepoints: Vec<GridPosition>,
    /// 間接部隊が支援射撃のために陣取るべき自軍都市。
    pub support_city: Option<GridPosition>,
    /// 敵の地上侵入経路が完全遮断されているか。
    pub is_fully_blocked: bool,
}

/// 都市防衛の封鎖線と支援位置を計画する。
///
/// 都市自身を遮蔽地点として誤選択せず（距離1以上）、迂回可能・複数入口・不足人数・
/// 通行不能/到達不能を考慮して、直接部隊が塞ぐべき最小のボトルネックを選定する。
pub(super) fn plan_city_defense(
    map: &Map,
    master: &MasterDataRegistry,
    city: GridPosition,
    enemies: &[(GridPosition, u32)],
    direct_unit_count: usize,
    indirect_ranges: &[(u32, u32)],
    occupied_by_others: &HashSet<GridPosition>,
) -> Option<CityDefensePlan> {
    if direct_unit_count == 0 && indirect_ranges.is_empty() {
        return None;
    }
    if enemies.is_empty() {
        return None;
    }

    // 地上部隊（歩兵）が通行可能な地形か
    let is_passable = |pos: GridPosition| {
        map.get_terrain(pos.x, pos.y).is_some_and(|terrain| {
            crate::systems::movement::get_valid_movement_cost(
                master,
                MovementType::Infantry,
                terrain,
            )
            .is_some()
        })
    };

    if !is_passable(city) {
        return None;
    }

    // 1. 都市からの地上歩行ステップ距離マップ（BFS等高線）
    // 間接支援射撃が届く範囲（最大4マス程度）まで探索する
    let max_radius = crate::ai::v4::capital_route::model::CAPITAL_ROUTE_REGION_RADIUS + 1;
    let mut city_dist = HashMap::default();
    let mut queue = VecDeque::new();
    city_dist.insert(city, 0_u32);
    queue.push_back(city);

    while let Some(current) = queue.pop_front() {
        let d = city_dist[&current];
        if d >= max_radius {
            continue;
        }
        for (nx, ny) in map.get_adjacent(current.x, current.y) {
            let next = GridPosition { x: nx, y: ny };
            if is_passable(next) && !city_dist.contains_key(&next) {
                city_dist.insert(next, d + 1);
                queue.push_back(next);
            }
        }
    }

    // 2. 敵から都市への地上到達可能性
    let reachable_enemies = enemies
        .iter()
        .filter(|(pos, _)| is_passable(*pos))
        .collect::<Vec<_>>();
    if reachable_enemies.is_empty() {
        return None;
    }

    // 最も都市に近い敵の距離
    let min_enemy_dist = reachable_enemies
        .iter()
        .filter_map(|(pos, _)| city_dist.get(pos).copied())
        .min();

    // 敵がすでに都市に隣接(<=1)または都市内にいる場合は、都市直前(d=1)で防衛
    // それ以外は d in 1..=3 の中で探索（都市自身 d=0 は絶対に除外）
    let candidate_distances: Vec<u32> = match min_enemy_dist {
        Some(d) if d <= 1 => vec![1],
        Some(d) => (1..=d.min(3)).collect(),
        None => (1..=3).collect(),
    };

    // 3. 各距離 d について、敵からの侵入を遮断する入口集合 (K_d) を計算する
    struct CutCandidate {
        distance: u32,
        chokepoints: Vec<GridPosition>,
        is_fully_blocked: bool,
    }

    let mut best_cut: Option<CutCandidate> = None;

    for &d in &candidate_distances {
        let mut entry_points = HashSet::default();
        let mut visited = HashSet::default();
        let mut eq = VecDeque::new();

        for &(enemy_pos, _) in &reachable_enemies {
            eq.push_back(*enemy_pos);
            visited.insert(*enemy_pos);
        }

        let mut reached_city_directly = false;

        while let Some(current) = eq.pop_front() {
            if current == city {
                reached_city_directly = true;
                break;
            }
            if let Some(&dist) = city_dist.get(&current) {
                if dist == d {
                    entry_points.insert(current);
                    // d に到達したら、それより都市側(dist < d)へは進まない
                    continue;
                }
            }
            for (nx, ny) in map.get_adjacent(current.x, current.y) {
                let next = GridPosition { x: nx, y: ny };
                if is_passable(next) && !visited.contains(&next) {
                    visited.insert(next);
                    eq.push_back(next);
                }
            }
        }

        // 敵が都市に到達できず、入口もなければ除外。
        if entry_points.is_empty() && !reached_city_directly {
            continue;
        }
        // 敵自身が立つマスには直接部隊を配置できない。敵を先に排除できる保証もないため、
        // そこを壁と仮定して「完全遮断」と数える候補は使わない。
        if entry_points
            .iter()
            .any(|point| reachable_enemies.iter().any(|(enemy, _)| enemy == point))
        {
            continue;
        }

        // entry_points を障害物として配置した場合に、敵から都市への経路が遮断されるか検証
        let is_blocked = {
            let mut v = HashSet::default();
            let mut q = VecDeque::new();
            for &(enemy_pos, _) in &reachable_enemies {
                if !entry_points.contains(enemy_pos) {
                    q.push_back(*enemy_pos);
                    v.insert(*enemy_pos);
                }
            }
            let mut can_reach = false;
            while let Some(curr) = q.pop_front() {
                if curr == city {
                    can_reach = true;
                    break;
                }
                for (nx, ny) in map.get_adjacent(curr.x, curr.y) {
                    let next = GridPosition { x: nx, y: ny };
                    if is_passable(next) && !entry_points.contains(&next) && !v.contains(&next) {
                        v.insert(next);
                        q.push_back(next);
                    }
                }
            }
            !can_reach
        };

        let mut chokepoints: Vec<GridPosition> = entry_points.into_iter().collect();
        // 敵の最寄りETA順にソート（脅威が早い方を優先）
        chokepoints.sort_by_key(|pos| {
            let min_eta = enemies
                .iter()
                .map(|(ep, mov)| map.distance(ep.x, ep.y, pos.x, pos.y).div_ceil(*mov))
                .min()
                .unwrap_or(u32::MAX);
            (min_eta, pos.y, pos.x)
        });

        let cand = CutCandidate {
            distance: d,
            chokepoints,
            is_fully_blocked: is_blocked,
        };

        let is_better = match &best_cut {
            None => true,
            Some(prev) => {
                let prev_ok = prev.is_fully_blocked && prev.chokepoints.len() <= direct_unit_count;
                let cand_ok = cand.is_fully_blocked && cand.chokepoints.len() <= direct_unit_count;
                match (cand_ok, prev_ok) {
                    (true, false) => true,
                    (false, true) => false,
                    (true, true) => {
                        // どちらも部隊数内で完全遮断可能: 必要人数が少ない方優先
                        if cand.chokepoints.len() != prev.chokepoints.len() {
                            cand.chokepoints.len() < prev.chokepoints.len()
                        } else {
                            // 同じなら d=2 等、都市から適度に離れている方を優先（dが大きい）
                            cand.distance > prev.distance
                        }
                    }
                    (false, false) => {
                        // どちらも部隊数オーバーまたは完全遮断不可: 人数が少ない方、または d=1
                        if cand.is_fully_blocked != prev.is_fully_blocked {
                            cand.is_fully_blocked
                        } else if cand.chokepoints.len() != prev.chokepoints.len() {
                            cand.chokepoints.len() < prev.chokepoints.len()
                        } else {
                            cand.distance < prev.distance
                        }
                    }
                }
            }
        };

        if is_better {
            best_cut = Some(cand);
        }
    }

    let mut cut = best_cut?;

    // 人数不足の場合の切り詰め
    let is_fully_blocked = cut.is_fully_blocked && cut.chokepoints.len() <= direct_unit_count;
    if cut.chokepoints.len() > direct_unit_count {
        cut.chokepoints.truncate(direct_unit_count);
    }

    // 4. 間接部隊の支援都市の選定
    // 都市 city が封鎖地点の少なくとも1つを射程内に収めているか
    let support_city = if !occupied_by_others.contains(&city) && !indirect_ranges.is_empty() {
        let city_serves = cut.chokepoints.iter().any(|&cp| {
            let dist = map.distance(city.x, city.y, cp.x, cp.y);
            indirect_ranges
                .iter()
                .any(|&(min_r, max_r)| dist >= min_r && dist <= max_r)
        });
        if city_serves { Some(city) } else { None }
    } else {
        None
    };

    Some(CityDefensePlan {
        chokepoints: cut.chokepoints,
        support_city,
        is_fully_blocked,
    })
}
