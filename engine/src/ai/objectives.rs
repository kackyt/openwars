use crate::ai::islands::IslandId;
use crate::components::GridPosition;
use crate::resources::Terrain;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct PriorityScore(pub i32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct InfantryCount(pub usize);

/// 戦略的な目標（占領すべき島など）を表す構造体
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Objective {
    pub target_island: IslandId,
    pub priority_score: PriorityScore,
    pub needed_infantry: InfantryCount, // この島を制圧するために必要な歩兵の数
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
            priority_score: PriorityScore(base_score - distance_penalty),
            needed_infantry: InfantryCount(needed_infantry),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_objective_evaluate() {
        let target_island = IslandId(1);
        let properties = vec![
            (GridPosition { x: 0, y: 0 }, Terrain::Capital),
            (GridPosition { x: 1, y: 0 }, Terrain::Factory),
            (GridPosition { x: 2, y: 0 }, Terrain::City),
        ];
        let distance_penalty = 50;

        let objective = Objective::evaluate(target_island, &properties, distance_penalty);

        assert_eq!(objective.target_island, IslandId(1));
        // Capital (1000) + Factory (500) + City (100) - Penalty (50) = 1550
        assert_eq!(objective.priority_score, PriorityScore(1550));
        assert_eq!(objective.needed_infantry, InfantryCount(3));
    }

    #[test]
    fn test_objective_evaluate_empty() {
        let target_island = IslandId(2);
        let properties = vec![];
        let distance_penalty = 10;

        let objective = Objective::evaluate(target_island, &properties, distance_penalty);

        assert_eq!(objective.target_island, IslandId(2));
        assert_eq!(objective.priority_score, PriorityScore(-10));
        // 拠点が0個でも歩兵は最低1送る
        assert_eq!(objective.needed_infantry, InfantryCount(1));
    }
}
