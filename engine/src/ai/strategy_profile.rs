//! V4がターン全体で共有する戦略プロファイル。
//!
//! 小規模マップ向けの作戦だけを生産・Roadmap・Squadへ個別に混在させると、各層が
//! 異なる前提で同じ部隊を評価してしまう。ここで一度だけプロファイルを確定し、以後の
//! V4パイプラインは同じプロファイルを参照する。

use crate::components::{GridPosition, Property};
use crate::resources::{Map, Terrain};
use bevy_ecs::prelude::*;

/// 小規模マップ展開戦術を適用する盤面の最大セル数。
///
/// map_1 (10x14) と map_2 (14x14) は含め、map_6 (30x22) と map_25 (30x18) は
/// 含めない。盤面サイズは対局中に変化しないため、戦況や所有権で戦略経路が途中で
/// 切り替わることはない。
const SMALL_MAP_MAX_CELLS: usize = 14 * 14;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum V4StrategyProfile {
    /// main互換の汎用V4戦略。
    Standard,
    /// 拡張レースと前線密度を強く扱う小規模マップ専用戦略。
    SmallMapExpansion,
}

#[derive(Resource, Debug, Default)]
pub struct V4StrategyProfileRegistry {
    profiles: std::collections::HashMap<crate::components::PlayerId, V4StrategyProfile>,
}

/// ターン開始時に決めたプロファイルを返す。未決定なら盤面構造から一度だけ確定する。
pub fn profile_for(world: &mut World, player_id: crate::components::PlayerId) -> V4StrategyProfile {
    if let Some(profile) = world
        .get_resource::<V4StrategyProfileRegistry>()
        .and_then(|registry| registry.profiles.get(&player_id))
        .copied()
    {
        return profile;
    }

    let profile = classify_profile(world);
    world
        .get_resource_or_insert_with(V4StrategyProfileRegistry::default)
        .profiles
        .insert(player_id, profile);
    profile
}

/// 読み取り専用の経路では未初期化をStandardとして扱う。
pub fn current_profile(world: &World, player_id: crate::components::PlayerId) -> V4StrategyProfile {
    world
        .get_resource::<V4StrategyProfileRegistry>()
        .and_then(|registry| registry.profiles.get(&player_id))
        .copied()
        .unwrap_or(V4StrategyProfile::Standard)
}

fn classify_profile(world: &World) -> V4StrategyProfile {
    let Some(map) = world.get_resource::<Map>() else {
        return V4StrategyProfile::Standard;
    };
    let capturable_non_capitals = world
        .iter_entities()
        .filter_map(|entity| {
            let position = entity.get::<GridPosition>()?;
            let property = entity.get::<Property>()?;
            Some((*position, property))
        })
        .filter(|(_, property)| {
            property.max_capture_points > 0 && property.terrain != Terrain::Capital
        })
        .count();

    if map.width.saturating_mul(map.height) <= SMALL_MAP_MAX_CELLS && capturable_non_capitals > 0 {
        V4StrategyProfile::SmallMapExpansion
    } else {
        V4StrategyProfile::Standard
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resources::{GridTopology, master_data::MasterDataRegistry};

    #[test]
    fn profile_is_fixed_by_map_structure_not_by_turn_state() {
        let master_data = MasterDataRegistry::load().unwrap();
        for map_id in ["map_1", "map_2"] {
            let (mut world, _) = crate::setup::initialize_world_from_master_data_with_topology(
                &master_data,
                map_id,
                GridTopology::Square,
            )
            .unwrap();
            assert_eq!(
                profile_for(&mut world, crate::components::PlayerId(1)),
                V4StrategyProfile::SmallMapExpansion
            );
        }
        for map_id in ["map_6", "map_25"] {
            let (mut world, _) = crate::setup::initialize_world_from_master_data_with_topology(
                &master_data,
                map_id,
                GridTopology::Square,
            )
            .unwrap();
            assert_eq!(
                profile_for(&mut world, crate::components::PlayerId(1)),
                V4StrategyProfile::Standard
            );
        }
    }
}
