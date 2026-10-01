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
    /// 共通パイプラインで実行する標準マップ向けの汎用V4戦略。
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

/// 標準V4（V4かつ盤面がStandardプロファイル）かを判定する。
///
/// V4以外のプレイヤーについてプロファイルを登録しないよう、V4の場合だけ
/// `profile_for` で盤面から確定する。
pub fn is_standard_v4(world: &mut World, player_id: crate::components::PlayerId) -> bool {
    crate::ai::resolve_player_ai_version(world, player_id) == crate::ai::AiVersion::V4
        && profile_for(world, player_id) == V4StrategyProfile::Standard
}

/// 読み取り専用の経路で標準V4かを判定する。
///
/// V4のターン開始時に `profile_for` でプロファイルが確定済みである前提で、
/// `current_profile` を参照する。
pub fn is_standard_v4_readonly(world: &World, player_id: crate::components::PlayerId) -> bool {
    crate::ai::resolve_player_ai_version(world, player_id) == crate::ai::AiVersion::V4
        && current_profile(world, player_id) == V4StrategyProfile::Standard
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
    fn small_map_boundary_requires_a_capturable_non_capital() {
        fn profile_at(width: usize, height: usize, terrain: Terrain) -> V4StrategyProfile {
            let mut world = World::new();
            world.insert_resource(Map {
                width,
                height,
                tiles: vec![Terrain::Plains; width * height],
                topology: GridTopology::Square,
            });
            world.spawn((
                GridPosition { x: 0, y: 0 },
                Property::new(terrain, None, 100),
            ));
            profile_for(&mut world, crate::components::PlayerId(1))
        }

        // 境界の両側と、首都しか存在しない盤面で小規模戦略の適用範囲を固定する。
        assert_eq!(
            profile_at(14, 14, Terrain::City),
            V4StrategyProfile::SmallMapExpansion
        );
        assert_eq!(
            profile_at(197, 1, Terrain::City),
            V4StrategyProfile::Standard
        );
        assert_eq!(
            profile_at(14, 14, Terrain::Capital),
            V4StrategyProfile::Standard
        );
    }

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
