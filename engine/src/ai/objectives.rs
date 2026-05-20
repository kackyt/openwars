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

use crate::resources::master_data::MasterDataRegistry;

impl Objective {
    /// 島の期待値を計算する
    /// 期待値 = (獲得可能な追加ターン収入) / (1.0 + 最寄りの自軍生産拠点からの最短距離 + 20.0 * 敵の生産拠点数)
    pub fn evaluate(
        target_island: IslandId,
        properties: &[(GridPosition, Terrain)],
        distance_to_nearest_base: i32,
        enemy_production_count: u32,
        registry: &MasterDataRegistry,
    ) -> Self {
        let mut total_income_increase = 0;
        let mut properties_count = 0;

        for (_, terrain) in properties {
            properties_count += 1;
            // 地形ごとの毎ターンの獲得追加収入を取得して合算
            let income = registry.landscape_income(terrain.as_str());
            total_income_increase += income;
        }

        // 期待値スコアの算出（軍事脅威ペナルティを加味）
        // 敵の生産拠点（首都・工場）の数に応じて分母にペナルティを加算（1拠点につき +20）
        let priority_score = if properties_count > 0 {
            let penalty = 20.0 * enemy_production_count as f64;
            let score =
                (total_income_increase as f64) / (1.0 + distance_to_nearest_base as f64 + penalty);
            score as i32
        } else {
            0
        };

        let needed_infantry = if properties_count > 0 {
            properties_count
        } else {
            1
        };

        Self {
            target_island,
            priority_score: PriorityScore(priority_score),
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
        let distance_to_nearest_base = 3;
        let registry = MasterDataRegistry::load().expect("Failed to load master data");

        // 敵の生産拠点数が 0 の場合
        let objective = Objective::evaluate(
            target_island,
            &properties,
            distance_to_nearest_base,
            0,
            &registry,
        );

        assert_eq!(objective.target_island, IslandId(1));
        // Capital (4000) + Factory (1000) + City (1000) = 6000
        // Expected Value = 6000 / (1.0 + 3.0 + 0.0) = 1500
        assert_eq!(objective.priority_score, PriorityScore(1500));
        assert_eq!(objective.needed_infantry, InfantryCount(3));
    }

    #[test]
    fn test_objective_evaluate_with_threat_penalty() {
        let target_island = IslandId(1);
        let properties = vec![
            (GridPosition { x: 0, y: 0 }, Terrain::Capital),
            (GridPosition { x: 1, y: 0 }, Terrain::Factory),
            (GridPosition { x: 2, y: 0 }, Terrain::City),
        ];
        let distance_to_nearest_base = 3;
        let registry = MasterDataRegistry::load().expect("Failed to load master data");

        // 敵の生産拠点数が 1 の場合
        let objective = Objective::evaluate(
            target_island,
            &properties,
            distance_to_nearest_base,
            1,
            &registry,
        );

        assert_eq!(objective.target_island, IslandId(1));
        // Capital (4000) + Factory (1000) + City (1000) = 6000
        // Expected Value = 6000 / (1.0 + 3.0 + 20.0 * 1.0) = 6000 / 24.0 = 250
        assert_eq!(objective.priority_score, PriorityScore(250));
        assert_eq!(objective.needed_infantry, InfantryCount(3));
    }

    #[test]
    fn test_objective_evaluate_empty() {
        let target_island = IslandId(2);
        let properties = vec![];
        let distance_to_nearest_base = 10;
        let registry = MasterDataRegistry::load().expect("Failed to load master data");

        let objective = Objective::evaluate(
            target_island,
            &properties,
            distance_to_nearest_base,
            0,
            &registry,
        );

        assert_eq!(objective.target_island, IslandId(2));
        assert_eq!(objective.priority_score, PriorityScore(0));
        // 拠点が0個でも歩兵は最低1送る
        assert_eq!(objective.needed_infantry, InfantryCount(1));
    }
}
