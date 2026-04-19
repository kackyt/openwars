use bevy_ecs::prelude::*;
use engine::components::{GridPosition, PlayerId, Property};
use engine::resources::master_data::MasterDataRegistry;
use engine::resources::{
    GameOverCondition, GridTopology, Map, MatchState, Player, Players, Terrain,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CurrentScreen {
    MapSelection,
    InGame,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionType {
    Wait,
    Attack,
    Capture,
    Supply,
    Drop,
    Load,
    Merge,
    Cancel,
    EndTurn,
    Produce,
    Repair,
}

impl ActionType {
    pub fn label(&self) -> &'static str {
        match self {
            ActionType::Wait => "待機",
            ActionType::Attack => "攻撃",
            ActionType::Capture => "占領",
            ActionType::Supply => "補給",
            ActionType::Drop => "降車",
            ActionType::Load => "搭載",
            ActionType::Merge => "合流",
            ActionType::Cancel => "キャンセル",
            ActionType::EndTurn => "ターン終了",
            ActionType::Produce => "生産",
            ActionType::Repair => "修復",
        }
    }
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
        options: Vec<ActionType>,
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
        targets: Vec<Entity>,
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
    WaitActionMenu {
        unit_entity: Entity,
    },
    EventPopup {
        message: String,
    },
    GameOverPopup {
        message: String,
        condition: GameOverCondition,
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
        if self.log_messages.len() > 30 {
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
                    // ゲーム画面へ遷移
                    if let Err(e) = self.initialize_world(map_name.clone()) {
                        self.ui_state
                            .add_log(format!("マップ読み込みエラー: {}", e));
                    } else {
                        self.ui_state.current_screen = CurrentScreen::InGame;
                        self.ui_state.in_game_state = InGameState::Normal;
                        self.ui_state.cursor_pos = (0, 0);
                        self.ui_state
                            .add_log(format!("マップ '{}' を読み込みました。", map_name));
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
            KeyCode::Char('x') => self.handle_cancel_key(),
            _ => {}
        }
    }

    fn handle_cancel_key(&mut self) {
        match self.ui_state.in_game_state.clone() {
            InGameState::UnitSelected { .. } => {
                self.ui_state.in_game_state = InGameState::Normal;
            }
            InGameState::ActionMenu { unit_entity, .. } => {
                if let Some(_ue) = unit_entity {
                    // 移動の取り消し
                    if let Some(world) = &mut self.world {
                        world.send_event(engine::events::UndoMoveCommand);
                    }
                }
                self.ui_state.in_game_state = InGameState::Normal;
            }
            InGameState::ProductionMenu { .. } => {
                self.ui_state.in_game_state = InGameState::Normal;
            }
            InGameState::TargetSelection { unit_entity, .. } => {
                // アクション選択メニューに戻る
                self.reopen_unit_action_menu(unit_entity);
            }
            InGameState::CargoSelection { .. } => {
                self.ui_state.in_game_state = InGameState::Normal;
            }
            InGameState::DropTargetSelection {
                transport_entity, ..
            } => {
                // 乗降選択またはアクションメニューに戻るのが理想だが
                // 簡易化のためアクションメニューに戻す
                self.reopen_unit_action_menu(transport_entity);
            }
            InGameState::WaitActionMenu { unit_entity: _ } => {
                if let Some(world) = &mut self.world {
                    world.send_event(engine::events::UndoMoveCommand);
                }
                self.ui_state.in_game_state = InGameState::Normal;
            }
            InGameState::EventPopup { .. } => {
                self.ui_state.in_game_state = InGameState::Normal;
            }
            InGameState::GameOverPopup { .. } => {
                // Return to map selection is handled in handle_key
            }
            InGameState::Normal => {}
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
                InGameState::EventPopup { .. } | InGameState::WaitActionMenu { .. } => {}
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
                } => {
                    if *selected_index < options.len().saturating_sub(1) {
                        *selected_index += 1;
                    }
                }
                InGameState::ProductionMenu {
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
                InGameState::EventPopup { .. } | InGameState::WaitActionMenu { .. } => {}
                _ => {
                    if let Some(world) = &self.world
                        && let Some(map) = world.get_resource::<engine::resources::Map>()
                        && self.ui_state.cursor_pos.1 < map.height.saturating_sub(1)
                    {
                        self.ui_state.cursor_pos.1 += 1;
                    }
                }
            },
            KeyCode::Left | KeyCode::Char('h') => match &mut self.ui_state.in_game_state {
                InGameState::ActionMenu { .. }
                | InGameState::ProductionMenu { .. }
                | InGameState::CargoSelection { .. }
                | InGameState::WaitActionMenu { .. }
                | InGameState::EventPopup { .. } => {}
                _ => {
                    if self.ui_state.cursor_pos.0 > 0 {
                        self.ui_state.cursor_pos.0 -= 1;
                    }
                }
            },
            KeyCode::Right | KeyCode::Char('l') => match &mut self.ui_state.in_game_state {
                InGameState::ActionMenu { .. }
                | InGameState::ProductionMenu { .. }
                | InGameState::CargoSelection { .. }
                | InGameState::WaitActionMenu { .. }
                | InGameState::EventPopup { .. } => {}
                _ => {
                    if let Some(world) = &self.world
                        && let Some(map) = world.get_resource::<engine::resources::Map>()
                        && self.ui_state.cursor_pos.0 < map.width.saturating_sub(1)
                    {
                        self.ui_state.cursor_pos.0 += 1;
                    }
                }
            },
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
                let mut targets = vec![];
                if let Some(world) = &mut self.world {
                    targets = engine::systems::transport::get_droppable_tiles(
                        world,
                        transport_entity,
                        passenger,
                    );
                }

                if targets.is_empty() {
                    self.ui_state
                        .add_log("降ろせる場所がありません。".to_string());
                    self.reopen_unit_action_menu(transport_entity);
                } else {
                    // 最初の有効な降車先にカーソルを移動
                    self.ui_state.cursor_pos = targets[0];
                    self.ui_state.in_game_state = InGameState::DropTargetSelection {
                        transport_entity,
                        cargo_entity: passenger,
                        targets,
                        selected_index: 0,
                    };
                    self.ui_state
                        .add_log("降ろす場所を選択してください...".to_string());
                }
            }
            InGameState::DropTargetSelection {
                transport_entity,
                cargo_entity,
                ..
            } => self.handle_drop_target_confirm(transport_entity, cargo_entity),
            InGameState::WaitActionMenu { .. } => {}
            InGameState::EventPopup { .. } => {
                self.ui_state.in_game_state = InGameState::Normal;
            }
            InGameState::GameOverPopup { .. } => {
                // タイトル（マップ選択）へ戻る
                self.world = None;
                self.schedule = None;
                self.ui_state.current_screen = CurrentScreen::MapSelection;
                self.ui_state.in_game_state = InGameState::Normal;
                self.ui_state.cursor_pos = (0, 0);
            }
        }
    }
    fn handle_normal_confirm(&mut self) {
        let mut options = vec![ActionType::EndTurn, ActionType::Cancel];
        let mut selected_unit = None;

        if let Some(world) = &mut self.world {
            let cx = self.ui_state.cursor_pos.0;
            let cy = self.ui_state.cursor_pos.1;

            if let (Some(match_state), Some(players)) = (
                world.get_resource::<engine::resources::MatchState>(),
                world.get_resource::<engine::resources::Players>(),
            ) {
                let active_player_id = players.0[match_state.active_player_index.0].id;

                let mut u_query = world.query::<(
                    Entity,
                    &engine::components::GridPosition,
                    &engine::components::Faction,
                    &engine::components::ActionCompleted,
                    Option<&engine::components::HasMoved>,
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
                        .query::<(&engine::components::UnitStats, &engine::components::Fuel)>()
                        .get(world, entity)
                    {
                        u_stats = Some((st.movement_type, st.max_movement, st.unit_type));
                        fuel_cur = f.current;
                    }

                    let mut unit_positions = std::collections::HashMap::new();
                    let mut q_all = world.query::<(
                        Entity,
                        &engine::components::GridPosition,
                        &engine::components::Faction,
                        &engine::components::UnitStats,
                        Option<&engine::components::CargoCapacity>,
                        Option<&engine::components::Transporting>,
                    )>();
                    for (e, p, f, s, c, t) in q_all.iter(world) {
                        if e == entity || t.is_some() {
                            continue;
                        }
                        let free_slots = c
                            .map(|c| c.max.saturating_sub(c.loaded.len() as u32))
                            .unwrap_or(0);
                        unit_positions.insert(
                            (p.x, p.y),
                            engine::systems::movement::OccupantInfo {
                                player_id: f.0,
                                is_transport: s.max_cargo > 0,
                                unit_type: s.unit_type,
                                loadable_types: s.loadable_unit_types.clone(),
                                free_slots,
                            },
                        );
                    }

                    if let (Some(map), Some((m_type, max_mov, u_type))) =
                        (world.get_resource::<engine::resources::Map>(), u_stats)
                    {
                        reachable = engine::systems::movement::calculate_reachable_tiles(
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
                        .add_log(format!("ユニットを選択しました: {:?}", (cx, cy)));
                    return;
                }

                let mut is_factory = false;
                for (pos, prop) in world
                    .query::<(
                        &engine::components::GridPosition,
                        &engine::components::Property,
                    )>()
                    .iter(world)
                {
                    if pos.x == cx && pos.y == cy {
                        if self
                            .master_data
                            .is_production_facility(prop.terrain.as_str())
                        {
                            is_factory = true;
                        }
                        break;
                    }
                }

                if is_factory {
                    match engine::systems::production::can_produce_at_tile(
                        world,
                        active_player_id,
                        cx,
                        cy,
                        &self.master_data,
                    ) {
                        Ok(()) => {
                            options.insert(0, ActionType::Produce);
                        }
                        Err(e) => {
                            self.ui_state.add_log(e);
                        }
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
        options: Vec<ActionType>,
        selected_index: usize,
    ) {
        let selected = options[selected_index];
        match selected {
            ActionType::Cancel => {
                if let Some(_ue) = unit_entity {
                    // 移動の取り消し
                    if let Some(world) = &mut self.world {
                        world.send_event(engine::events::UndoMoveCommand);
                    }
                }
                self.ui_state.in_game_state = InGameState::Normal;
            }
            ActionType::EndTurn => {
                self.ui_state.in_game_state = InGameState::Normal;
                self.ui_state.add_log("ターンを終了しました。".to_string());

                if let Some(world) = &mut self.world {
                    world.send_event(engine::events::NextPhaseCommand);
                }
            }
            ActionType::Produce => {
                let mut options = Vec::new();
                if let Some(world) = &mut self.world {
                    let mut player_funds = 0;

                    if let (Some(match_state), Some(players)) = (
                        world.get_resource::<engine::resources::MatchState>(),
                        world.get_resource::<engine::resources::Players>(),
                    ) {
                        player_funds = players.0[match_state.active_player_index.0].funds;
                    }

                    let mut landscape_name = None;
                    let mut p_query = world.query::<(
                        &engine::components::GridPosition,
                        &engine::components::Property,
                    )>();
                    for (pos, prop) in p_query.iter(world) {
                        if pos.x == self.ui_state.cursor_pos.0
                            && pos.y == self.ui_state.cursor_pos.1
                        {
                            landscape_name = Some(prop.terrain.as_str());
                        }
                    }

                    let Some(landscape_name) = landscape_name else {
                        self.ui_state
                            .add_log("生産施設の地形取得に失敗しました。".to_string());
                        self.ui_state.in_game_state = InGameState::Normal;
                        return;
                    };

                    let mut sorted_names: Vec<_> = self.master_data.units.keys().cloned().collect();
                    sorted_names.sort_by(|a, b| a.0.cmp(&b.0));
                    for name in sorted_names {
                        if let Some(record) = self.master_data.units.get(&name) {
                            if player_funds < record.cost {
                                continue;
                            }

                            if let Ok(u_type) = self.master_data.unit_type_for_name(&name.0)
                                && self.master_data.can_produce_unit(landscape_name, u_type)
                            {
                                options.push(name.0.clone());
                            }
                        }
                    }
                }
                options.push("キャンセル".to_string());

                self.ui_state.in_game_state = InGameState::ProductionMenu {
                    factory_pos: self.ui_state.cursor_pos,
                    options,
                    selected_index: 0,
                };
            }
            _ => {
                if let Some(entity) = unit_entity {
                    let is_moved = if let Some(world) = &mut self.world {
                        let mut moved = false;
                        if let Some(pm) = world.get_resource::<engine::resources::PendingMove>()
                            && pm.unit_entity == entity
                            && let Some(pos) = world.get::<engine::components::GridPosition>(entity)
                        {
                            moved = pos.x != pm.original_pos.x || pos.y != pm.original_pos.y;
                        }
                        moved
                    } else {
                        false
                    };

                    match selected {
                        ActionType::Wait => {
                            if let Some(world) = &mut self.world {
                                world.send_event(engine::events::WaitUnitCommand {
                                    unit_entity: entity,
                                });
                            }
                            self.ui_state.in_game_state = InGameState::Normal;
                            self.ui_state.add_log("待機しました。".to_string());
                        }
                        ActionType::Capture | ActionType::Repair => {
                            if let Some(world) = &mut self.world {
                                world.send_event(engine::events::CapturePropertyCommand {
                                    unit_entity: entity,
                                });
                            }
                            self.ui_state.in_game_state = InGameState::Normal;
                            if selected == ActionType::Capture {
                                self.ui_state.add_log("占領を開始しました。".to_string());
                            } else {
                                self.ui_state.add_log("拠点を修復しました。".to_string());
                            }
                        }
                        ActionType::Attack => {
                            let targets = if let Some(world) = &mut self.world {
                                engine::systems::combat::get_attackable_targets(
                                    world, entity, !is_moved,
                                )
                            } else {
                                vec![]
                            };
                            self.ui_state.in_game_state = InGameState::TargetSelection {
                                unit_entity: entity,
                                action: "攻撃".to_string(),
                                targets,
                                selected_index: 0,
                            };
                            self.ui_state
                                .add_log("攻撃対象を選択してください...".to_string());
                        }
                        ActionType::Drop => {
                            let mut passengers = vec![];
                            if let Some(world) = &mut self.world
                                && let Ok(cargo) = world
                                    .query::<&engine::components::CargoCapacity>()
                                    .get(world, entity)
                            {
                                for &p_ent in &cargo.loaded {
                                    if let Some(act) =
                                        world.get::<engine::components::ActionCompleted>(p_ent)
                                        && !act.0
                                    {
                                        passengers.push(p_ent);
                                    }
                                }
                            }
                            if passengers.is_empty() {
                                self.ui_state
                                    .add_log("降車可能な未行動ユニットがいません。".to_string());
                            } else {
                                self.ui_state.in_game_state = InGameState::CargoSelection {
                                    transport_entity: entity,
                                    passengers,
                                    selected_index: 0,
                                };
                            }
                        }
                        ActionType::Supply => {
                            let targets = if let Some(world) = &mut self.world {
                                engine::systems::supply::get_suppliable_targets(world, entity)
                            } else {
                                vec![]
                            };
                            self.ui_state.in_game_state = InGameState::TargetSelection {
                                unit_entity: entity,
                                action: "補給".to_string(),
                                targets,
                                selected_index: 0,
                            };
                            self.ui_state
                                .add_log("補給対象を選択してください...".to_string());
                        }
                        ActionType::Merge => {
                            let targets = if let Some(world) = &mut self.world {
                                engine::systems::merge::get_mergable_targets(world, entity)
                            } else {
                                vec![]
                            };
                            if targets.len() == 1 {
                                if let Some(world) = &mut self.world {
                                    world.send_event(engine::events::MergeUnitCommand {
                                        source_entity: entity,
                                        target_entity: targets[0],
                                    });
                                    self.ui_state.add_log("合流しています...".to_string());
                                    self.ui_state.in_game_state = InGameState::Normal;
                                }
                            } else {
                                self.ui_state.in_game_state = InGameState::TargetSelection {
                                    unit_entity: entity,
                                    action: "合流".to_string(),
                                    targets,
                                    selected_index: 0,
                                };
                                self.ui_state
                                    .add_log("合流対象を選択してください...".to_string());
                            }
                        }
                        ActionType::Load => {
                            let targets = if let Some(world) = &mut self.world {
                                engine::systems::transport::get_loadable_transports(world, entity)
                            } else {
                                vec![]
                            };
                            if targets.len() == 1 {
                                if let Some(world) = &mut self.world {
                                    world.send_event(engine::events::LoadUnitCommand {
                                        transport_entity: targets[0],
                                        unit_entity: entity,
                                    });
                                    self.ui_state
                                        .add_log("輸送ユニットに搭載しています...".to_string());
                                    self.ui_state.in_game_state = InGameState::Normal;
                                }
                            } else {
                                self.ui_state.in_game_state = InGameState::TargetSelection {
                                    unit_entity: entity,
                                    action: "搭載".to_string(),
                                    targets,
                                    selected_index: 0,
                                };
                                self.ui_state
                                    .add_log("搭載先のユニットを選択してください...".to_string());
                            }
                        }
                        _ => {}
                    }
                }
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
        if selected == "キャンセル" {
            self.ui_state.in_game_state = InGameState::Normal;
        } else {
            if let Some(world) = &mut self.world
                && let (Some(match_state), Some(players)) = (
                    world.get_resource::<engine::resources::MatchState>(),
                    world.get_resource::<engine::resources::Players>(),
                )
            {
                let active_player_id = players.0[match_state.active_player_index.0].id;
                let Ok(unit_type) = self.master_data.unit_type_for_name(selected) else {
                    self.ui_state
                        .add_log(format!("未対応のユニット種別です: {}", selected));
                    self.ui_state.in_game_state = InGameState::Normal;
                    return;
                };
                world.send_event(engine::events::ProduceUnitCommand {
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
        let (cx, cy) = self.ui_state.cursor_pos;
        let mut target_unit = None;

        if let InGameState::TargetSelection { targets, .. } = &self.ui_state.in_game_state
            && let Some(world) = &self.world
        {
            for &target in targets {
                if let Some(pos) = world.get::<engine::components::GridPosition>(target)
                    && pos.x == cx
                    && pos.y == cy
                {
                    target_unit = Some(target);
                    break;
                }
            }
        }

        if let Some(world) = &mut self.world {
            if action == "攻撃" {
                if let Some(target) = target_unit {
                    match engine::systems::combat::can_attack(unit_entity, target, world) {
                        Ok(()) => {
                            world.send_event(engine::events::AttackUnitCommand {
                                attacker_entity: unit_entity,
                                defender_entity: target,
                            });
                            self.ui_state.add_log(format!("攻撃中: {:?}", (cx, cy)));
                            self.ui_state.in_game_state = InGameState::Normal;
                        }
                        Err(e) => {
                            self.ui_state.add_log(format!("攻撃中止: {}", e));
                            self.reopen_unit_action_menu(unit_entity);
                        }
                    }
                } else {
                    self.ui_state
                        .add_log("対象がいません。キャンセルされました。".to_string());
                    self.reopen_unit_action_menu(unit_entity);
                }
            } else if action == "補給" {
                if let Some(target) = target_unit {
                    world.send_event(engine::events::SupplyUnitCommand {
                        supplier_entity: unit_entity,
                        target_entity: target,
                    });
                    self.ui_state.add_log(format!("補給中: {:?}", (cx, cy)));
                    self.ui_state.in_game_state = InGameState::Normal;
                } else {
                    self.ui_state
                        .add_log("補給対象がいません。キャンセルされました。".to_string());
                    self.reopen_unit_action_menu(unit_entity);
                }
            } else if action == "合流" {
                if let Some(target) = target_unit {
                    world.send_event(engine::events::MergeUnitCommand {
                        source_entity: unit_entity,
                        target_entity: target,
                    });
                    self.ui_state.add_log(format!("合流中: {:?}", (cx, cy)));
                    self.ui_state.in_game_state = InGameState::Normal;
                } else {
                    self.ui_state
                        .add_log("合流対象がいません。キャンセルされました。".to_string());
                    self.reopen_unit_action_menu(unit_entity);
                }
            } else if action == "搭載" {
                if let Some(target) = target_unit {
                    world.send_event(engine::events::LoadUnitCommand {
                        transport_entity: target,
                        unit_entity,
                    });
                    self.ui_state.add_log(format!("搭載中: {:?}", (cx, cy)));
                    self.ui_state.in_game_state = InGameState::Normal;
                } else {
                    self.ui_state
                        .add_log("搭載先がいません。キャンセルされました。".to_string());
                    self.reopen_unit_action_menu(unit_entity);
                }
            }
        }
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
            self.ui_state.add_log("移動範囲外です。".to_string());
            self.ui_state.in_game_state = InGameState::Normal;
        } else {
            if let Some(world) = &mut self.world {
                world.send_event(engine::events::MoveUnitCommand {
                    unit_entity,
                    target_x: cx,
                    target_y: cy,
                });

                self.ui_state.in_game_state = InGameState::WaitActionMenu { unit_entity };
            }
            self.ui_state
                .add_log(format!("ユニットを移動しました: {:?}", (cx, cy)));
        }
    }
    pub fn reopen_unit_action_menu(&mut self, unit_entity: Entity) {
        let world = match &mut self.world {
            Some(w) => w,
            None => return,
        };

        let mut is_moved = false;
        if let Some(pm) = world.get_resource::<engine::resources::PendingMove>()
            && pm.unit_entity == unit_entity
            && let Some(pos) = world.get::<engine::components::GridPosition>(unit_entity)
        {
            is_moved = pos.x != pm.original_pos.x || pos.y != pm.original_pos.y;
        }

        let actions = engine::systems::action::get_available_actions(world, unit_entity, is_moved);
        let mut options = Vec::new();

        if actions.can_wait {
            options.push(ActionType::Wait);
        }

        if actions.can_attack {
            options.insert(0, ActionType::Attack);
        }

        if actions.can_capture {
            options.push(ActionType::Capture);
        }

        if actions.can_repair {
            options.push(ActionType::Repair);
        }

        if actions.can_supply {
            options.push(ActionType::Supply);
        }
        if actions.can_drop {
            options.push(ActionType::Drop);
        }
        if actions.can_load {
            options.push(ActionType::Load);
        }
        if actions.can_merge {
            options.push(ActionType::Merge);
        }

        if is_moved {
            options.push(ActionType::Cancel);
        }

        self.ui_state.in_game_state = InGameState::ActionMenu {
            unit_entity: Some(unit_entity),
            options,
            selected_index: 0,
        };
    }
    fn handle_drop_target_confirm(&mut self, transport_entity: Entity, cargo_entity: Entity) {
        let cx = self.ui_state.cursor_pos.0;
        let cy = self.ui_state.cursor_pos.1;

        if let InGameState::DropTargetSelection { targets, .. } = &self.ui_state.in_game_state
            && !targets.contains(&(cx, cy))
        {
            self.ui_state
                .add_log("降車位置が不正です。キャンセルされました。".to_string());
            self.reopen_unit_action_menu(transport_entity);
            return;
        }

        if let Some(world) = &mut self.world {
            world.send_event(engine::events::UnloadUnitCommand {
                transport_entity,
                cargo_entity,
                target_x: cx,
                target_y: cy,
            });
        }
        self.ui_state
            .add_log(format!("ユニットを降ろしました: {:?}", (cx, cy)));
        self.ui_state.in_game_state = InGameState::WaitActionMenu {
            unit_entity: transport_entity,
        };
    }

    pub fn handle_key(&mut self, key: crossterm::event::KeyEvent) {
        match self.ui_state.current_screen {
            CurrentScreen::MapSelection => self.handle_map_selection_key(key),
            CurrentScreen::InGame => self.handle_in_game_key(key),
        }
    }

    fn initialize_world(&mut self, map_name: String) -> anyhow::Result<()> {
        use engine::events::*;
        use engine::systems::*;

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
        world.init_resource::<Events<UndoMoveCommand>>();

        // Add event clearing systems
        // Intentionally skipping manual event clearance (update_system) to avoid Bevy version disparities.
        // EventReader correctly tracks indices, so old events won't be reprocessed.

        // Add game logic systems (order is managed by engine)
        add_main_game_systems(&mut schedule);

        // Build UnitRegistry and DamageChart from MasterDataRegistry
        let mut damage_chart = engine::resources::DamageChart::new();
        for (unit_name, unit_record) in &self.master_data.units {
            let att_type = self.master_data.unit_type_for_name(&unit_name.0)?;

            if let Some(w1_name) = &unit_record.weapon1 {
                let weapon = self
                    .master_data
                    .weapons
                    .get(&engine::resources::master_data::UnitName(w1_name.clone()))
                    .ok_or_else(|| {
                        anyhow::anyhow!("Weapon '{}' not found for unit '{}'", w1_name, unit_name.0)
                    })?;

                for (def_name, dmg) in &weapon.damages {
                    let def_type = self.master_data.unit_type_for_name(def_name)?;
                    damage_chart.insert_damage(att_type, def_type, *dmg);
                }
            }

            if let Some(w2_name) = &unit_record.weapon2 {
                let weapon = self
                    .master_data
                    .weapons
                    .get(&engine::resources::master_data::UnitName(w2_name.clone()))
                    .ok_or_else(|| {
                        anyhow::anyhow!("Weapon '{}' not found for unit '{}'", w2_name, unit_name.0)
                    })?;

                for (def_name, dmg) in &weapon.damages {
                    let def_type = self.master_data.unit_type_for_name(def_name)?;
                    damage_chart.insert_secondary_damage(att_type, def_type, *dmg);
                }
            }
        }
        world.insert_resource(damage_chart);

        let mut unit_registry_map = std::collections::HashMap::new();
        for name in self.master_data.units.keys() {
            let stats = self.master_data.create_unit_stats(name)?;
            unit_registry_map.insert(stats.unit_type, stats);
        }
        let unit_registry = engine::resources::UnitRegistry(unit_registry_map);
        world.insert_resource(unit_registry);

        world.insert_resource(engine::resources::GameRng::default());

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
                        let landscape_name = terrain.as_str();
                        let durability = self.master_data.landscape_durability(landscape_name);
                        if durability > 0 {
                            let owner = if cell.player_id == 0 {
                                None
                            } else {
                                Some(PlayerId(cell.player_id))
                            };
                            world.spawn((
                                GridPosition { x, y },
                                Property::new(terrain, owner, durability),
                            ));
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
