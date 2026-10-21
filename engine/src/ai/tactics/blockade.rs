//! 敵生産口の占位（blockade）。小規模マップ・通常経路の行動選択で共有する。

use super::board::TacticalBoard;
use super::unit::UnitState;
use crate::collections::HashSet;
use crate::components::GridPosition;
use crate::resources::master_data::MasterDataRegistry;
use crate::resources::{Terrain, UnitType};
use crate::systems::production::is_within_production_range;

/// 敵生産口の占位（blockade）。自軍の生産口を塞がない知識を攻勢的に反転させ、
/// 敵の補充を物理的に遮断する。
pub(crate) struct EnemyProductionBlockade {
    /// 敵首都の生産可能範囲にある敵所有の生産施設。
    pub(crate) sites: HashSet<GridPosition>,
    /// 敵の補充依存度（分子, 分母）。占位価値の局面係数。
    dependence: (u32, u32),
}

/// 1つの移動先での占位の事実。
#[derive(Default, Clone, Copy)]
pub(crate) struct BlockadeTile {
    pub(crate) starts_on_site: bool,
    /// 移動先が敵生産口なら、その占位価値（それ以外は0）。
    pub(crate) site_value: u32,
    /// 移動先が敵生産口で、かつ占位して生き残れる（戦闘可能で、次ターンの期待被害がHP未満）。
    pub(crate) holds_site: bool,
}

impl EnemyProductionBlockade {
    pub(crate) fn observe(board: &TacticalBoard) -> Self {
        let enemy_capitals = board
            .properties
            .iter()
            .filter_map(|(position, terrain, owner)| match owner {
                Some(owner) if *owner != board.player_id && *terrain == Terrain::Capital => {
                    Some((*owner, *position))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        let sites = board
            .properties
            .iter()
            .filter_map(|(position, terrain, owner)| {
                let owner = owner.filter(|owner| *owner != board.player_id)?;
                if !board.registry.is_production_facility(terrain.as_str()) {
                    return None;
                }
                let owner_capitals = enemy_capitals
                    .iter()
                    .filter_map(|(capital_owner, capital)| {
                        (*capital_owner == owner).then_some(*capital)
                    })
                    .collect::<Vec<_>>();
                is_within_production_range(
                    &owner_capitals,
                    position.x,
                    position.y,
                    board.map.topology,
                )
                .then_some(*position)
            })
            .collect::<HashSet<_>>();
        let dependence = enemy_replacement_dependence(sites.len(), board.enemies.len());
        Self { sites, dependence }
    }

    pub(crate) fn assess(
        &self,
        board: &TacticalBoard,
        unit: &UnitState,
        position: GridPosition,
        defense_bonus: u32,
    ) -> BlockadeTile {
        let starts_on_site = self.sites.contains(&unit.position);
        if !self.sites.contains(&position) {
            return BlockadeTile {
                starts_on_site,
                ..BlockadeTile::default()
            };
        }
        // 施設が供給し得る補充の価値を、敵がその補充にどれだけ依存しているかで割り引く。
        let site_value = board
            .properties
            .iter()
            .find(|(site, ..)| *site == position)
            .map(|(_, terrain, _)| production_facility_blockade_value(*terrain, &board.registry))
            .unwrap_or(0)
            .saturating_mul(self.dependence.0)
            / self.dependence.1;
        // 序盤の自殺的突入を抑止するため、戦闘不能unitや次ターン即死圏
        // （期待被damage >= 現在HP）は占位から除外する。
        let incoming_damage = crate::ai::threat::expected_incoming_damage(
            &board.map,
            (position.x, position.y),
            unit.stats.unit_type,
            defense_bonus,
            &board.enemies,
            &board.damage_chart,
        );
        BlockadeTile {
            starts_on_site,
            site_value,
            holds_site: !unit.combat_ineffective && incoming_damage < unit.hp,
        }
    }
}

/// 生産施設が1ターンに供給し得るunitの平均コスト（補充阻止価値）。
/// 敵の生産マスを1つ占位することは、敵の補充を毎ターン1体分止めることであり、
/// その価値は止めた補充の価値に等しいという教義に基づく。
/// 魔法定数を避け、マスターデータのunitカタログから導出する。
fn production_facility_blockade_value(terrain: Terrain, registry: &MasterDataRegistry) -> u32 {
    let producible_costs: Vec<u32> = registry
        .unit_order
        .iter()
        .filter_map(|name| {
            let record = registry.units.get(name)?;
            let unit_type = UnitType::from_str(&record.name.0)?;
            registry
                .can_produce_unit(terrain.as_str(), unit_type)
                .then_some(record.cost)
        })
        .collect();
    if producible_costs.is_empty() {
        0
    } else {
        producible_costs.iter().sum::<u32>() / producible_costs.len() as u32
    }
}

/// 敵の補充依存度を分数 (分子, 分母) で返す。占位の実効価値係数。
///
/// 占位が止められるのは「敵生産マス1つあたり毎ターン1体の補充」であり、
/// その価値は敵の次ターン戦力に占める補充の割合、すなわち
/// 生産能力 / (生産能力 + 現有戦力) に比例する。
///
/// - 敵が大軍を擁する序盤〜中盤はこの割合が小さい。補充1体を止める利得より、
///   自軍1体を占位に貼り付けて遊ばせる損のほうが大きいため、占位は事実上無効になる。
/// - 敵が消耗し、補充だけが生命線になった終盤で最大化し、全滅勝利を後押しする。
fn enemy_replacement_dependence(production_capacity: usize, fielded_units: usize) -> (u32, u32) {
    let capacity = production_capacity as u32;
    let denominator = capacity.saturating_add(fielded_units as u32);
    if denominator == 0 {
        // 生産能力も現有戦力もない敵に対して、止めるべき補充は存在しない。
        (0, 1)
    } else {
        (capacity, denominator)
    }
}

#[cfg(test)]
mod tests {
    use super::enemy_replacement_dependence;

    #[test]
    fn enemy_replacement_dependence_scales_with_reliance_on_production() {
        // 敵が大軍を擁する序盤〜中盤: 補充1体を止めても次ターン戦力への寄与は小さい。
        let (num, den) = enemy_replacement_dependence(4, 36);
        assert_eq!((num, den), (4, 40));
        assert!(
            num * 10 <= den,
            "大軍を持つ敵への占位価値は1割以下に割り引かれるべき"
        );

        // 敵が消耗し補充だけが生命線になった終盤: 占位価値が最大化する。
        let (num, den) = enemy_replacement_dependence(4, 2);
        assert_eq!((num, den), (4, 6));
        assert!(
            num * 2 > den,
            "消耗した敵への占位価値は半分より大きく評価されるべき"
        );

        // 敵が全滅していれば止めるべき補充はないが、生産能力は価値を保つ。
        assert_eq!(enemy_replacement_dependence(3, 0), (3, 3));

        // 敵の生産能力がなければ占位の価値はゼロ。
        assert_eq!(enemy_replacement_dependence(0, 10), (0, 10));

        // 生産能力も戦力もない敵に対しては、ゼロ除算せず価値ゼロを返す。
        assert_eq!(enemy_replacement_dependence(0, 0), (0, 1));
    }
}
