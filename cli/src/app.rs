use bevy_ecs::prelude::*;
use engine::ai::{AiVersion, PlayerAiSettings, resolve_player_ai_version};
use engine::components::{
    ActionCompleted, CargoCapacity, Faction, Fuel, GridPosition, HasMoved, Property, Transporting,
    UnitStats,
};
use engine::resources::master_data::MasterDataRegistry;
use engine::resources::{GameOverCondition, GridTopology, Map, MatchState, PendingMove, Players};
use engine::setup::initialize_world_from_master_data_with_topology;

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
    SaveGame,
    LoadGame,
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
            ActionType::SaveGame => "セーブ",
            ActionType::LoadGame => "ロード",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InGameState {
    Normal,
    WaitAiAction,
    UnitSelected {
        unit_entity: Entity,
        start_pos: (usize, usize),
        reachable_tiles: std::collections::BTreeSet<(usize, usize)>,
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
    SaveSelection {
        selected_index: usize,
        options: Vec<String>,
        files: Vec<Option<String>>,
    },
    LoadSelection {
        selected_index: usize,
        options: Vec<String>,
        files: Vec<Option<String>>,
        is_title_screen: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayerControlType {
    Human,
    Ai,
}

/// CLIで選択可能なAIバージョン。V2は互換用としてエンジン内に残し、画面には公開しません。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CliAiVersion {
    V1,
    V3,
    V4,
    V100,
    V200,
}

impl CliAiVersion {
    pub fn label(self) -> &'static str {
        match self {
            Self::V1 => "V1",
            Self::V3 => "V3",
            Self::V4 => "V4",
            Self::V100 => "V100",
            Self::V200 => "V200",
        }
    }

    fn to_engine(self) -> AiVersion {
        match self {
            Self::V1 => AiVersion::V1,
            Self::V3 => AiVersion::V3,
            Self::V4 => AiVersion::V4,
            Self::V100 => AiVersion::V100,
            Self::V200 => AiVersion::V200,
        }
    }

    /// V2セーブをCLIで開いた場合は、選択肢に存在する最新系列のV3へ正規化します。
    fn from_engine(version: AiVersion) -> Self {
        match version {
            AiVersion::V1 => Self::V1,
            AiVersion::V2 | AiVersion::V3 => Self::V3,
            AiVersion::V4 => Self::V4,
            AiVersion::V100 => Self::V100,
            AiVersion::V200 => Self::V200,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RightPanelMode {
    Info,
    Stats,
}

pub struct UiState {
    pub current_screen: CurrentScreen,
    pub in_game_state: InGameState,
    pub selected_map_index: usize,
    pub available_maps: Vec<String>,
    /// マップ選択画面で選ぶグリッド形状（スクエア/ヘックス）
    pub selected_topology: GridTopology,
    // In-game state
    pub player_controls: std::collections::HashMap<u32, PlayerControlType>,
    /// Humanへ切り替えた後も、次に使用するAIバージョンを保持します。
    pub player_ai_versions: std::collections::HashMap<u32, CliAiVersion>,
    pub cursor_pos: (usize, usize),
    pub log_messages: Vec<String>,
    pub right_panel_mode: RightPanelMode,
}

impl UiState {
    pub fn new(maps: Vec<String>) -> Self {
        let mut controls = std::collections::HashMap::new();
        controls.insert(1, PlayerControlType::Human);
        controls.insert(2, PlayerControlType::Ai);

        let mut ai_versions = std::collections::HashMap::new();
        ai_versions.insert(1, CliAiVersion::V3);
        ai_versions.insert(2, CliAiVersion::V3);

        Self {
            current_screen: CurrentScreen::MapSelection,
            in_game_state: InGameState::Normal,
            selected_map_index: 0,
            available_maps: maps,
            selected_topology: GridTopology::Square,
            player_controls: controls,
            player_ai_versions: ai_versions,
            cursor_pos: (0, 0),
            log_messages: Vec::new(),
            right_panel_mode: RightPanelMode::Info,
        }
    }

    pub fn toggle_right_panel_mode(&mut self) {
        self.right_panel_mode = match self.right_panel_mode {
            RightPanelMode::Info => RightPanelMode::Stats,
            RightPanelMode::Stats => RightPanelMode::Info,
        };
    }

    pub fn add_log(&mut self, msg: String) {
        self.log_messages.push(msg);
        if self.log_messages.len() > 30 {
            self.log_messages.remove(0);
        }
    }

    /// 指定されたプレイヤーIDが人間かどうかを判定します。
    /// 未登録のプレイヤーはデフォルトで人間とみなします。
    pub fn is_human(&self, player_id: u32) -> bool {
        !matches!(
            self.player_controls.get(&player_id),
            Some(PlayerControlType::Ai)
        )
    }

    /// 登録されているプレイヤーの中に人間がいるかどうかを判定します。
    pub fn has_human_player(&self) -> bool {
        self.player_controls
            .values()
            .any(|v| *v == PlayerControlType::Human)
    }

    pub fn ai_version(&self, player_id: u32) -> CliAiVersion {
        self.player_ai_versions
            .get(&player_id)
            .copied()
            .unwrap_or(CliAiVersion::V3)
    }

    pub fn control_label(&self, player_id: u32) -> String {
        if self.is_human(player_id) {
            "Human".to_string()
        } else {
            format!("AI({})", self.ai_version(player_id).label())
        }
    }

    /// マップ選択画面ではHuman、V1、V3、V4、V100、V200の順で切り替えます。
    fn cycle_player_setup(&mut self, player_id: u32) {
        let control = self
            .player_controls
            .get(&player_id)
            .copied()
            .unwrap_or(PlayerControlType::Human);
        let version = self.ai_version(player_id);

        match (control, version) {
            (PlayerControlType::Human, _) => {
                self.player_controls
                    .insert(player_id, PlayerControlType::Ai);
                self.player_ai_versions.insert(player_id, CliAiVersion::V1);
            }
            (PlayerControlType::Ai, CliAiVersion::V1) => {
                self.player_ai_versions.insert(player_id, CliAiVersion::V3);
            }
            (PlayerControlType::Ai, CliAiVersion::V3) => {
                self.player_ai_versions.insert(player_id, CliAiVersion::V4);
            }
            (PlayerControlType::Ai, CliAiVersion::V4) => {
                self.player_ai_versions
                    .insert(player_id, CliAiVersion::V100);
            }
            (PlayerControlType::Ai, CliAiVersion::V100) => {
                self.player_ai_versions
                    .insert(player_id, CliAiVersion::V200);
            }
            (PlayerControlType::Ai, CliAiVersion::V200) => {
                self.player_controls
                    .insert(player_id, PlayerControlType::Human);
            }
        }
    }

    /// インゲームの切り替えでは、選択済みAIバージョンを変更しません。
    fn toggle_player_control(&mut self, player_id: u32) -> PlayerControlType {
        let next = if self.is_human(player_id) {
            PlayerControlType::Ai
        } else {
            PlayerControlType::Human
        };
        self.player_controls.insert(player_id, next);
        self.player_ai_versions
            .entry(player_id)
            .or_insert(CliAiVersion::V3);
        next
    }

    /// CLIで記憶している全プレイヤーのバージョンをECSリソースへ反映します。
    fn apply_ai_versions_to_world(&self, world: &mut World) {
        let player_ids = world
            .get_resource::<Players>()
            .map(|players| {
                players
                    .0
                    .iter()
                    .map(|player| player.id.0)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        if !world.contains_resource::<PlayerAiSettings>() {
            world.insert_resource(PlayerAiSettings::default());
        }
        let mut settings = world.resource_mut::<PlayerAiSettings>();
        for player_id in player_ids {
            settings.set_version(
                engine::components::PlayerId(player_id),
                self.ai_version(player_id).to_engine(),
            );
        }
    }

    /// セーブから復元した実効バージョンをCLI状態へ取り込み、V2だけはV3へ正規化します。
    fn adopt_ai_versions_from_world(&mut self, world: &mut World) {
        let versions = world
            .get_resource::<Players>()
            .map(|players| {
                players
                    .0
                    .iter()
                    .map(|player| {
                        (
                            player.id.0,
                            CliAiVersion::from_engine(resolve_player_ai_version(world, player.id)),
                        )
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        for (player_id, version) in versions {
            self.player_ai_versions.insert(player_id, version);
        }
        self.apply_ai_versions_to_world(world);
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

        if let InGameState::LoadSelection {
            selected_index,
            options,
            files,
            is_title_screen,
        } = &self.ui_state.in_game_state
        {
            self.handle_load_selection_key(
                key,
                *selected_index,
                options.clone(),
                files.clone(),
                *is_title_screen,
            );
            return;
        }

        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => self.should_quit = true,
            KeyCode::Up | KeyCode::Char('k') if self.ui_state.selected_map_index > 0 => {
                self.ui_state.selected_map_index -= 1;
            }
            KeyCode::Up | KeyCode::Char('k') => {}
            KeyCode::Down | KeyCode::Char('j')
                if self.ui_state.selected_map_index
                    < self.ui_state.available_maps.len().saturating_sub(1) =>
            {
                self.ui_state.selected_map_index += 1;
            }
            KeyCode::Down | KeyCode::Char('j') => {}
            KeyCode::Char('1') => self.ui_state.cycle_player_setup(1),
            KeyCode::Char('2') => self.ui_state.cycle_player_setup(2),
            // グリッド形状（スクエア/ヘックス）の切り替え
            KeyCode::Char('t') => {
                self.ui_state.selected_topology = match self.ui_state.selected_topology {
                    GridTopology::Square => GridTopology::Hex,
                    GridTopology::Hex => GridTopology::Square,
                };
            }
            // セーブデータのロード画面を表示する
            KeyCode::Char('l') | KeyCode::Char('L') => {
                let (options, files) = self.get_slot_status();
                self.ui_state.in_game_state = InGameState::LoadSelection {
                    selected_index: 0,
                    options,
                    files,
                    is_title_screen: true,
                };
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
                        let grid_label = match self.ui_state.selected_topology {
                            GridTopology::Square => "スクエア",
                            GridTopology::Hex => "ヘックス",
                        };
                        self.ui_state.add_log(format!(
                            "マップ '{}' を読み込みました。(グリッド: {})",
                            map_name, grid_label
                        ));
                    }
                }
            }
            _ => {}
        }
    }

    pub fn get_slot_status(&self) -> (Vec<String>, Vec<Option<String>>) {
        let mut options = Vec::new();
        let mut files = Vec::new();
        let saves_dir = std::path::Path::new("saves");
        if !saves_dir.exists() {
            let _ = std::fs::create_dir_all(saves_dir);
        }

        for i in 1..=5 {
            let file_name = format!("slot_{}.sav", i);
            let file_path = saves_dir.join(&file_name);
            let path_str = file_path.to_string_lossy().into_owned();

            if file_path.exists() {
                if let Ok(content) = std::fs::read_to_string(&file_path) {
                    match engine::serialize::read_save_header(&content) {
                        Ok(header) => {
                            options.push(format!(
                                "スロット {} : {} (Turn {}, {})",
                                i, header.map_name, header.turn_number, header.active_player_name
                            ));
                        }
                        Err(_) => {
                            options.push(format!("スロット {} : [破損データ]", i));
                        }
                    }
                } else {
                    options.push(format!("スロット {} : [読込失敗]", i));
                }
                files.push(Some(path_str));
            } else {
                options.push(format!("スロット {} : [空スロット]", i));
                files.push(None);
            }
        }
        (options, files)
    }

    pub fn save_game_to_file(&mut self, path: &str, map_name: &str) -> anyhow::Result<()> {
        let world = self
            .world
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("World not initialized"))?;
        let save_str = engine::serialize::export_save_data(world, map_name)?;

        let path_obj = std::path::Path::new(path);
        if let Some(parent) = path_obj.parent()
            && !parent.exists()
        {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, save_str)?;
        Ok(())
    }

    pub fn load_game_from_file(&mut self, path: &str) -> anyhow::Result<()> {
        let save_str = std::fs::read_to_string(path)?;
        let (mut world, schedule) =
            engine::serialize::import_save_data(&save_str, &self.master_data)?;

        // 読み込んだマップのトポロジーとAIバージョンをAppのUI状態に同期する
        if let Some(map) = world.get_resource::<engine::resources::Map>() {
            self.ui_state.selected_topology = map.topology;
        }
        self.ui_state.adopt_ai_versions_from_world(&mut world);

        self.world = Some(world);
        self.schedule = Some(schedule);
        Ok(())
    }

    fn handle_save_selection_key(
        &mut self,
        key: crossterm::event::KeyEvent,
        selected_index: usize,
        options: Vec<String>,
        files: Vec<Option<String>>,
    ) {
        use crossterm::event::KeyCode;
        match key.code {
            KeyCode::Up | KeyCode::Char('k') if selected_index > 0 => {
                self.ui_state.in_game_state = InGameState::SaveSelection {
                    selected_index: selected_index - 1,
                    options,
                    files,
                };
            }
            KeyCode::Down | KeyCode::Char('j')
                if selected_index < options.len().saturating_sub(1) =>
            {
                self.ui_state.in_game_state = InGameState::SaveSelection {
                    selected_index: selected_index + 1,
                    options,
                    files,
                };
            }
            KeyCode::Char(' ') | KeyCode::Enter => {
                let slot = selected_index + 1;
                let path = format!("saves/slot_{}.sav", slot);
                let map_name = self
                    .ui_state
                    .available_maps
                    .get(self.ui_state.selected_map_index)
                    .cloned()
                    .unwrap_or_else(|| "unknown_map".to_string());

                match self.save_game_to_file(&path, &map_name) {
                    Ok(()) => {
                        self.ui_state
                            .add_log(format!("スロット {} にセーブしました。", slot));
                    }
                    Err(e) => {
                        self.ui_state.add_log(format!("セーブ失敗: {}", e));
                    }
                }
                self.ui_state.in_game_state = InGameState::Normal;
            }
            KeyCode::Esc | KeyCode::Char('x') => {
                self.ui_state.in_game_state = InGameState::Normal;
            }
            _ => {}
        }
    }

    fn handle_load_selection_key(
        &mut self,
        key: crossterm::event::KeyEvent,
        selected_index: usize,
        options: Vec<String>,
        files: Vec<Option<String>>,
        is_title_screen: bool,
    ) {
        use crossterm::event::KeyCode;
        match key.code {
            KeyCode::Up | KeyCode::Char('k') if selected_index > 0 => {
                self.ui_state.in_game_state = InGameState::LoadSelection {
                    selected_index: selected_index - 1,
                    options,
                    files,
                    is_title_screen,
                };
            }
            KeyCode::Down | KeyCode::Char('j')
                if selected_index < options.len().saturating_sub(1) =>
            {
                self.ui_state.in_game_state = InGameState::LoadSelection {
                    selected_index: selected_index + 1,
                    options,
                    files,
                    is_title_screen,
                };
            }
            KeyCode::Char(' ') | KeyCode::Enter => {
                if let Some(Some(path)) = files.get(selected_index) {
                    match self.load_game_from_file(path) {
                        Ok(()) => {
                            self.ui_state.add_log(format!(
                                "スロット {} からロードしました。",
                                selected_index + 1
                            ));
                            self.ui_state.current_screen = CurrentScreen::InGame;
                        }
                        Err(e) => {
                            self.ui_state.add_log(format!("ロード失敗: {}", e));
                        }
                    }
                } else {
                    self.ui_state
                        .add_log("選択されたスロットは空です。".to_string());
                }
                self.ui_state.in_game_state = InGameState::Normal;
            }
            KeyCode::Esc | KeyCode::Char('x') => {
                self.ui_state.in_game_state = InGameState::Normal;
                if is_title_screen {
                    self.ui_state.current_screen = CurrentScreen::MapSelection;
                }
            }
            _ => {}
        }
    }

    pub fn handle_in_game_key(&mut self, key: crossterm::event::KeyEvent) {
        // セーブ/ロードメニューが開いている場合はそちらのキー処理に流す
        match &self.ui_state.in_game_state {
            InGameState::SaveSelection {
                selected_index,
                options,
                files,
            } => {
                let selected_index = *selected_index;
                let options = options.clone();
                let files = files.clone();
                self.handle_save_selection_key(key, selected_index, options, files);
                return;
            }
            InGameState::LoadSelection {
                selected_index,
                options,
                files,
                is_title_screen,
            } => {
                let selected_index = *selected_index;
                let options = options.clone();
                let files = files.clone();
                let is_title_screen = *is_title_screen;
                self.handle_load_selection_key(
                    key,
                    selected_index,
                    options,
                    files,
                    is_title_screen,
                );
                return;
            }
            _ => {}
        }

        // AIターンの場合は一部のキー（終了、スタッツ表示切替）以外は無視する
        if let Some(world) = &self.world
            && let Some(match_state) = world.get_resource::<MatchState>()
            && let Some(players) = world.get_resource::<Players>()
            && let Some(active_player) = players.0.get(match_state.active_player_index.0)
            && (matches!(self.ui_state.in_game_state, InGameState::Normal)
                || matches!(self.ui_state.in_game_state, InGameState::WaitAiAction))
            && !self.ui_state.is_human(active_player.id.0)
        {
            match key.code {
                crossterm::event::KeyCode::Char('q') => self.should_quit = true,
                crossterm::event::KeyCode::Tab | crossterm::event::KeyCode::Char('s') => {
                    self.ui_state.toggle_right_panel_mode();
                }
                _ => return, // AIターン中は移動等のゲーム操作キー入力を無視
            }
        }

        use crossterm::event::KeyCode;

        match key.code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Tab | KeyCode::Char('s') => self.ui_state.toggle_right_panel_mode(),
            KeyCode::Esc => self.return_to_map_selection(),
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
            InGameState::GameOverPopup { .. } => self.return_to_map_selection(),
            InGameState::SaveSelection { .. } | InGameState::LoadSelection { .. } => {
                self.ui_state.in_game_state = InGameState::Normal;
            }
            InGameState::Normal | InGameState::WaitAiAction => {}
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
                        && let Some(map) = world.get_resource::<Map>()
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
                        && let Some(map) = world.get_resource::<Map>()
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
            InGameState::WaitAiAction => {}
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
            InGameState::GameOverPopup { .. } => self.return_to_map_selection(),
            InGameState::SaveSelection { .. } | InGameState::LoadSelection { .. } => {}
        }
    }
    fn handle_normal_confirm(&mut self) {
        let mut options = vec![
            ActionType::EndTurn,
            ActionType::SaveGame,
            ActionType::LoadGame,
            ActionType::Cancel,
        ];
        let mut selected_unit = None;

        if let Some(world) = &mut self.world {
            let cx = self.ui_state.cursor_pos.0;
            let cy = self.ui_state.cursor_pos.1;

            if let (Some(match_state), Some(players)) = (
                world.get_resource::<MatchState>(),
                world.get_resource::<Players>(),
            ) {
                let active_player_id = players.0[match_state.active_player_index.0].id;

                let mut u_query = world.query::<(
                    Entity,
                    &GridPosition,
                    &Faction,
                    &ActionCompleted,
                    Option<&HasMoved>,
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
                    let mut reachable = std::collections::BTreeSet::new();
                    let mut u_stats = None;
                    let mut fuel_cur = 0;

                    if let Ok((st, f)) = world.query::<(&UnitStats, &Fuel)>().get(world, entity) {
                        u_stats = Some((st.movement_type, st.max_movement, st.unit_type));
                        fuel_cur = f.current;
                    }

                    let mut unit_positions = std::collections::HashMap::new();
                    let mut q_all = world.query::<(
                        Entity,
                        &GridPosition,
                        &Faction,
                        &UnitStats,
                        Option<&CargoCapacity>,
                        Option<&Transporting>,
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
                        (world.get_resource::<Map>(), u_stats)
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
                for (pos, prop) in world.query::<(&GridPosition, &Property)>().iter(world) {
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
                if let Some(ue) = unit_entity {
                    // 移動の取り消し
                    if let Some(world) = &mut self.world {
                        let mut moved = false;
                        if let Some(pm) = world.get_resource::<PendingMove>()
                            && pm.unit_entity == ue
                            && let Some(pos) = world.get::<GridPosition>(ue)
                        {
                            moved = pos.x != pm.original_pos.x || pos.y != pm.original_pos.y;
                        }
                        if moved {
                            world.send_event(engine::events::UndoMoveCommand);
                        }
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
            ActionType::SaveGame => {
                let (options, files) = self.get_slot_status();
                self.ui_state.in_game_state = InGameState::SaveSelection {
                    selected_index: 0,
                    options,
                    files,
                };
            }
            ActionType::LoadGame => {
                let (options, files) = self.get_slot_status();
                self.ui_state.in_game_state = InGameState::LoadSelection {
                    selected_index: 0,
                    options,
                    files,
                    is_title_screen: false,
                };
            }
            ActionType::Produce => {
                let mut options = Vec::new();
                if let Some(world) = &mut self.world {
                    let mut player_funds = 0;

                    if let (Some(match_state), Some(players)) = (
                        world.get_resource::<MatchState>(),
                        world.get_resource::<Players>(),
                    ) {
                        player_funds = players.0[match_state.active_player_index.0].funds;
                    }

                    let mut landscape_name = None;
                    let mut p_query = world.query::<(&GridPosition, &Property)>();
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

                    // マスターデータの定義順（unit.csvの並び順）に従って、生産可能なユニットを走査します。
                    for name in &self.master_data.unit_order {
                        if let Some(record) = self.master_data.units.get(name) {
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
                        if let Some(pm) = world.get_resource::<PendingMove>()
                            && pm.unit_entity == entity
                            && let Some(pos) = world.get::<GridPosition>(entity)
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
                                self.ui_state.add_log("修復しています...".to_string());
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
                                && let Ok(cargo) =
                                    world.query::<&CargoCapacity>().get(world, entity)
                            {
                                for &p_ent in &cargo.loaded {
                                    if let Some(act) = world.get::<ActionCompleted>(p_ent)
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
                    world.get_resource::<MatchState>(),
                    world.get_resource::<Players>(),
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
                if let Some(pos) = world.get::<GridPosition>(target)
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
        reachable_tiles: std::collections::BTreeSet<(usize, usize)>,
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
        if let Some(pm) = world.get_resource::<PendingMove>()
            && pm.unit_entity == unit_entity
            && let Some(pos) = world.get::<GridPosition>(unit_entity)
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

        options.push(ActionType::Cancel);

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
        // AIモードトグルのためのホットキー ('p')
        if let crossterm::event::KeyCode::Char('p') = key.code
            && let Some(world) = &self.world
            && let Some(match_state) = world.get_resource::<MatchState>()
            && let Some(players) = world.get_resource::<Players>()
            && let Some(active_player) = players.0.get(match_state.active_player_index.0)
        {
            let pid = active_player.id.0;
            let new_ctrl = self.ui_state.toggle_player_control(pid);
            if let Some(world) = &mut self.world {
                self.ui_state.apply_ai_versions_to_world(world);
            }

            self.ui_state.add_log(format!(
                "Player {} is now {}",
                pid,
                match new_ctrl {
                    PlayerControlType::Human => "Human".to_string(),
                    PlayerControlType::Ai => {
                        format!("AI({})", self.ui_state.ai_version(pid).label())
                    }
                }
            ));
        }

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

        let (mut world, schedule) = initialize_world_from_master_data_with_topology(
            &self.master_data,
            &map_name,
            self.ui_state.selected_topology,
        )?;
        self.ui_state.apply_ai_versions_to_world(&mut world);

        self.world = Some(world);
        self.schedule = Some(schedule);
        Ok(())
    }

    fn return_to_map_selection(&mut self) {
        self.world = None;
        self.schedule = None;
        self.ui_state.current_screen = CurrentScreen::MapSelection;
        self.ui_state.in_game_state = InGameState::Normal;
        self.ui_state.cursor_pos = (0, 0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::resources::Player;

    #[test]
    fn default_player_setup_is_human_and_v3_ai() {
        let state = UiState::new(vec!["map_1".to_string()]);

        assert!(state.is_human(1));
        assert!(!state.is_human(2));
        assert_eq!(state.ai_version(1), CliAiVersion::V3);
        assert_eq!(state.ai_version(2), CliAiVersion::V3);
        assert_eq!(state.control_label(2), "AI(V3)");
    }

    #[test]
    fn map_selection_cycles_human_v1_v3_v4_v100_v200() {
        let mut state = UiState::new(vec![]);

        state.cycle_player_setup(1);
        assert!(!state.is_human(1));
        assert_eq!(state.ai_version(1), CliAiVersion::V1);

        state.cycle_player_setup(1);
        assert!(!state.is_human(1));
        assert_eq!(state.ai_version(1), CliAiVersion::V3);

        state.cycle_player_setup(1);
        assert!(!state.is_human(1));
        assert_eq!(state.ai_version(1), CliAiVersion::V4);

        state.cycle_player_setup(1);
        assert!(!state.is_human(1));
        assert_eq!(state.ai_version(1), CliAiVersion::V100);

        state.cycle_player_setup(1);
        assert!(!state.is_human(1));
        assert_eq!(state.ai_version(1), CliAiVersion::V200);

        state.cycle_player_setup(1);
        assert!(state.is_human(1));
    }

    #[test]
    fn in_game_toggle_preserves_selected_version() {
        let mut state = UiState::new(vec![]);
        state.cycle_player_setup(1);

        assert_eq!(state.toggle_player_control(1), PlayerControlType::Human);
        assert_eq!(state.toggle_player_control(1), PlayerControlType::Ai);
        assert_eq!(state.ai_version(1), CliAiVersion::V1);
    }

    #[test]
    fn ui_versions_are_applied_to_world() {
        let mut state = UiState::new(vec![]);
        state.cycle_player_setup(1);
        let mut world = World::new();
        world.insert_resource(Players(vec![
            Player::new(1, "Player 1".to_string()),
            Player::new(2, "Player 2".to_string()),
        ]));

        state.apply_ai_versions_to_world(&mut world);

        assert_eq!(
            resolve_player_ai_version(&world, engine::components::PlayerId(1)),
            AiVersion::V1
        );
        assert_eq!(
            resolve_player_ai_version(&world, engine::components::PlayerId(2)),
            AiVersion::V3
        );
    }

    #[test]
    fn loaded_v2_is_normalized_to_v3_for_cli() {
        let mut state = UiState::new(vec![]);
        let mut world = World::new();
        world.insert_resource(Players(vec![Player::new(2, "Player 2".to_string())]));
        let mut settings = PlayerAiSettings::default();
        settings.set_version(engine::components::PlayerId(2), AiVersion::V2);
        world.insert_resource(settings);

        state.adopt_ai_versions_from_world(&mut world);

        assert_eq!(state.ai_version(2), CliAiVersion::V3);
        assert_eq!(
            resolve_player_ai_version(&world, engine::components::PlayerId(2)),
            AiVersion::V3
        );
    }
}
