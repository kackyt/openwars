use bevy_ecs::prelude::*;
use openwars_engine::components::{GridPosition, PlayerId, Property};
use openwars_engine::resources::master_data::MasterDataRegistry;
use openwars_engine::resources::{GridTopology, Map, MatchState, Player, Players, Terrain};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CurrentScreen {
    MapSelection,
    InGame,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InGameState {
    Normal,
    UnitSelected {
        unit_entity: Entity,
        start_pos: (usize, usize),
        reachable_tiles: std::collections::HashSet<(usize, usize)>,
    },
    ActionMenu {
        unit_entity: Option<Entity>,
        options: Vec<String>,
        selected_index: usize,
    },
    ProductionMenu {
        factory_pos: (usize, usize),
        options: Vec<String>,
        selected_index: usize,
    },
    TargetSelection {
        unit_entity: Entity,
        action: String,
        targets: Vec<(usize, usize)>,
        selected_index: usize,
    },
    CargoSelection {
        transport_entity: Entity,
        passengers: Vec<Entity>,
        selected_index: usize,
    },
    DropTargetSelection {
        transport_entity: Entity,
        cargo_entity: Entity,
        targets: Vec<(usize, usize)>,
        selected_index: usize,
    },
    EventPopup {
        message: String,
    },
}

pub struct UiState {
    pub current_screen: CurrentScreen,
    pub in_game_state: InGameState,
    pub selected_map_index: usize,
    pub available_maps: Vec<String>,
    // In-game state
    pub cursor_pos: (usize, usize),
    pub log_messages: Vec<String>,
}

impl UiState {
    pub fn new(maps: Vec<String>) -> Self {
        Self {
            current_screen: CurrentScreen::MapSelection,
            in_game_state: InGameState::Normal,
            selected_map_index: 0,
            available_maps: maps,
            cursor_pos: (0, 0),
            log_messages: Vec::new(),
        }
    }

    pub fn add_log(&mut self, msg: String) {
        self.log_messages.push(msg);
        if self.log_messages.len() > 10 {
            self.log_messages.remove(0);
        }
    }
}

pub struct App {
    pub master_data: MasterDataRegistry,
    pub world: Option<World>,
    pub schedule: Option<Schedule>,
    pub ui_state: UiState,
    pub should_quit: bool,
}

impl App {
    pub fn new() -> anyhow::Result<Self> {
        let master_data = MasterDataRegistry::load()?;
        let mut map_names: Vec<String> = master_data.maps.keys().cloned().collect();
        map_names.sort();

        Ok(Self {
            master_data,
            world: None,
            schedule: None,
            ui_state: UiState::new(map_names),
            should_quit: false,
        })
    }

    pub fn handle_map_selection_key(&mut self, key: crossterm::event::KeyEvent) {
        use crossterm::event::KeyCode;

        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => self.should_quit = true,
            KeyCode::Up | KeyCode::Char('k') => {
                if self.ui_state.selected_map_index > 0 {
                    self.ui_state.selected_map_index -= 1;
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.ui_state.selected_map_index
                    < self.ui_state.available_maps.len().saturating_sub(1)
                {
                    self.ui_state.selected_map_index += 1;
                }
            }
            KeyCode::Enter | KeyCode::Char(' ') => {
                // Determine the selected map
                let map_name = self
                    .ui_state
                    .available_maps
                    .get(self.ui_state.selected_map_index)
                    .cloned();
                if let Some(map_name) = map_name {
                    // Transition to in-game
                    if let Err(e) = self.initialize_world(map_name.clone()) {
                        self.ui_state.add_log(format!("Map load error: {}", e));
                    } else {
                        self.ui_state.current_screen = CurrentScreen::InGame;
                        self.ui_state.in_game_state = InGameState::Normal;
                        self.ui_state.cursor_pos = (0, 0);
                        self.ui_state.add_log(format!("Map '{}' loaded.", map_name));
                    }
                }
            }
            _ => {}
        }
    }

    pub fn handle_in_game_key(&mut self, key: crossterm::event::KeyEvent) {
        use crossterm::event::KeyCode;

        match key.code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Esc => {
                // Back to map selection
                self.world = None;
                self.schedule = None;
                self.ui_state.current_screen = CurrentScreen::MapSelection;
                self.ui_state.in_game_state = InGameState::Normal;
                self.ui_state.cursor_pos = (0, 0);
            }
            KeyCode::Up
            | KeyCode::Char('k')
            | KeyCode::Down
            | KeyCode::Char('j')
            | KeyCode::Left
            | KeyCode::Char('h')
            | KeyCode::Right
            | KeyCode::Char('l') => self.handle_navigation_key(key.code),
            KeyCode::Char(' ') | KeyCode::Enter => self.handle_action_key(),
            _ => {}
        }
    }

    fn handle_navigation_key(&mut self, code: crossterm::event::KeyCode) {
        use crossterm::event::KeyCode;
        match code {
            KeyCode::Up | KeyCode::Char('k') => match &mut self.ui_state.in_game_state {
                InGameState::ActionMenu { selected_index, .. }
                | InGameState::ProductionMenu { selected_index, .. }
                | InGameState::CargoSelection { selected_index, .. } => {
                    if *selected_index > 0 {
                        *selected_index -= 1;
                    }
                }
                InGameState::EventPopup { .. } => {}
                _ => {
                    if self.ui_state.cursor_pos.1 > 0 {
                        self.ui_state.cursor_pos.1 -= 1;
                    }
                }
            },
            KeyCode::Down | KeyCode::Char('j') => match &mut self.ui_state.in_game_state {
                InGameState::ActionMenu {
                    selected_index,
                    options,
                    ..
                }
                | InGameState::ProductionMenu {
                    selected_index,
                    options,
                    ..
                } => {
                    if *selected_index < options.len().saturating_sub(1) {
                        *selected_index += 1;
                    }
                }
                InGameState::CargoSelection {
                    selected_index,
                    passengers,
                    ..
                } => {
                    if *selected_index < passengers.len().saturating_sub(1) {
                        *selected_index += 1;
                    }
                }
                InGameState::EventPopup { .. } => {}
                _ => {
                    if let Some(world) = &self.world
                        && let Some(map) = world.get_resource::<openwars_engine::resources::Map>()
                        && self.ui_state.cursor_pos.1 < map.height.saturating_sub(1)
                    {
                        self.ui_state.cursor_pos.1 += 1;
                    }
                }
            },
            KeyCode::Left | KeyCode::Char('h') => {
                if !matches!(
                    self.ui_state.in_game_state,
                    InGameState::ActionMenu { .. }
                        | InGameState::ProductionMenu { .. }
                        | InGameState::CargoSelection { .. }
                        | InGameState::EventPopup { .. }
                ) && self.ui_state.cursor_pos.0 > 0
                {
                    self.ui_state.cursor_pos.0 -= 1;
                }
            }
            KeyCode::Right | KeyCode::Char('l') => {
                if !matches!(
                    self.ui_state.in_game_state,
                    InGameState::ActionMenu { .. }
                        | InGameState::ProductionMenu { .. }
                        | InGameState::CargoSelection { .. }
                        | InGameState::EventPopup { .. }
                ) && let Some(world) = &self.world
                    && let Some(map) = world.get_resource::<openwars_engine::resources::Map>()
                    && self.ui_state.cursor_pos.0 < map.width.saturating_sub(1)
                {
                    self.ui_state.cursor_pos.0 += 1;
                }
            }
            _ => {}
        }
    }

    fn handle_action_key(&mut self) {
        let state_clone = self.ui_state.in_game_state.clone();
        match state_clone {
            InGameState::Normal => self.handle_normal_confirm(),
            InGameState::ActionMenu {
                unit_entity,
                options,
                selected_index,
            } => self.handle_action_menu_selection(unit_entity, options, selected_index),
            InGameState::ProductionMenu {
                factory_pos,
                options,
                selected_index,
            } => self.handle_production_menu_selection(factory_pos, options, selected_index),
            InGameState::TargetSelection {
                unit_entity,
                action,
                ..
            } => self.handle_target_selection_confirm(unit_entity, action),
            InGameState::UnitSelected {
                unit_entity,
                start_pos,
                reachable_tiles,
            } => self.handle_unit_selected_confirm(unit_entity, start_pos, reachable_tiles),
            InGameState::CargoSelection {
                transport_entity,
                passengers,
                selected_index,
            } => {
                let passenger = passengers[selected_index];
                self.ui_state.in_game_state = InGameState::DropTargetSelection {
                    transport_entity,
                    cargo_entity: passenger,
                    targets: vec![],
                    selected_index: 0,
                };
                self.ui_state
                    .add_log("Select target tile to drop...".to_string());
            }
            InGameState::DropTargetSelection {
                transport_entity,
                cargo_entity,
                ..
            } => self.handle_drop_target_confirm(transport_entity, cargo_entity),
            InGameState::EventPopup { .. } => {
                self.ui_state.in_game_state = InGameState::Normal;
            }
        }
    }
    fn handle_normal_confirm(&mut self) {
        let mut options = vec!["End Turn".to_string(), "Cancel".to_string()];
        let mut selected_unit = None;

        if let Some(world) = &mut self.world {
            let cx = self.ui_state.cursor_pos.0;
            let cy = self.ui_state.cursor_pos.1;

            if let (Some(match_state), Some(players)) = (
                world.get_resource::<openwars_engine::resources::MatchState>(),
                world.get_resource::<openwars_engine::resources::Players>(),
            ) {
                let active_player_id = players.0[match_state.active_player_index.0].id;

                let mut u_query = world.query::<(
                    Entity,
                    &openwars_engine::components::GridPosition,
                    &openwars_engine::components::Faction,
                    &openwars_engine::components::ActionCompleted,
                    Option<&openwars_engine::components::HasMoved>,
                )>();
                for (entity, pos, faction, action_completed, has_moved) in u_query.iter(world) {
                    if pos.x == cx
                        && pos.y == cy
                        && faction.0 == active_player_id
                        && !action_completed.0
                        && !has_moved.map(|h| h.0).unwrap_or(false)
                    {
                        selected_unit = Some(entity);
                    }
                }

                if let Some(entity) = selected_unit {
                    let mut reachable = std::collections::HashSet::new();
                    let mut u_stats = None;
                    let mut fuel_cur = 0;

                    if let Ok((st, f)) = world
                        .query::<(
                            &openwars_engine::components::UnitStats,
                            &openwars_engine::components::Fuel,
                        )>()
                        .get(world, entity)
                    {
                        u_stats = Some((st.movement_type, st.max_movement, st.unit_type));
                        fuel_cur = f.current;
                    }

                    let mut unit_positions = std::collections::HashMap::new();
                    let mut q_all = world.query::<(
                        &openwars_engine::components::GridPosition,
                        &openwars_engine::components::Faction,
                        &openwars_engine::components::UnitStats,
                        Option<&openwars_engine::components::CargoCapacity>,
                    )>();
                    for (p, f, s, c) in q_all.iter(world) {
                        let free_slots = c
                            .map(|c| c.max.saturating_sub(c.loaded.len() as u32))
                            .unwrap_or(0);
                        unit_positions.insert(
                            (p.x, p.y),
                            openwars_engine::systems::movement::OccupantInfo {
                                player_id: f.0,
                                is_transport: s.max_cargo > 0,
                                loadable_types: s.loadable_unit_types.clone(),
                                free_slots,
                            },
                        );
                    }

                    if let (Some(map), Some((m_type, max_mov, u_type))) = (
                        world.get_resource::<openwars_engine::resources::Map>(),
                        u_stats,
                    ) {
                        reachable = openwars_engine::systems::movement::calculate_reachable_tiles(
                            map,
                            &unit_positions,
                            (cx, cy),
                            m_type,
                            max_mov,
                            fuel_cur,
                            active_player_id,
                            u_type,
                            &self.master_data,
                        );
                    }

                    self.ui_state.in_game_state = InGameState::UnitSelected {
                        unit_entity: entity,
                        start_pos: (cx, cy),
                        reachable_tiles: reachable,
                    };
                    self.ui_state
                        .add_log(format!("Selected unit at {:?}", (cx, cy)));
                    return;
                }

                let mut p_query = world.query::<(
                    &openwars_engine::components::GridPosition,
                    &openwars_engine::components::Property,
                )>();
                let mut is_factory = false;
                let mut capital_pos = None;
                for (pos, prop) in p_query.iter(world) {
                    if prop.owner_id == Some(active_player_id) {
                        if pos.x == cx && pos.y == cy {
                            let landscape_name = prop.terrain.as_str();
                            if self.master_data.is_production_facility(landscape_name) {
                                is_factory = true;
                            }
                        }
                        if prop.terrain == openwars_engine::resources::Terrain::Capital {
                            capital_pos = Some(*pos);
                        }
                    }
                }

                if is_factory {
                    if openwars_engine::systems::production::is_within_production_range(
                        capital_pos,
                        cx,
                        cy,
                    ) {
                        options.insert(0, "Produce".to_string());
                    } else {
                        self.ui_state
                            .add_log("Too far from Capital to produce!".to_string());
                    }
                }
            }
        }

        self.ui_state.in_game_state = InGameState::ActionMenu {
            unit_entity: None,
            options,
            selected_index: 0,
        };
    }

    fn handle_action_menu_selection(
        &mut self,
        unit_entity: Option<Entity>,
        options: Vec<String>,
        selected_index: usize,
    ) {
        let selected = &options[selected_index];
        if selected == "Cancel" {
            self.ui_state.in_game_state = InGameState::Normal;
        } else if selected == "End Turn" {
            self.ui_state.in_game_state = InGameState::Normal;
            self.ui_state.add_log("Turn ended.".to_string());

            if let Some(world) = &mut self.world {
                world.send_event(openwars_engine::events::NextPhaseCommand);
            }
        } else if selected == "Produce" {
            let mut options = Vec::new();
            if let Some(world) = &mut self.world {
                let mut player_funds = 0;

                if let (Some(match_state), Some(players)) = (
                    world.get_resource::<openwars_engine::resources::MatchState>(),
                    world.get_resource::<openwars_engine::resources::Players>(),
                ) {
                    player_funds = players.0[match_state.active_player_index.0].funds;
                }

                let mut landscape_name = "平地";
                let mut p_query = world.query::<(
                    &openwars_engine::components::GridPosition,
                    &openwars_engine::components::Property,
                )>();
                for (pos, prop) in p_query.iter(world) {
                    if pos.x == self.ui_state.cursor_pos.0 && pos.y == self.ui_state.cursor_pos.1 {
                        landscape_name = prop.terrain.as_str();
                    }
                }

                let mut sorted_names: Vec<_> = self.master_data.units.keys().cloned().collect();
                sorted_names.sort_by(|a, b| a.0.cmp(&b.0));
                for name in sorted_names {
                    if let Some(record) = self.master_data.units.get(&name) {
                        if player_funds < record.cost {
                            continue;
                        }

                        if self
                            .master_data
                            .can_produce_unit(landscape_name, record.movement_type)
                        {
                            options.push(name.0.clone());
                        }
                    }
                }
            }
            options.push("Cancel".to_string());

            self.ui_state.in_game_state = InGameState::ProductionMenu {
                factory_pos: self.ui_state.cursor_pos,
                options,
                selected_index: 0,
            };
        } else if let Some(entity) = unit_entity {
            if selected == "Wait" {
                if let Some(world) = &mut self.world {
                    world.send_event(openwars_engine::events::WaitUnitCommand {
                        unit_entity: entity,
                    });
                }
                self.ui_state.in_game_state = InGameState::Normal;
                self.ui_state.add_log("Unit waited.".to_string());
            } else if selected == "Capture" {
                if let Some(world) = &mut self.world {
                    world.send_event(openwars_engine::events::CapturePropertyCommand {
                        unit_entity: entity,
                    });
                }
                self.ui_state.in_game_state = InGameState::Normal;
                self.ui_state.add_log("Capture initiated.".to_string());
            } else if selected == "Attack" {
                self.ui_state.in_game_state = InGameState::TargetSelection {
                    unit_entity: entity,
                    action: "Attack".to_string(),
                    targets: vec![],
                    selected_index: 0,
                };
                self.ui_state
                    .add_log("Select target to attack...".to_string());
            } else if selected == "Drop" {
                let mut passengers = vec![];
                if let Some(world) = &mut self.world {
                    let mut q = world.query::<&openwars_engine::components::CargoCapacity>();
                    if let Ok(cargo) = q.get(world, entity) {
                        passengers = cargo.loaded.clone();
                    }
                }
                if passengers.is_empty() {
                    self.ui_state.add_log("No passengers to drop.".to_string());
                } else {
                    self.ui_state.in_game_state = InGameState::CargoSelection {
                        transport_entity: entity,
                        passengers,
                        selected_index: 0,
                    };
                }
            } else if selected == "Supply" || selected == "Join" || selected == "Load" {
                self.ui_state.in_game_state = InGameState::TargetSelection {
                    unit_entity: entity,
                    action: selected.clone(),
                    targets: vec![],
                    selected_index: 0,
                };
                self.ui_state
                    .add_log(format!("Select target/tile for {}...", selected));
            }
        }
    }

    fn handle_production_menu_selection(
        &mut self,
        factory_pos: (usize, usize),
        options: Vec<String>,
        selected_index: usize,
    ) {
        let selected = &options[selected_index];
        if selected == "Cancel" {
            self.ui_state.in_game_state = InGameState::Normal;
        } else {
            if let Some(world) = &mut self.world
                && let (Some(match_state), Some(players)) = (
                    world.get_resource::<openwars_engine::resources::MatchState>(),
                    world.get_resource::<openwars_engine::resources::Players>(),
                )
            {
                let active_player_id = players.0[match_state.active_player_index.0].id;
                let Some(unit_type) = openwars_engine::resources::UnitType::from_str(selected)
                else {
                    self.ui_state
                        .add_log(format!("未対応のユニット種別です: {}", selected));
                    self.ui_state.in_game_state = InGameState::Normal;
                    return;
                };
                world.send_event(openwars_engine::events::ProduceUnitCommand {
                    player_id: active_player_id,
                    target_x: factory_pos.0,
                    target_y: factory_pos.1,
                    unit_type,
                });
                self.ui_state.add_log(format!(
                    "{} を生産しました。次ターンから行動可能です。(位置: {:?})",
                    selected, factory_pos
                ));
            }
            self.ui_state.in_game_state = InGameState::Normal;
        }
    }

    fn handle_target_selection_confirm(&mut self, unit_entity: Entity, action: String) {
        let cx = self.ui_state.cursor_pos.0;
        let cy = self.ui_state.cursor_pos.1;

        if let Some(world) = &mut self.world {
            let mut target_unit = None;
            let mut q = world.query::<(Entity, &openwars_engine::components::GridPosition)>();
            for (e, pos) in q.iter(world) {
                if pos.x == cx && pos.y == cy && e != unit_entity {
                    target_unit = Some(e);
                }
            }

            if action == "Attack" {
                if let Some(target) = target_unit {
                    match openwars_engine::systems::combat::can_attack(unit_entity, target, world) {
                        Ok(()) => {
                            world.send_event(openwars_engine::events::AttackUnitCommand {
                                attacker_entity: unit_entity,
                                defender_entity: target,
                            });
                            self.ui_state
                                .add_log(format!("Attacking target at {:?}", (cx, cy)));
                        }
                        Err(e) => {
                            self.ui_state.add_log(format!("Attack cancelled: {}", e));
                        }
                    }
                } else {
                    self.ui_state
                        .add_log("No target there. Cancelled.".to_string());
                }
            } else if action == "Supply" {
                if let Some(target) = target_unit {
                    world.send_event(openwars_engine::events::SupplyUnitCommand {
                        supplier_entity: unit_entity,
                        target_entity: target,
                    });
                    self.ui_state
                        .add_log(format!("Supplying unit at {:?}", (cx, cy)));
                } else {
                    self.ui_state
                        .add_log("No logic for supply. Cancelled.".to_string());
                }
            } else if action == "Join" {
                if let Some(target) = target_unit {
                    world.send_event(openwars_engine::events::MergeUnitCommand {
                        source_entity: unit_entity,
                        target_entity: target,
                    });
                    self.ui_state
                        .add_log(format!("Joining unit at {:?}", (cx, cy)));
                } else {
                    self.ui_state
                        .add_log("No logic for join. Cancelled.".to_string());
                }
            } else if action == "Load" {
                if let Some(target) = target_unit {
                    world.send_event(openwars_engine::events::LoadUnitCommand {
                        transport_entity: target,
                        unit_entity,
                    });
                    self.ui_state
                        .add_log(format!("Loading into transport at {:?}", (cx, cy)));
                } else {
                    self.ui_state
                        .add_log("No transport at target. Cancelled.".to_string());
                }
            }
        }
        self.ui_state.in_game_state = InGameState::Normal;
    }

    fn handle_unit_selected_confirm(
        &mut self,
        unit_entity: Entity,
        _start_pos: (usize, usize),
        reachable_tiles: std::collections::HashSet<(usize, usize)>,
    ) {
        let cx = self.ui_state.cursor_pos.0;
        let cy = self.ui_state.cursor_pos.1;

        if !reachable_tiles.contains(&(cx, cy)) {
            self.ui_state
                .add_log("Target is out of movement range.".to_string());
            self.ui_state.in_game_state = InGameState::Normal;
        } else {
            if let Some(world) = &mut self.world {
                world.send_event(openwars_engine::events::MoveUnitCommand {
                    unit_entity,
                    target_x: cx,
                    target_y: cy,
                });

                // --- アクションメニューの動的生成 ---
                let mut options = vec!["Wait".to_string()];

                if let Ok((stats, faction)) = world
                    .query::<(
                        &openwars_engine::components::UnitStats,
                        &openwars_engine::components::Faction,
                    )>()
                    .get(world, unit_entity)
                {
                    // 借用エラー回避のため必要な情報を取得
                    let stats_can_capture = stats.can_capture;
                    let stats_can_supply = stats.can_supply;
                    let stats_max_cargo = stats.max_cargo;
                    let stats_min_range = stats.min_range;
                    let stats_max_range = stats.max_range;
                    let unit_faction = faction.0;

                    // 攻撃可能か判定
                    let mut can_atk = false;
                    let mut q_targets =
                        world.query::<(Entity, &openwars_engine::components::GridPosition)>();
                    for (target_ent, target_pos) in q_targets.iter(world) {
                        if target_ent != unit_entity {
                            let dist = (cx as i64 - target_pos.x as i64).unsigned_abs() as u32
                                + (cy as i64 - target_pos.y as i64).unsigned_abs() as u32;
                            if dist >= stats_min_range && dist <= stats_max_range {
                                // 間接攻撃ユニットは、移動したターンには攻撃できない(仕様)
                                if stats_min_range > 1 && (cx, cy) != _start_pos {
                                    continue;
                                }
                                can_atk = true;
                                break;
                            }
                        }
                    }
                    if can_atk {
                        options.insert(0, "Attack".to_string());
                    }

                    if stats_can_capture {
                        // 現在地が占領可能な施設か判定
                        let mut q_prop = world.query::<(
                            &openwars_engine::components::GridPosition,
                            &openwars_engine::components::Property,
                        )>();
                        for (p_pos, p_prop) in q_prop.iter(world) {
                            if p_pos.x == cx && p_pos.y == cy {
                                // 自軍の拠点以外なら占領可能
                                if p_prop.owner_id != Some(unit_faction) {
                                    options.push("Capture".to_string());
                                }
                            }
                        }
                    }

                    if stats_can_supply {
                        options.push("Supply".to_string());
                    }

                    if stats_max_cargo > 0 {
                        let mut has_passengers = false;
                        if let Ok(cargo) = world
                            .query::<&openwars_engine::components::CargoCapacity>()
                            .get(world, unit_entity)
                        {
                            has_passengers = !cargo.loaded.is_empty();
                        }
                        if has_passengers {
                            options.push("Drop".to_string());
                        }
                        options.push("Load".to_string());
                    }

                    options.push("Join".to_string());
                }

                options.push("Cancel".to_string());

                self.ui_state.in_game_state = InGameState::ActionMenu {
                    unit_entity: Some(unit_entity),
                    options,
                    selected_index: 0,
                };
            }
            self.ui_state
                .add_log(format!("Moved unit to {:?}", (cx, cy)));
        }
    }

    fn handle_drop_target_confirm(&mut self, transport_entity: Entity, cargo_entity: Entity) {
        let cx = self.ui_state.cursor_pos.0;
        let cy = self.ui_state.cursor_pos.1;
        if let Some(world) = &mut self.world {
            world.send_event(openwars_engine::events::UnloadUnitCommand {
                transport_entity,
                cargo_entity,
                target_x: cx,
                target_y: cy,
            });
        }
        self.ui_state
            .add_log(format!("Dropping passenger at {:?}", (cx, cy)));
        self.ui_state.in_game_state = InGameState::Normal;
    }

    pub fn handle_key(&mut self, key: crossterm::event::KeyEvent) {
        match self.ui_state.current_screen {
            CurrentScreen::MapSelection => self.handle_map_selection_key(key),
            CurrentScreen::InGame => self.handle_in_game_key(key),
        }
    }

    fn initialize_world(&mut self, map_name: String) -> anyhow::Result<()> {
        use openwars_engine::events::*;
        use openwars_engine::systems::*;

        let mut world = World::new();
        let mut schedule = Schedule::default();

        // Register events
        world.init_resource::<Events<ProduceUnitCommand>>();
        world.init_resource::<Events<MoveUnitCommand>>();
        world.init_resource::<Events<AttackUnitCommand>>();
        world.init_resource::<Events<CapturePropertyCommand>>();
        world.init_resource::<Events<MergeUnitCommand>>();
        world.init_resource::<Events<SupplyUnitCommand>>();
        world.init_resource::<Events<LoadUnitCommand>>();
        world.init_resource::<Events<UnloadUnitCommand>>();
        world.init_resource::<Events<WaitUnitCommand>>();
        world.init_resource::<Events<NextPhaseCommand>>();

        world.init_resource::<Events<UnitMovedEvent>>();
        world.init_resource::<Events<UnitAttackedEvent>>();
        world.init_resource::<Events<UnitDestroyedEvent>>();
        world.init_resource::<Events<UnitMergedEvent>>();
        world.init_resource::<Events<PropertyCapturedEvent>>();
        world.init_resource::<Events<GamePhaseChangedEvent>>();
        world.init_resource::<Events<GameOverEvent>>();

        // Add event clearing systems
        // Intentionally skipping manual event clearance (update_system) to avoid Bevy version disparities.
        // EventReader correctly tracks indices, so old events won't be reprocessed.

        // Add game logic systems (order is important for game loop, but default parallel works for independent ones)
        // Note: engine systems are mostly command -> event processors.
        schedule.add_systems(
            (
                produce_unit_system,
                move_unit_system,
                attack_unit_system,
                remove_destroyed_units_system,
                capture_property_system,
                merge_unit_system,
                supply_unit_system,
                load_unit_system,
                unload_unit_system,
                wait_unit_system,
                next_phase_system,
                daily_update_system,
            )
                .chain(),
        );

        // Build UnitRegistry and DamageChart from MasterDataRegistry
        let mut damage_chart = openwars_engine::resources::DamageChart::new();
        for (unit_name, unit_record) in &self.master_data.units {
            if let Some(att_type) = openwars_engine::resources::UnitType::from_str(&unit_name.0) {
                if let Some(w1_name) = &unit_record.weapon1
                    && let Some(weapon) = self.master_data.weapons.get(
                        &openwars_engine::resources::master_data::UnitName(w1_name.clone()),
                    )
                {
                    for (def_name, dmg) in &weapon.damages {
                        if let Some(def_type) =
                            openwars_engine::resources::UnitType::from_str(def_name)
                        {
                            damage_chart.insert_damage(att_type, def_type, *dmg);
                        }
                    }
                }
                if let Some(w2_name) = &unit_record.weapon2
                    && let Some(weapon) = self.master_data.weapons.get(
                        &openwars_engine::resources::master_data::UnitName(w2_name.clone()),
                    )
                {
                    for (def_name, dmg) in &weapon.damages {
                        if let Some(def_type) =
                            openwars_engine::resources::UnitType::from_str(def_name)
                        {
                            damage_chart.insert_secondary_damage(att_type, def_type, *dmg);
                        }
                    }
                }
            }
        }
        world.insert_resource(damage_chart);

        let mut unit_registry_map = std::collections::HashMap::new();
        for (name, record) in &self.master_data.units {
            if let Some(u_type) = openwars_engine::resources::UnitType::from_str(&name.0) {
                let mut min_range = 0;
                let mut max_range = 0;

                let w1 = record.weapon1.as_ref().and_then(|w| {
                    self.master_data.weapons.get(
                        &openwars_engine::resources::master_data::UnitName(w.clone()),
                    )
                });
                let w2 = record.weapon2.as_ref().and_then(|w| {
                    self.master_data.weapons.get(
                        &openwars_engine::resources::master_data::UnitName(w.clone()),
                    )
                });

                if let Some(w) = w1 {
                    min_range = w.range_min;
                    max_range = w.range_max;
                } else if let Some(w) = w2 {
                    min_range = w.range_min;
                    max_range = w.range_max;
                }

                let can_capture = u_type == openwars_engine::resources::UnitType::Infantry
                    || u_type == openwars_engine::resources::UnitType::Mech;
                let can_supply = u_type == openwars_engine::resources::UnitType::SupplyTruck;

                let mut max_cargo = 0;
                let mut loadable = Vec::new();
                if let Some(loads) = self.master_data.loads.get(&name.0) {
                    for load_record in loads {
                        max_cargo = max_cargo.max(load_record.capacity);
                        if let Some(target_type) =
                            openwars_engine::resources::UnitType::from_str(&load_record.target)
                        {
                            loadable.push(target_type);
                        }
                    }
                }

                let daily_fuel = match u_type {
                    openwars_engine::resources::UnitType::Fighter
                    | openwars_engine::resources::UnitType::HeavyFighter
                    | openwars_engine::resources::UnitType::Bomber => 5,
                    openwars_engine::resources::UnitType::Bcopters
                    | openwars_engine::resources::UnitType::TransportHelicopter => 2,
                    openwars_engine::resources::UnitType::Battleship
                    | openwars_engine::resources::UnitType::Carrier
                    | openwars_engine::resources::UnitType::Lander => 1,
                    _ => 0,
                };

                let stats = openwars_engine::components::UnitStats {
                    unit_type: u_type,
                    cost: record.cost,
                    max_movement: record.movement,
                    movement_type: record.movement_type,
                    max_fuel: record.fuel,
                    max_ammo1: w1.map(|w| w.ammo).unwrap_or(0),
                    max_ammo2: w2.map(|w| w.ammo).unwrap_or(0),
                    min_range,
                    max_range,
                    daily_fuel_consumption: daily_fuel,
                    can_capture,
                    can_supply,
                    max_cargo,
                    loadable_unit_types: loadable,
                };
                unit_registry_map.insert(u_type, stats);
            }
        }
        let unit_registry = openwars_engine::resources::UnitRegistry(unit_registry_map);
        world.insert_resource(unit_registry);

        world.insert_resource(openwars_engine::resources::GameRng::default());

        if let Some(map_data) = self.master_data.get_map(&map_name) {
            let width = map_data.width;
            let height = map_data.height;
            let mut ecs_map = Map::new(width, height, Terrain::Plains, GridTopology::Square);

            let mut players = std::collections::HashSet::new();

            for y in 0..height {
                for x in 0..width {
                    if let Some(cell) = map_data.get_cell(x, y) {
                        let terrain = self.master_data.terrain_from_id(cell.terrain_id)?;
                        let _ = ecs_map.set_terrain(x, y, terrain);

                        if cell.player_id != 0 {
                            players.insert(cell.player_id);
                        }

                        // Spawn property entity if applicable
                        if terrain.max_capture_points() > 0 {
                            let owner = if cell.player_id == 0 {
                                None
                            } else {
                                Some(PlayerId(cell.player_id))
                            };
                            world.spawn((GridPosition { x, y }, Property::new(terrain, owner)));
                        }
                    }
                }
            }

            world.insert_resource(ecs_map);
            world.insert_resource(MatchState::default());

            let mut player_list = vec![];

            // Ensure at least Player 1 and Player 2 are in the game
            players.insert(1);
            players.insert(2);

            for &pid in &players {
                let mut income = 0;
                for y in 0..height {
                    for x in 0..width {
                        if let Some(cell) = map_data.get_cell(x, y)
                            && cell.player_id == pid
                        {
                            let landscape =
                                self.master_data.get_landscape(cell.terrain_id).ok_or_else(
                                    || anyhow::anyhow!("Unknown terrain ID: {:?}", cell.terrain_id),
                                )?;
                            // 収入判定ロジックをマスターデータ問い合わせに置換
                            income += self.master_data.landscape_income(&landscape.name);
                        }
                    }
                }
                // --- ターンごとの収入を初期資金に付与。この処理はエンジンイベントへと移行予定だが現状維持 ---
                let mut p = Player::new(pid, format!("Player {}", pid));
                p.funds = income; // Give turn 1 income
                player_list.push(p);
            }
            if player_list.is_empty() {
                player_list.push(Player::new(1, "Player 1".to_string()));
            }
            player_list.sort_by_key(|p| p.id.0); // Ensure consistent turn order
            world.insert_resource(Players(player_list));
        }

        // Add a master data resource so systems can access it if needed
        world.insert_resource(self.master_data.clone());

        self.world = Some(world);
        self.schedule = Some(schedule);
        Ok(())
    }
}
