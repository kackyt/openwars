use crate::components::{Faction, Health, PlayerId, Property, UnitStats};
use bevy_ecs::prelude::*;

/// 盤面の静的評価関数。
/// 指定したプレイヤー（通常はAIプレイヤー）から見た盤面の優位性を算出します。
/// 戦力スコア（HPとユニットコストの積）や陣地・拠点スコアを総合します。
pub fn evaluate_board(world: &mut World, perspective_player: PlayerId) -> i32 {
    let mut score = 0;

    // 1. ユニット戦力の評価
    // 自軍ユニットはプラス、敵軍ユニットはマイナスとして加算します。
    let mut query = world.query::<(&Faction, &Health, &UnitStats)>();
    for (faction, health, stats) in query.iter(world) {
        // 現在のHP割合を掛けた実質価値を算出
        let value = (stats.cost as i32 * health.current as i32) / health.max as i32;

        if faction.0 == perspective_player {
            score += value;
        } else {
            score -= value;
        }
    }

    // 2. 拠点所有の評価
    // 拠点は毎ターンの収入源となるため、高く評価します。
    // 特に首都は非常に高い価値を持ちます。
    let mut prop_query = world.query::<&Property>();
    for prop in prop_query.iter(world) {
        if let Some(owner) = prop.owner_id {
            let prop_value = match prop.terrain {
                crate::resources::Terrain::Capital => 10000,
                crate::resources::Terrain::Factory | crate::resources::Terrain::Airport => 2000,
                crate::resources::Terrain::City => 1000,
                _ => 0,
            };

            if owner == perspective_player {
                score += prop_value;
            } else {
                score -= prop_value;
            }
        }
    }

    score
}
