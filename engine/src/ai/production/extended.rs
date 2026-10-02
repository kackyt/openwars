//! 拡張経路（小規模マップ戦略・非V4）の実装。
//!
//! 親モジュールはmain互換の実装を持ち、本文の異なる関数は
//! `crate::ai::code_path::is_extended()` でここへ委譲する。

#[allow(unused_imports)]
use super::*;

pub(super) fn remaining_campaign_requirements(
    shortfall: &IslandCampaignShortfall,
) -> Vec<CampaignProductionRequirement> {
    let mut requirements = Vec::new();
    if shortfall.heavy_transport_slots > 0 {
        requirements.push(CampaignProductionRequirement::HeavyTransport);
    }
    if shortfall.light_transport_slots > 0 {
        requirements.push(CampaignProductionRequirement::LightTransport);
    }
    if shortfall.ground_combat_units > 0 {
        // 地形上の突破未達だけを表す構造要求。敵数から固定台数を積むCombatとは分ける。
        requirements.push(CampaignProductionRequirement::GroundCombat);
    }
    if shortfall.capture_units > 0 {
        requirements.push(CampaignProductionRequirement::Capture);
    }
    if shortfall.combat_units > 0 {
        requirements.push(CampaignProductionRequirement::Combat);
    }
    requirements
}

pub(super) fn campaign_candidate_matches(
    requirement: CampaignProductionRequirement,
    unit_type: UnitType,
    stats: &UnitStats,
) -> bool {
    // 要求量を減らせない不正なマスターデータは候補から除外し、計画を確実に前進させる。
    match requirement {
        CampaignProductionRequirement::HeavyTransport => {
            unit_type == UnitType::Lander && stats.max_cargo > 0
        }
        CampaignProductionRequirement::LightTransport => {
            unit_type == UnitType::TransportHelicopter && stats.max_cargo > 0
        }
        CampaignProductionRequirement::GroundCombat => {
            matches!(
                unit_type,
                UnitType::Tank | UnitType::MdTank | UnitType::TankZ
            ) && stats.movement_type == MovementType::Tank
        }
        CampaignProductionRequirement::Capture => stats.can_capture,
        CampaignProductionRequirement::Combat => {
            stats.cost > 0
                && !stats.can_capture
                && !matches!(
                    unit_type,
                    UnitType::TransportHelicopter | UnitType::Lander | UnitType::SupplyTruck
                )
        }
    }
}

pub(super) fn consume_campaign_candidate(
    shortfall: &mut IslandCampaignShortfall,
    requirement: CampaignProductionRequirement,
    stats: &UnitStats,
) {
    match requirement {
        CampaignProductionRequirement::HeavyTransport => {
            shortfall.heavy_transport_slots = shortfall
                .heavy_transport_slots
                .saturating_sub(stats.max_cargo);
        }
        CampaignProductionRequirement::LightTransport => {
            shortfall.light_transport_slots = shortfall
                .light_transport_slots
                .saturating_sub(stats.max_cargo);
        }
        CampaignProductionRequirement::GroundCombat => {
            // 突破用のTankも通常のCombat一体として使える。同じ購入を次の
            // Combat枠で再計上すると、構造要求だけで二重生産になってしまう。
            shortfall.ground_combat_units = shortfall.ground_combat_units.saturating_sub(1);
            shortfall.combat_units = shortfall.combat_units.saturating_sub(1);
        }
        CampaignProductionRequirement::Capture => {
            shortfall.capture_units = shortfall.capture_units.saturating_sub(1);
        }
        CampaignProductionRequirement::Combat => {
            shortfall.combat_units = shortfall.combat_units.saturating_sub(1);
        }
    }
    shortfall.reserved_budget = shortfall.reserved_budget.saturating_sub(stats.cost);
}

#[allow(clippy::too_many_arguments)]
pub(super) fn plan_campaign_shortfall_production_with_damage(
    player_id: PlayerId,
    shortfalls: &[IslandCampaignShortfall],
    facilities: &[(GridPosition, Terrain)],
    available_types: &[(UnitType, UnitStats)],
    map: &crate::resources::Map,
    master_data: &MasterDataRegistry,
    available_funds: u32,
    damage_chart: Option<&DamageChart>,
) -> CampaignProductionOutcome {
    let mut rows = shortfalls.to_vec();
    rows.sort_by_key(|row| (row.priority_rank, row.island_id.0));
    let mut sorted_facilities = facilities.to_vec();
    sorted_facilities.sort_by_key(|(position, _)| (position.y, position.x));
    let mut sorted_types = available_types.to_vec();
    sorted_types
        .sort_by_key(|(unit_type, stats)| (campaign_unit_type_rank(*unit_type), stats.cost));

    let mut outcome = CampaignProductionOutcome {
        commands: Vec::new(),
        intents: Vec::new(),
        remaining_funds: available_funds,
        generic_funds: 0,
        used_facilities: std::collections::HashSet::new(),
        completed_all_rows: true,
    };
    // 上位作戦が施設不足で同一手番に完成しなくても、その残予算だけは保護した上で、
    // 競合しない施設（例: SecureのFactoryに対するExpandのAirport）を下位作戦へ使う。
    let mut protected_higher_priority_funds = 0_u32;
    let mut combat_connectivity = TerrainConnectivity::default();
    let unit_positions = std::collections::HashMap::new();
    let mut turn_distance_cache = TurnDistanceCache::default();

    'rows: for row in &mut rows {
        let mut produced_structural_assault_unit = false;
        loop {
            let requirements = remaining_campaign_requirements(row);
            if requirements.is_empty() {
                break;
            }
            if row.decision == IslandCampaignDecision::Assault
                && produced_structural_assault_unit
                && requirements == [CampaignProductionRequirement::Combat]
            {
                // 金額だけのCombat端数は輸送量を保証できないため、構造便へ後付けしない。
                outcome.completed_all_rows = false;
                break 'rows;
            }
            let mut selected = None;
            for (requirement_index, requirement) in requirements.into_iter().enumerate() {
                let mut candidates = Vec::new();
                for (facility_position, terrain) in &sorted_facilities {
                    if outcome.used_facilities.contains(facility_position) {
                        continue;
                    }
                    for (unit_type, stats) in &sorted_types {
                        if !master_data.can_produce_unit(terrain.as_str(), *unit_type)
                            || !campaign_candidate_matches(requirement, *unit_type, stats)
                            // 島嶼キャンペーンのCombat枠は、この生産関数が実在する
                            // 輸送Entity・空き枠・任務接続を保証できない。そこで地上兵を
                            // 「後で何かが運ぶ」と見込まず、施設から作戦地点へ自力到達
                            // できる候補だけを許す。渡洋支援は航空・艦船、または既に
                            // campaignへ割り当て済みの輸送可能戦力が担当する。
                            || (matches!(
                                requirement,
                                CampaignProductionRequirement::GroundCombat
                                    | CampaignProductionRequirement::Combat
                            )
                                && !combat_connectivity.is_reachable(
                                    map,
                                    master_data,
                                    (facility_position.x, facility_position.y),
                                    (row.target_position.x, row.target_position.y),
                                    stats.movement_type,
                                ))
                            || stats.cost
                                > outcome
                                    .remaining_funds
                                    .saturating_sub(protected_higher_priority_funds)
                            || stats.cost > row.reserved_budget
                        {
                            continue;
                        }
                        let combat_coverage = u32::from(matches!(
                            requirement,
                            CampaignProductionRequirement::GroundCombat
                                | CampaignProductionRequirement::Combat
                        ));
                        // Capture枠は戦闘火力で選ばない。対象へ到着してCaptureを完了する
                        // 手番を優先し、同着なら安い候補を選ぶ。火力は別のCombat計画が担う。
                        let capture_completion_turn =
                            if requirement == CampaignProductionRequirement::Capture {
                                calculate_turn_distance(
                                    map,
                                    master_data,
                                    &unit_positions,
                                    (facility_position.x, facility_position.y),
                                    (row.target_position.x, row.target_position.y),
                                    stats.movement_type,
                                    stats.max_movement,
                                    0,
                                    player_id,
                                    &mut turn_distance_cache,
                                )
                                .turns
                                .saturating_add(2)
                            } else {
                                0
                            };
                        let (covered_enemy_types, total_expected_damage) =
                            if requirement == CampaignProductionRequirement::Capture {
                                (0, 0)
                            } else {
                                damage_chart.map_or((0_u32, 0_u32), |chart| {
                                    row.priority_enemy_types.iter().fold(
                                        (0_u32, 0_u32),
                                        |(covered, total), enemy_type| {
                                            let damage = chart
                                                .get_base_damage(*unit_type, *enemy_type)
                                                .unwrap_or_default()
                                                .max(
                                                    chart
                                                        .get_base_damage_secondary(
                                                            *unit_type,
                                                            *enemy_type,
                                                        )
                                                        .unwrap_or_default(),
                                                );
                                            (
                                                covered + u32::from(damage > 0),
                                                total.saturating_add(damage),
                                            )
                                        },
                                    )
                                })
                            };
                        candidates.push((
                            capture_completion_turn,
                            std::cmp::Reverse(covered_enemy_types),
                            std::cmp::Reverse(total_expected_damage),
                            std::cmp::Reverse(combat_coverage),
                            stats.cost,
                            campaign_unit_type_rank(*unit_type),
                            facility_position.y,
                            facility_position.x,
                            *facility_position,
                            *unit_type,
                            stats,
                        ));
                    }
                }
                candidates.sort_by_key(|candidate| {
                    (
                        candidate.0,
                        candidate.1,
                        candidate.2,
                        candidate.3,
                        candidate.4,
                        candidate.5,
                        candidate.6,
                        candidate.7,
                    )
                });
                if let Some((_, _, _, _, _, _, _, _, position, unit_type, stats)) =
                    candidates.into_iter().next()
                {
                    selected = Some((
                        requirement_index > 0,
                        requirement,
                        position,
                        unit_type,
                        stats,
                    ));
                    break;
                }
            }
            let Some((used_lower_requirement, requirement, position, unit_type, stats)) = selected
            else {
                // 未完成分の資金は下位rowから隔離する。ただし施設種別が競合しない場合まで
                // 生産全体を直列化するとAirportが遊ぶため、次のcampaign rowは評価する。
                let protectable = outcome
                    .remaining_funds
                    .saturating_sub(protected_higher_priority_funds);
                protected_higher_priority_funds = protected_higher_priority_funds
                    .saturating_add(row.reserved_budget.min(protectable));
                outcome.completed_all_rows = false;
                break;
            };

            let command = ProduceUnitCommand {
                player_id,
                target_x: position.x,
                target_y: position.y,
                unit_type,
            };
            let role = match requirement {
                CampaignProductionRequirement::HeavyTransport
                | CampaignProductionRequirement::LightTransport => {
                    CampaignProductionRole::Transport
                }
                CampaignProductionRequirement::Capture => CampaignProductionRole::Capture,
                CampaignProductionRequirement::GroundCombat
                | CampaignProductionRequirement::Combat => CampaignProductionRole::Combat,
            };
            outcome.intents.push(CampaignProductionIntent {
                command: command.clone(),
                island_id: row.island_id,
                role,
                mission_target: (role == CampaignProductionRole::Capture)
                    .then_some(row.target_position),
            });
            outcome.commands.push(command);
            outcome.remaining_funds = outcome.remaining_funds.saturating_sub(stats.cost);
            outcome.used_facilities.insert(position);
            consume_campaign_candidate(row, requirement, stats);
            if row.decision == IslandCampaignDecision::Assault
                && requirement != CampaignProductionRequirement::Combat
            {
                produced_structural_assault_unit = true;
            }
            if used_lower_requirement {
                // 最優先要件の購入資金を次ターンへ残すため、同じパッケージ内の
                // 安価な前提要素を1件だけ先行購入してこの手番の計画を止める。
                outcome.completed_all_rows = false;
                break 'rows;
            }
        }
    }

    // 未完成行だけでなく未処理の下位行も含め、残った予約総額を先に隔離する。
    // 現金が予約を上回る部分だけをV4の迎撃・戦闘へ開放し、島嶼作戦を資金面で
    // 壊さずに遊休生産枠と余剰資金を活用する。
    let remaining_reservations = rows.iter().fold(0_u32, |total, row| {
        total.saturating_add(row.reserved_budget)
    });
    outcome.generic_funds = outcome
        .remaining_funds
        .saturating_sub(remaining_reservations);
    outcome
}

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
