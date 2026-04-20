use crate::components::{Faction, Health, PlayerId, Property, Transporting, UnitStats};
use bevy_ecs::prelude::*;

/// 盤面の静的評価関数。
/// 指定したプレイヤー（通常はAIプレイヤー）から見た盤面の優位性を算出します。
/// 戦力スコア（HPとユニットコストの積）や陣地・拠点スコアを総合します。
pub fn evaluate_board(world: &mut World, perspective_player: PlayerId) -> i32 {
    let mut score = 0;

    // 1. ユニット戦力の評価
    // 自軍ユニットはプラス、敵軍ユニットはマイナスとして加算します。
    // 輸送中のユニット（Transportingコンポーネントを持つ）は、二重計上を防ぐため評価から除外します。
    let mut query =
        world.query_filtered::<(&Faction, &Health, &UnitStats), Without<Transporting>>();
    for (faction, health, stats) in query.iter(world) {
        // 現在のHP割合を掛けた実質価値を算出
        let value = if health.max > 0 {
            (stats.cost as i32 * health.current as i32) / health.max as i32
        } else {
            0
        };

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resources::Terrain;

    #[test]
    fn test_evaluate_board() {
        let mut world = World::new();
        let p1 = PlayerId(1);
        let p2 = PlayerId(2);

        // Friendly unit (full hp) -> 1000 cost * 10/10 = +1000
        world.spawn((
            Faction(p1),
            Health {
                current: 100,
                max: 100,
            },
            UnitStats {
                cost: 1000,
                ..UnitStats::mock()
            },
        ));

        // Friendly unit (half hp) -> 2000 cost * 5/10 = +1000
        world.spawn((
            Faction(p1),
            Health {
                current: 50,
                max: 100,
            },
            UnitStats {
                cost: 2000,
                ..UnitStats::mock()
            },
        ));

        // Enemy unit -> 1500 cost * 10/10 = -1500
        world.spawn((
            Faction(p2),
            Health {
                current: 100,
                max: 100,
            },
            UnitStats {
                cost: 1500,
                ..UnitStats::mock()
            },
        ));

        // Enemy transported unit -> should be ignored
        world.spawn((
            Faction(p2),
            Health {
                current: 100,
                max: 100,
            },
            UnitStats {
                cost: 5000,
                ..UnitStats::mock()
            },
            Transporting(Entity::from_raw(999)),
        ));

        // Zero max HP unit -> should be ignored safely
        world.spawn((
            Faction(p1),
            Health {
                current: 100,
                max: 0,
            },
            UnitStats {
                cost: 5000,
                ..UnitStats::mock()
            },
        ));

        // Properties
        world.spawn(Property::new(Terrain::Capital, Some(p1), 200)); // +10000
        world.spawn(Property::new(Terrain::City, Some(p1), 200)); // +1000
        world.spawn(Property::new(Terrain::Factory, Some(p2), 200)); // -2000
        world.spawn(Property::new(Terrain::City, None, 200)); // 0 (unowned)

        let score = evaluate_board(&mut world, p1);
        // Expected score:
        // P1 Units: 1000 + 1000 = 2000
        // P2 Units: -1500
        // P1 Props: 10000 + 1000 = 11000
        // P2 Props: -2000
        // Total: 2000 - 1500 + 11000 - 2000 = 9500
        assert_eq!(score, 9500);

        let score_p2 = evaluate_board(&mut world, p2);
        // From P2's perspective, it should be the exact inverse
        assert_eq!(score_p2, -9500);
    }
}
