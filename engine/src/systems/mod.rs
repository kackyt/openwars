pub mod action;
pub mod combat;
pub mod merge;
pub mod movement;
pub mod production;
pub mod property;
pub mod supply;
pub mod transport;
pub mod turn_management;

pub use action::*;
pub use combat::*;
pub use merge::*;
pub use movement::*;
pub use production::*;
pub use property::*;
pub use supply::*;
pub use transport::*;
pub use turn_management::*;

use crate::ai::code_path::{has_extended_v4, has_no_extended_v4};
use bevy_ecs::schedule::{IntoSystemConfigs, Schedule, SystemSet};

#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub struct GameSystemSet;

/// すべてのゲームロジックシステムを正しい順序でスケジュールに追加します。
/// プレゼンテーション層はこの関数を呼び出すことで、ビジネスロジックの整合性を保つことができます。
pub fn add_main_game_systems(schedule: &mut Schedule) {
    schedule.configure_sets(GameSystemSet);
    schedule.add_systems(
        (
            crate::ai::runtime::state::clear_ai_cooldowns_system,
            produce_unit_system,
            // 生産完了を一度だけ受付け、両台帳の照合を従来の発番順で適用する。
            crate::ai::v4::production_events::reconcile_production_events_system,
            move_unit_system,
            attack_unit_system,
            crate::ai::v4::deployment::audit_deployment_attacks_system,
            sync_cargo_health_system,
            remove_destroyed_units_system,
            capture_property_system,
            merge_unit_system,
            // 撃破された輸送役の搭載部隊を、勝敗判定より先に再帰的に除去する。
            cleanup_cargo_system,
            supply_unit_system,
            load_unit_system,
            unload_unit_system,
            wait_unit_system,
            (
                crate::ai::v4::victory_roadmap::audit_victory_roadmap_system
                    .run_if(has_no_extended_v4),
                crate::ai::v4::victory_roadmap::audit::audit_victory_roadmap_system
                    .run_if(has_extended_v4),
            ),
            undo_move_system,
            next_phase_system,
            victory_check_system,
        )
            .chain()
            .in_set(GameSystemSet),
    );
}

#[cfg(test)]
mod cargo_victory_tests {
    use crate::components::{CargoCapacity, Faction, Health, PlayerId, Transporting};
    use crate::resources::{GameOverCondition, MatchState, master_data::MasterDataRegistry};

    /// 本番スケジュールで輸送船が撃破されたら、入れ子の搭載兵も消え全滅勝利する。
    #[test]
    fn destroyed_nested_cargo_does_not_prevent_elimination_victory() {
        let data = MasterDataRegistry::load().unwrap();
        let (mut world, mut schedule) =
            crate::setup::initialize_world_from_master_data(&data, "map_33").unwrap();
        let existing = world
            .query::<(bevy_ecs::prelude::Entity, &Faction)>()
            .iter(&world)
            .map(|(entity, _)| entity)
            .collect::<Vec<_>>();
        for entity in existing {
            world.despawn(entity);
        }
        world.resource_mut::<MatchState>().current_turn_number.0 = 2;
        world.spawn((
            Faction(PlayerId(2)),
            Health {
                current: 100,
                max: 100,
            },
        ));
        let ship = world
            .spawn((
                Faction(PlayerId(1)),
                Health {
                    current: 0,
                    max: 100,
                },
                CargoCapacity {
                    max: 1,
                    loaded: vec![],
                },
            ))
            .id();
        let vehicle = world
            .spawn((
                Faction(PlayerId(1)),
                Health {
                    current: 100,
                    max: 100,
                },
                Transporting(ship),
                CargoCapacity {
                    max: 1,
                    loaded: vec![],
                },
            ))
            .id();
        let infantry = world
            .spawn((
                Faction(PlayerId(1)),
                Health {
                    current: 100,
                    max: 100,
                },
                Transporting(vehicle),
            ))
            .id();
        world
            .get_mut::<CargoCapacity>(ship)
            .unwrap()
            .loaded
            .push(vehicle);
        world
            .get_mut::<CargoCapacity>(vehicle)
            .unwrap()
            .loaded
            .push(infantry);
        schedule.run(&mut world);
        assert!(world.get_entity(ship).is_err());
        assert!(world.get_entity(vehicle).is_err());
        assert!(
            world.get_entity(infantry).is_err(),
            "入れ子の歩兵が孤立して残ってはならない"
        );
        assert_eq!(
            world.resource::<MatchState>().game_over,
            Some(GameOverCondition::Winner(PlayerId(2)))
        );
    }
}
