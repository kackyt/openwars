//! 拡張経路（小規模マップ戦略・非V4）の実装。
//!
//! 親モジュールはmain互換の実装を持ち、本文の異なる関数は
//! `crate::ai::code_path::is_extended()` でここへ委譲する。

#[allow(unused_imports)]
use super::*;

#[cfg(test)]
mod additional_tests {
    use super::*;
    use crate::resources::{GridTopology, Map, Terrain};

    fn campaign_test_types(master_data: &MasterDataRegistry) -> Vec<(UnitType, UnitStats)> {
        master_data
            .unit_order
            .iter()
            .filter_map(|name| master_data.create_unit_stats(name).ok())
            .map(|stats| (stats.unit_type, stats))
            .collect()
    }

    /// 既存のcampaign生産テストは同一の陸続き盤面を前提とする。実コードの
    /// 到達性引数を隠さず渡しつつ、各テストの意図を生産順・予算へ集中させる。
    fn plan_campaign_shortfall_production(
        player_id: PlayerId,
        shortfalls: &[IslandCampaignShortfall],
        facilities: &[(GridPosition, Terrain)],
        available_types: &[(UnitType, UnitStats)],
        master_data: &MasterDataRegistry,
        available_funds: u32,
    ) -> CampaignProductionOutcome {
        let map = Map::new(64, 64, Terrain::Plains, GridTopology::Square);
        super::plan_campaign_shortfall_production(
            player_id,
            shortfalls,
            facilities,
            available_types,
            &map,
            master_data,
            available_funds,
        )
    }

    #[test]
    fn campaign_production_rejects_zero_capacity_transport() {
        let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
        let master_data = MasterDataRegistry::load().unwrap();
        let mut helicopter = master_data
            .create_unit_stats(&crate::resources::master_data::UnitName(
                UnitType::TransportHelicopter.as_str().to_owned(),
            ))
            .unwrap();
        helicopter.max_cargo = 0;
        let rows = vec![crate::ai::island_campaign::IslandCampaignShortfall {
            island_id: crate::ai::islands::IslandId(0),
            decision: crate::ai::island_campaign::IslandCampaignDecision::Expand,
            target_position: GridPosition { x: 0, y: 0 },
            light_transport_slots: 1,
            heavy_transport_slots: 0,
            capture_units: 0,
            ground_combat_units: 0,
            combat_units: 0,
            reserved_budget: helicopter.cost,
            priority_rank: 0,
            priority_enemy_types: Vec::new(),
        }];

        let outcome = plan_campaign_shortfall_production(
            PlayerId(1),
            &rows,
            &[(GridPosition { x: 0, y: 0 }, Terrain::Airport)],
            &[(UnitType::TransportHelicopter, helicopter)],
            &master_data,
            u32::MAX,
        );

        assert!(outcome.commands.is_empty());
        assert!(!outcome.completed_all_rows);
    }

    #[test]
    fn campaign_production_rejects_zero_cost_combat_unit() {
        let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
        let master_data = MasterDataRegistry::load().unwrap();
        let mut infantry = master_data
            .create_unit_stats(&crate::resources::master_data::UnitName(
                UnitType::Infantry.as_str().to_owned(),
            ))
            .unwrap();
        infantry.cost = 0;
        let rows = vec![crate::ai::island_campaign::IslandCampaignShortfall {
            island_id: crate::ai::islands::IslandId(0),
            decision: crate::ai::island_campaign::IslandCampaignDecision::Defend,
            target_position: GridPosition { x: 0, y: 0 },
            light_transport_slots: 0,
            heavy_transport_slots: 0,
            capture_units: 0,
            ground_combat_units: 0,
            combat_units: 1,
            reserved_budget: 1,
            priority_rank: 0,
            priority_enemy_types: Vec::new(),
        }];

        let outcome = plan_campaign_shortfall_production(
            PlayerId(1),
            &rows,
            &[(GridPosition { x: 0, y: 0 }, Terrain::Factory)],
            &[(UnitType::Infantry, infantry)],
            &master_data,
            1,
        );

        assert!(outcome.commands.is_empty());
        assert!(!outcome.completed_all_rows);
    }

    #[test]
    fn campaign_production_exposes_only_funds_above_all_remaining_reservations() {
        let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
        let master_data = MasterDataRegistry::load().unwrap();
        let available_types = campaign_test_types(&master_data);
        let rows = vec![crate::ai::island_campaign::IslandCampaignShortfall {
            island_id: crate::ai::islands::IslandId(0),
            decision: crate::ai::island_campaign::IslandCampaignDecision::Secure,
            target_position: GridPosition { x: 0, y: 0 },
            light_transport_slots: 0,
            heavy_transport_slots: 0,
            capture_units: 1,
            ground_combat_units: 0,
            combat_units: 0,
            reserved_budget: 1_000,
            priority_rank: 1,
            priority_enemy_types: Vec::new(),
        }];
        let facilities = vec![
            (GridPosition { x: 0, y: 0 }, Terrain::Factory),
            (GridPosition { x: 1, y: 0 }, Terrain::Factory),
        ];

        let outcome = plan_campaign_shortfall_production(
            PlayerId(1),
            &rows,
            &facilities,
            &available_types,
            &master_data,
            10_000,
        );

        assert_eq!(outcome.commands.len(), 1);
        assert_eq!(outcome.commands[0].unit_type, UnitType::Infantry);
        assert_eq!(outcome.remaining_funds, 9_000);
        assert_eq!(outcome.generic_funds, 9_000);
        assert!(outcome.completed_all_rows);
    }

    #[test]
    fn campaign_combat_requires_an_affordable_non_capture_unit() {
        let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
        let master_data = MasterDataRegistry::load().unwrap();
        let available_types = campaign_test_types(&master_data);
        let facilities = vec![(GridPosition { x: 0, y: 0 }, Terrain::Factory)];
        let row =
            |combat_units, reserved_budget| crate::ai::island_campaign::IslandCampaignShortfall {
                island_id: crate::ai::islands::IslandId(0),
                decision: crate::ai::island_campaign::IslandCampaignDecision::Defend,
                target_position: GridPosition { x: 0, y: 0 },
                light_transport_slots: 0,
                heavy_transport_slots: 0,
                capture_units: 0,
                ground_combat_units: 0,
                combat_units,
                reserved_budget,
                priority_rank: 0,
                priority_enemy_types: Vec::new(),
            };

        let small_remainder = plan_campaign_shortfall_production(
            PlayerId(1),
            &[row(1, 1_000)],
            &facilities,
            &available_types,
            &master_data,
            1_000,
        );
        let large = plan_campaign_shortfall_production(
            PlayerId(1),
            &[row(2, 30_000)],
            &facilities,
            &available_types,
            &master_data,
            30_000,
        );

        assert!(small_remainder.commands.is_empty());
        assert!(!small_remainder.completed_all_rows);
        assert_eq!(large.commands.len(), 1);
        assert!(!large.completed_all_rows);
    }

    #[test]
    fn campaign_assault_production_requires_lander_and_helicopter() {
        let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
        let master_data = MasterDataRegistry::load().unwrap();
        let available_types = campaign_test_types(&master_data);
        let rows = vec![crate::ai::island_campaign::IslandCampaignShortfall {
            island_id: crate::ai::islands::IslandId(0),
            decision: crate::ai::island_campaign::IslandCampaignDecision::Assault,
            target_position: GridPosition { x: 0, y: 0 },
            light_transport_slots: 2,
            heavy_transport_slots: 2,
            capture_units: 0,
            ground_combat_units: 0,
            combat_units: 0,
            reserved_budget: 20_500,
            priority_rank: 4,
            priority_enemy_types: Vec::new(),
        }];
        let facilities = vec![
            (GridPosition { x: 0, y: 0 }, Terrain::Port),
            (GridPosition { x: 1, y: 0 }, Terrain::Airport),
        ];

        let outcome = plan_campaign_shortfall_production(
            PlayerId(1),
            &rows,
            &facilities,
            &available_types,
            &master_data,
            20_500,
        );
        let produced: std::collections::HashSet<_> = outcome
            .commands
            .iter()
            .map(|command| command.unit_type)
            .collect();

        assert_eq!(
            produced,
            std::collections::HashSet::from([UnitType::Lander, UnitType::TransportHelicopter,])
        );
        assert!(outcome.completed_all_rows);
    }

    #[test]
    fn campaign_assault_defers_combat_support_after_building_the_first_landing_wave() {
        let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
        let master_data = MasterDataRegistry::load().unwrap();
        let available_types = campaign_test_types(&master_data);
        let rows = vec![crate::ai::island_campaign::IslandCampaignShortfall {
            island_id: crate::ai::islands::IslandId(0),
            decision: crate::ai::island_campaign::IslandCampaignDecision::Assault,
            target_position: GridPosition { x: 0, y: 0 },
            light_transport_slots: 4,
            heavy_transport_slots: 0,
            capture_units: 2,
            ground_combat_units: 0,
            combat_units: 2,
            reserved_budget: 10_000,
            priority_rank: 2,
            priority_enemy_types: Vec::new(),
        }];
        let facilities = vec![
            (GridPosition { x: 0, y: 0 }, Terrain::Airport),
            (GridPosition { x: 1, y: 0 }, Terrain::Airport),
            (GridPosition { x: 2, y: 0 }, Terrain::Factory),
            (GridPosition { x: 3, y: 0 }, Terrain::Factory),
            (GridPosition { x: 4, y: 0 }, Terrain::Factory),
        ];

        let outcome = plan_campaign_shortfall_production(
            PlayerId(1),
            &rows,
            &facilities,
            &available_types,
            &master_data,
            20_200,
        );

        assert_eq!(outcome.commands.len(), 4);
        assert_eq!(
            outcome
                .commands
                .iter()
                .filter(|command| command.unit_type == UnitType::TransportHelicopter)
                .count(),
            2
        );
        assert_eq!(
            outcome
                .commands
                .iter()
                .filter(|command| command.unit_type == UnitType::Infantry)
                .count(),
            2
        );
        assert_eq!(outcome.remaining_funds, 10_200);
        assert!(!outcome.completed_all_rows);
    }

    /// Lander資金が足りない初手でも同じパッケージの輸送ヘリを1件だけ先行し、
    /// 残金を次ターンのLander購入へ温存する。
    #[test]
    fn campaign_assault_produces_one_affordable_prerequisite_while_saving_for_lander() {
        let _code_path = crate::ai::code_path::enter(crate::ai::code_path::CodePath::Extended);
        let master_data = MasterDataRegistry::load().unwrap();
        let available_types = campaign_test_types(&master_data);
        let rows = vec![crate::ai::island_campaign::IslandCampaignShortfall {
            island_id: crate::ai::islands::IslandId(0),
            decision: crate::ai::island_campaign::IslandCampaignDecision::Assault,
            target_position: GridPosition { x: 0, y: 0 },
            light_transport_slots: 2,
            heavy_transport_slots: 2,
            capture_units: 2,
            ground_combat_units: 0,
            combat_units: 2,
            reserved_budget: 22_500,
            priority_rank: 2,
            priority_enemy_types: Vec::new(),
        }];
        let facilities = vec![
            (GridPosition { x: 0, y: 0 }, Terrain::Port),
            (GridPosition { x: 1, y: 0 }, Terrain::Airport),
            (GridPosition { x: 2, y: 0 }, Terrain::Factory),
        ];

        let outcome = plan_campaign_shortfall_production(
            PlayerId(1),
            &rows,
            &facilities,
            &available_types,
            &master_data,
            14_000,
        );

        assert_eq!(outcome.commands.len(), 1);
        assert_eq!(outcome.commands[0].unit_type, UnitType::TransportHelicopter);
        assert_eq!(outcome.remaining_funds, 10_000);
        assert!(!outcome.completed_all_rows);
    }
}
