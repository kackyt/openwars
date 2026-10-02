//! AIの実装経路（main互換 / 拡張）の選択。
//!
//! 標準マップのV4はmain時点と同じ実装で動かし、小規模マップ戦略や他バージョンの
//! AIは拡張実装で動かす。本文が異なる関数は先頭で [`is_extended`] を見て
//! 各モジュールの `extended` 子モジュールへ委譲する。
//!
//! 経路はAIの入口（手番の実行・イベント処理）で [`enter`] によって確定させる。
//! 純粋な計算関数まで経路を引数で渡すとmain互換側の本文が変わってしまうため、
//! スレッドローカルで保持する。並列評価では [`current`] を取り出して各ワーカーへ
//! 引き継ぐ（`deterministic_parallel` を参照）。

use crate::ai::ai_version::AiVersion;
use crate::ai::strategy_profile::{V4StrategyProfile, profile_for};
use crate::components::PlayerId;
use bevy_ecs::prelude::{Res, Resource, World};
use std::cell::Cell;
use std::collections::HashSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodePath {
    /// main時点と同じ実装。標準マップのV4が使う。
    MainCompatible,
    /// 小規模マップ戦略などの拡張実装。V4以外と小規模マップのV4が使う。
    Extended,
}

thread_local! {
    // 入口で設定されない呼び出し（他バージョンのAI・単体テスト）は拡張実装を使う。
    static CURRENT: Cell<CodePath> = const { Cell::new(CodePath::Extended) };
}

/// 現在の実装経路。
pub fn current() -> CodePath {
    CURRENT.with(Cell::get)
}

/// 拡張実装で動いているか。
pub fn is_extended() -> bool {
    current() == CodePath::Extended
}

/// main互換経路で手番を実行したプレイヤー。
///
/// V4の台帳を更新するイベント処理システムは、main互換版と拡張版の一方だけを動かす。
/// V4の経路は盤面で決まるため、同じ対局のV4は全員同じ経路になる。
#[derive(Resource, Debug, Default)]
pub struct CodePathRegistry {
    main_compatible_players: HashSet<PlayerId>,
}

/// プレイヤーのAIバージョンと盤面から実装経路を決め、台帳へ記録する。
pub fn for_player(world: &mut World, player_id: PlayerId) -> CodePath {
    let is_v4 = crate::ai::resolve_player_ai_version(world, player_id) == AiVersion::V4;
    if is_v4 && profile_for(world, player_id) == V4StrategyProfile::Standard {
        world
            .get_resource_or_insert_with(CodePathRegistry::default)
            .main_compatible_players
            .insert(player_id);
        CodePath::MainCompatible
    } else {
        CodePath::Extended
    }
}

/// main互換経路のV4が対局にいるか（V4台帳のシステム選択条件）。
pub fn has_main_compatible_v4(registry: Option<Res<CodePathRegistry>>) -> bool {
    registry.is_some_and(|registry| !registry.main_compatible_players.is_empty())
}

/// 拡張経路のV4台帳システムを動かすか。
pub fn has_no_main_compatible_v4(registry: Option<Res<CodePathRegistry>>) -> bool {
    !has_main_compatible_v4(registry)
}

/// 破棄時に直前の経路へ戻すガード。
#[must_use = "ガードを保持している間だけ経路が有効になる"]
pub struct CodePathGuard {
    previous: CodePath,
}

impl Drop for CodePathGuard {
    fn drop(&mut self) {
        CURRENT.with(|cell| cell.set(self.previous));
    }
}

/// 経路を設定し、ガードの破棄時に元へ戻す。
pub fn enter(path: CodePath) -> CodePathGuard {
    let previous = CURRENT.with(|cell| cell.replace(path));
    CodePathGuard { previous }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::ai_version::PlayerAiSettings;
    use crate::resources::GridTopology;
    use crate::resources::master_data::MasterDataRegistry;

    fn world_for(map_id: &str, version: AiVersion) -> World {
        let master_data = MasterDataRegistry::load().unwrap();
        let (mut world, _) = crate::setup::initialize_world_from_master_data_with_topology(
            &master_data,
            map_id,
            GridTopology::Hex,
        )
        .unwrap();
        let mut settings = PlayerAiSettings::default();
        settings.set_version(PlayerId(1), version);
        world.insert_resource(settings);
        world
    }

    /// 標準マップのV4だけがmain互換経路を使い、台帳システムの選択にも反映される。
    #[test]
    fn only_standard_map_v4_uses_main_compatible_path() {
        let mut standard = world_for("map_6", AiVersion::V4);
        assert_eq!(
            for_player(&mut standard, PlayerId(1)),
            CodePath::MainCompatible
        );
        assert!(
            standard
                .resource::<CodePathRegistry>()
                .main_compatible_players
                .contains(&PlayerId(1))
        );

        let mut small = world_for("map_1", AiVersion::V4);
        assert_eq!(for_player(&mut small, PlayerId(1)), CodePath::Extended);
        assert!(small.get_resource::<CodePathRegistry>().is_none());

        let mut v3 = world_for("map_6", AiVersion::V3);
        assert_eq!(for_player(&mut v3, PlayerId(1)), CodePath::Extended);
    }

    /// ガードを抜けると直前の経路へ戻る（入れ子でも崩れない）。
    #[test]
    fn guard_restores_previous_path() {
        assert_eq!(current(), CodePath::Extended);
        {
            let _outer = enter(CodePath::MainCompatible);
            assert!(!is_extended());
            {
                let _inner = enter(CodePath::Extended);
                assert!(is_extended());
            }
            assert!(!is_extended());
        }
        assert_eq!(current(), CodePath::Extended);
    }

    /// 並列評価のワーカーへ呼び出し元の経路が引き継がれる。
    #[test]
    fn parallel_workers_inherit_caller_path() {
        let _path = enter(CodePath::MainCompatible);
        let paths =
            crate::ai::deterministic_parallel::map_ordered((0..16).collect(), |_: u32| current());
        assert!(paths.iter().all(|path| *path == CodePath::MainCompatible));
    }
}
