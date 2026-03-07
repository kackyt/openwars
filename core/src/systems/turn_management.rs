use crate::components::*;
use crate::events::*;
use crate::resources::*;
use bevy_ecs::prelude::*;
use std::collections::HashSet;

pub fn next_phase_system(
    _commands: Commands,
    mut match_state: ResMut<MatchState>,
    mut next_phase_events: EventReader<NextPhaseCommand>,
    mut phase_changed_events: EventWriter<GamePhaseChangedEvent>,
    mut q_units: Query<(
        Entity,
        &mut HasMoved,
        &mut ActionCompleted,
        &Faction,
        &UnitStats,
        &mut Fuel,
        &mut Ammo,
        &mut Health,
        &GridPosition,
    )>,
    mut players: ResMut<Players>,
    q_properties: Query<(&GridPosition, &Property)>,
    map: Res<Map>,
) {
    if match_state.game_over.is_some() {
        return;
    }

    for _ in next_phase_events.read() {
        match match_state.current_phase {
            Phase::Production => {
                match_state.current_phase = Phase::MovementAndAttack;
            }
            Phase::MovementAndAttack => {
                match_state.current_phase = Phase::EndTurn;

                match_state.active_player_index += 1;

                // Wrap around players
                if match_state.active_player_index >= players.0.len() {
                    match_state.active_player_index = 0;
                    match_state.current_turn_number += 1;

                    // Daily Updates (Fuel consumption, crashing)
                    for (_entity, _, _, _, stats, mut fuel, _, mut hp, pos) in q_units.iter_mut() {
                        if hp.is_destroyed() {
                            continue;
                        }
                        if stats.movement_type == MovementType::LowAltitude
                            || stats.movement_type == MovementType::HighAltitude
                        {
                            let terrain = map.get_terrain(pos.x, pos.y);
                            if terrain != Some(Terrain::Airport) {
                                if fuel.current == 0 {
                                    hp.current = 0; // Destroyed
                                } else {
                                    fuel.current =
                                        fuel.current.saturating_sub(stats.daily_fuel_consumption);
                                }
                            }
                        }
                    }
                }

                match_state.current_phase = Phase::Production;
                let active_player_id = players.0[match_state.active_player_index].id;

                // Reset flags and apply property resupply
                let mut owned_properties = HashSet::new();
                let mut city_count = 0;
                for (pos, prop) in q_properties.iter() {
                    if prop.owner_id == Some(active_player_id) {
                        if prop.terrain == Terrain::City
                            || prop.terrain == Terrain::Airport
                            || prop.terrain == Terrain::Factory
                            || prop.terrain == Terrain::Port
                            || prop.terrain == Terrain::Capital
                        {
                            owned_properties.insert((pos.x, pos.y));
                        }
                        if prop.terrain == Terrain::City || prop.terrain == Terrain::Airport {
                            city_count += 1;
                        }
                    }
                }

                // Add funds
                let budget_increase = city_count * 1000;
                let active_player_idx = players
                    .0
                    .iter()
                    .position(|p| p.id == active_player_id)
                    .unwrap();
                players.0[active_player_idx].funds += budget_increase;

                // Property resupply
                for (
                    _,
                    mut has_moved,
                    mut action_completed,
                    faction,
                    stats,
                    mut fuel,
                    mut ammo,
                    _,
                    pos,
                ) in q_units.iter_mut()
                {
                    if faction.0 == active_player_id {
                        has_moved.0 = false;
                        action_completed.0 = false;

                        if owned_properties.contains(&(pos.x, pos.y)) {
                            let ammo_diff = (stats.max_ammo1.saturating_sub(ammo.ammo1))
                                + (stats.max_ammo2.saturating_sub(ammo.ammo2));
                            let fuel_diff = stats.max_fuel.saturating_sub(fuel.current);
                            let cost = ammo_diff * 15 + fuel_diff * 5;

                            if players.0[active_player_idx].funds >= cost {
                                players.0[active_player_idx].funds -= cost;
                                fuel.current = stats.max_fuel;
                                ammo.ammo1 = stats.max_ammo1;
                                ammo.ammo2 = stats.max_ammo2;
                            }
                        }
                    }
                }

                phase_changed_events.send(GamePhaseChangedEvent {
                    new_phase: match_state.current_phase.clone(),
                    active_player: active_player_id,
                });
            }
            Phase::EndTurn => {
                match_state.current_phase = Phase::Production;
            }
        }
    }
}
