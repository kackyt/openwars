use crate::ai::islands::IslandId;
use crate::components::GridPosition;
use crate::resources::Terrain;

/// 戦略的な目標（占領すべき島など）を表す構造体
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Objective {
    pub target_island: IslandId,
    pub priority_score: i32,
    pub needed_infantry: usize, // この島を制圧するために必要な歩兵の数
}

impl Objective {
    /// 島の価値（スコア）を計算する
    /// - 拠点（Capital, Factory, Cityなど）の価値を合算する
    /// - 拠点数に応じて必要な歩兵数を計算する
    pub fn evaluate(
        target_island: IslandId,
        properties: &[(GridPosition, Terrain)],
        distance_penalty: i32,
    ) -> Self {
        let mut base_score = 0;
        let mut properties_count = 0;

        for (_, terrain) in properties {
            properties_count += 1;
            match terrain {
                Terrain::Capital => base_score += 1000, // 首都は最優先
                Terrain::Factory => base_score += 500,  // 工場も重要
                Terrain::Airport => base_score += 400,
                Terrain::Port => base_score += 400,
                Terrain::City => base_score += 100,
                _ => base_score += 10, // その他の拠点は少し価値がある
            }
        }

        let needed_infantry = if properties_count > 0 {
            properties_count
        } else {
            1 // 拠点がなくてもとりあえず1部隊は送る（上陸用など）
        };

        Self {
            target_island,
            priority_score: base_score - distance_penalty,
            needed_infantry,
        }
    }
}
