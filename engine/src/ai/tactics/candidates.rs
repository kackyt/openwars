//! 移動候補タイルの事前フィルタ（航空ユニットの帰投燃料保証など）。
use crate::components::{GridPosition, PlayerId, UnitStats};
use crate::resources::{Map, Terrain};

/// 航空ユニットが候補タイルへ移動した後も、自軍空港へ帰投できる燃料を残すか。
///
/// 移動燃料だけでなく、空港外で迎える各ラウンドの日次消費も予約する。攻撃加点や
/// Squad接近加点より前に候補自体を除外するため、任務持ち航空機も燃料切れになるまで
/// 前線を徘徊できない。
pub(crate) fn air_move_preserves_return_fuel(
    map: &Map,
    properties: &[(GridPosition, Terrain, Option<PlayerId>)],
    player_id: PlayerId,
    stats: &UnitStats,
    origin: GridPosition,
    candidate: GridPosition,
    current_fuel: u32,
) -> bool {
    if stats.movement_type != crate::resources::MovementType::Air {
        return true;
    }
    let airports: Vec<_> = properties
        .iter()
        .filter_map(|(position, terrain, owner)| {
            (*owner == Some(player_id) && *terrain == Terrain::Airport).then_some(*position)
        })
        .collect();
    if airports.is_empty() {
        // 最小テストfixtureや空港喪失後の既存unitには帰投先を導出できない。
        // ここで全行動を停止させず、空港を観測できる通常局面だけ安全圏を強制する。
        return true;
    }
    let movement_cost = map.distance(origin.x, origin.y, candidate.x, candidate.y);
    let remaining_fuel = current_fuel.saturating_sub(movement_cost);
    if airports.contains(&candidate) {
        return true;
    }
    let return_distance = airports
        .iter()
        .map(|airport| map.distance(candidate.x, candidate.y, airport.x, airport.y))
        .min()
        .unwrap_or(u32::MAX);
    let return_turns = return_distance.div_ceil(stats.max_movement.max(1));
    let required_fuel =
        return_distance.saturating_add(stats.daily_fuel_consumption.saturating_mul(return_turns));
    if remaining_fuel >= required_fuel {
        return true;
    }

    // 前線空港を敵に奪われるなどして、手番開始時点ですでに完全帰投可能圏の外へ
    // 出ることがある。この状態で通常の安全条件だけを使うと、空港へ近づく一歩まで
    // 全候補から消えて航空機が行動可能なまま停止する。燃料不足を増やさず、かつ
    // 最寄り空港への距離を縮める候補だけを回復移動として許可する。
    let origin_return_distance = airports
        .iter()
        .map(|airport| map.distance(origin.x, origin.y, airport.x, airport.y))
        .min()
        .unwrap_or(u32::MAX);
    let origin_return_turns = origin_return_distance.div_ceil(stats.max_movement.max(1));
    let origin_required_fuel = origin_return_distance.saturating_add(
        stats
            .daily_fuel_consumption
            .saturating_mul(origin_return_turns),
    );
    let origin_deficit = origin_required_fuel.saturating_sub(current_fuel);
    let candidate_deficit = required_fuel.saturating_sub(remaining_fuel);
    origin_deficit > 0
        && return_distance < origin_return_distance
        && candidate_deficit <= origin_deficit
}
