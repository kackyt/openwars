use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph},
};

use crate::app::{App, CurrentScreen};

pub fn ui(f: &mut Frame, app: &mut App) {
    match app.ui_state.current_screen {
        CurrentScreen::MapSelection => draw_map_selection(f, app),
        CurrentScreen::InGame => draw_in_game(f, app),
    }
}

fn unit_type_to_jp(unit_type: &openwars_engine::resources::UnitType) -> &'static str {
    use openwars_engine::resources::UnitType::*;
    match unit_type {
        Infantry => "歩兵",
        Mech => "重歩兵",
        CombatEngineer => "工兵",
        Recon => "装甲車",
        Tank => "軽戦車",
        MdTank => "中戦車",
        TankZ => "重戦車",
        Artillery => "自走砲",
        Rockets => "ロケット砲",
        AntiAir => "対空戦車",
        Missiles => "対空ミサイル",
        Fighter => "戦闘機",
        Bomber => "爆撃機",
        Bcopters => "戦闘ヘリ",
        TransportHelicopter => "輸送ヘリ",
        Battleship => "戦艦",
        Cruiser => "巡洋艦",
        Lander => "輸送船",
        Submarine => "潜水艦",
        SupplyTruck => "輸送車",
    }
}

fn draw_map_selection(f: &mut Frame, app: &mut App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .margin(2)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(0),
            Constraint::Length(3),
        ])
        .split(f.size());

    let title_block = Block::default()
        .borders(Borders::ALL)
        .title(" OpenWars CLI ")
        .style(Style::default().fg(Color::Cyan));
    let title = Paragraph::new("Select a Map to Play").block(title_block);
    f.render_widget(title, chunks[0]);

    let items: Vec<ListItem> = app
        .ui_state
        .available_maps
        .iter()
        .enumerate()
        .map(|(i, m)| {
            let style = if i == app.ui_state.selected_map_index {
                Style::default()
                    .bg(Color::White)
                    .fg(Color::Black)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            ListItem::new(Line::from(Span::styled(format!(" {} ", m), style)))
        })
        .collect();

    let maps_list = List::new(items).block(Block::default().borders(Borders::ALL).title(" Maps "));
    f.render_widget(maps_list, chunks[1]);

    let footer =
        Paragraph::new("Use [Up/Down] to navigate. Press [Enter] to select. Press [q] to quit.")
            .block(Block::default().borders(Borders::ALL));
    f.render_widget(footer, chunks[2]);
}

fn draw_in_game(f: &mut Frame, app: &mut App) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .margin(1)
        .constraints([Constraint::Percentage(75), Constraint::Percentage(25)])
        .split(f.size());

    // Left side: Map
    let map_block = Block::default().title(" Map ").borders(Borders::ALL);

    let mut map_lines = vec![];
    let cx = app.ui_state.cursor_pos.0;
    let cy = app.ui_state.cursor_pos.1;

    let mut reachable_tiles = None;
    if let crate::app::InGameState::UnitSelected {
        reachable_tiles: r, ..
    } = &app.ui_state.in_game_state
    {
        reachable_tiles = Some(r);
    }

    if let Some(world) = &mut app.world {
        // Collect unit/property info
        let mut factions = std::collections::HashMap::new();
        let mut units = std::collections::HashMap::new();

        // Property query
        let mut p_query = world.query::<(
            &openwars_engine::components::GridPosition,
            &openwars_engine::components::Property,
        )>();
        for (pos, prop) in p_query.iter(world) {
            if let Some(owner) = prop.owner_id {
                factions.insert((pos.x, pos.y), owner.0);
            }
        }

        // Unit query
        let mut u_query = world.query::<(
            &openwars_engine::components::GridPosition,
            &openwars_engine::components::Faction,
            &openwars_engine::components::UnitStats,
        )>();
        for (pos, faction, stats) in u_query.iter(world) {
            units.insert((pos.x, pos.y), (faction.0.0, stats.unit_type));
        }

        if let Some(map_res) = world.get_resource::<openwars_engine::resources::Map>() {
            for y in 0..map_res.height {
                let mut line_spans = vec![];
                for x in 0..map_res.width {
                    let terrain = map_res
                        .get_terrain(x, y)
                        .unwrap_or(openwars_engine::resources::Terrain::Plains);

                    let mut symbol = match terrain {
                        openwars_engine::resources::Terrain::Plains => ".",
                        openwars_engine::resources::Terrain::Road => "=",
                        openwars_engine::resources::Terrain::River => "~",
                        openwars_engine::resources::Terrain::Bridge => "=",
                        openwars_engine::resources::Terrain::Mountain => "^",
                        openwars_engine::resources::Terrain::Forest => "\"",
                        openwars_engine::resources::Terrain::Sea => "~",
                        openwars_engine::resources::Terrain::Shoal => ",",
                        openwars_engine::resources::Terrain::City => "C",
                        openwars_engine::resources::Terrain::Factory => "F",
                        openwars_engine::resources::Terrain::Airport => "A",
                        openwars_engine::resources::Terrain::Port => "P",
                        openwars_engine::resources::Terrain::Capital => "H",
                    };

                    let mut style = Style::default().fg(Color::DarkGray);

                    if let Some(owner) = factions.get(&(x, y)) {
                        style = style.fg(if *owner == 1 { Color::Blue } else { Color::Red });
                    }

                    if let Some((owner, u_type)) = units.get(&(x, y)) {
                        symbol = match u_type {
                            openwars_engine::resources::UnitType::Infantry => "i",
                            openwars_engine::resources::UnitType::Tank => "T",
                            openwars_engine::resources::UnitType::MdTank => "M",
                            openwars_engine::resources::UnitType::Recon => "R",
                            openwars_engine::resources::UnitType::Artillery => "a",
                            openwars_engine::resources::UnitType::Rockets => "r",
                            openwars_engine::resources::UnitType::AntiAir => "A",
                            openwars_engine::resources::UnitType::Missiles => "m",
                            openwars_engine::resources::UnitType::Fighter => "F",
                            openwars_engine::resources::UnitType::Bomber => "B",
                            openwars_engine::resources::UnitType::Bcopters => "b",
                            openwars_engine::resources::UnitType::TransportHelicopter => "h",
                            openwars_engine::resources::UnitType::Battleship => "S",
                            openwars_engine::resources::UnitType::Cruiser => "c",
                            openwars_engine::resources::UnitType::Lander => "l",
                            openwars_engine::resources::UnitType::Submarine => "s",
                            openwars_engine::resources::UnitType::SupplyTruck => "t",
                            _ => "?",
                        };
                        style = style
                            .fg(if *owner == 1 {
                                Color::LightBlue
                            } else {
                                Color::LightRed
                            })
                            .add_modifier(Modifier::BOLD);
                    }

                    if let Some(reachable) = reachable_tiles
                        && reachable.contains(&(x, y))
                    {
                        style = style.bg(Color::DarkGray).fg(Color::White);
                    }

                    if x == cx && y == cy {
                        style = style.bg(Color::White).fg(Color::Black);
                    }

                    line_spans.push(Span::styled(format!(" {} ", symbol), style));
                }
                map_lines.push(Line::from(line_spans));
            }
        }
    }

    let map_paragraph = Paragraph::new(map_lines).block(map_block);
    f.render_widget(map_paragraph, chunks[0]);

    let mut menu_data = None;
    match &app.ui_state.in_game_state {
        crate::app::InGameState::ActionMenu {
            options,
            selected_index,
            ..
        } => {
            menu_data = Some(("Action".to_string(), options.clone(), *selected_index));
        }
        crate::app::InGameState::ProductionMenu {
            options,
            selected_index,
            ..
        } => {
            menu_data = Some(("Produce".to_string(), options.clone(), *selected_index));
        }
        crate::app::InGameState::TargetSelection {
            action,
            ..
        } => {
            menu_data = Some((action.clone(), vec!["[Select with Cursor]".to_string()], 0));
        }
        crate::app::InGameState::CargoSelection {
            passengers,
            selected_index,
            ..
        } => {
            let mut options = Vec::new();
            if let Some(world) = &mut app.world {
                for entity in passengers {
                    let mut q = world.query::<&openwars_engine::components::UnitStats>();
                    if let Ok(stats) = q.get(world, *entity) {
                        options.push(unit_type_to_jp(&stats.unit_type).to_string());
                    } else {
                        options.push(format!("{:?}", entity));
                    }
                }
            } else {
                options = passengers.iter().map(|e| format!("{:?}", e)).collect();
            }
            if options.is_empty() {
                options.push("None".to_string());
            }
            menu_data = Some(("Drop which?".to_string(), options, *selected_index));
        }
        crate::app::InGameState::DropTargetSelection {
            ..
        } => {
            menu_data = Some(("Drop where?".to_string(), vec!["[Select with Cursor]".to_string()], 0));
        }
        _ => {}
    }

    if let Some((title, options, selected_index)) = menu_data {
        let menu_items: Vec<ListItem> = options
            .iter()
            .enumerate()
            .map(|(i, opt)| {
                let style = if i == selected_index {
                    Style::default()
                        .bg(Color::White)
                        .fg(Color::Black)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };
                ListItem::new(Span::styled(format!(" {} ", opt), style))
            })
            .collect();

        // Overlay menu near cursor
        let menu_rect = ratatui::layout::Rect {
            x: chunks[0].x + 2, // simplified placement
            y: chunks[0].y + 2,
            width: 20,
            height: (options.len() as u16) + 2,
        };
        let menu_block = ratatui::widgets::Clear;
        f.render_widget(menu_block, menu_rect);

        let menu_list =
            List::new(menu_items).block(Block::default().borders(Borders::ALL).title(title));
        f.render_widget(menu_list, menu_rect);
    }

    // Right side split: Info & Logs
    let right_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(chunks[1]);

    // Info
    let info_block = Block::default().title(" Info ").borders(Borders::ALL);
    let mut info_text = String::new();

    if let Some(world) = &mut app.world {
        if let (Some(match_state), Some(players)) = (
            world.get_resource::<openwars_engine::resources::MatchState>(),
            world.get_resource::<openwars_engine::resources::Players>(),
        ) && !players.0.is_empty()
        {
            let active_player = &players.0[match_state.active_player_index.0];
            let turn = match_state.current_turn_number.0;
            let name = active_player.name.clone();
            let id = active_player.id.0;
            let funds = active_player.funds;
            info_text.push_str(&format!("Turn: {}\n", turn));
            info_text.push_str(&format!("Player: {} ({})\n", name, id));
            info_text.push_str(&format!("Funds: {}\n\n", funds));
        }

        let cx = app.ui_state.cursor_pos.0;
        let cy = app.ui_state.cursor_pos.1;
        let mut u_query = world.query::<(
            &openwars_engine::components::GridPosition,
            &openwars_engine::components::Faction,
            &openwars_engine::components::UnitStats,
            &openwars_engine::components::Health,
            Option<&openwars_engine::components::Fuel>,
            Option<&openwars_engine::components::Ammo>,
        )>();

        for (u_pos, u_faction, u_stats, u_health, u_fuel, u_ammo) in u_query.iter(world) {
            if u_pos.x == cx && u_pos.y == cy {
                info_text.push_str("--- Unit Info ---\n");
                info_text.push_str(&format!("Type: {}\n", unit_type_to_jp(&u_stats.unit_type)));
                info_text.push_str(&format!("Faction: P{}\n", u_faction.0.0));

                let display_hp = (u_health.current.saturating_add(9)) / 10;
                info_text.push_str(&format!("HP: {}/10\n", display_hp));

                if let Some(f) = u_fuel {
                    info_text.push_str(&format!("Fuel: {}/{}\n", f.current, f.max));
                }

                if let Some(w) = u_ammo {
                    if w.max_ammo1 > 0 {
                        info_text.push_str(&format!("Ammo 1: {}/{}\n", w.ammo1, w.max_ammo1));
                    }
                    if w.max_ammo2 > 0 {
                        info_text.push_str(&format!("Ammo 2: {}/{}\n", w.ammo2, w.max_ammo2));
                    }
                }

                info_text.push_str("-----------------\n\n");
                break;
            }
        }
    }
    info_text.push_str(
        "Press [q] to quit.\nPress [Esc] map.\nUse [Arrows] move.\nPress [Space] action.",
    );

    let info_paragraph = Paragraph::new(info_text).block(info_block);
    f.render_widget(info_paragraph, right_chunks[0]);

    // Logs
    let logs_block = Block::default().title(" Logs ").borders(Borders::ALL);
    let logs_text = app.ui_state.log_messages.join("\n");
    let logs_paragraph = Paragraph::new(logs_text).block(logs_block);
    f.render_widget(logs_paragraph, right_chunks[1]);

    if let crate::app::InGameState::EventPopup { message } = &app.ui_state.in_game_state {
        let area = f.size();
        let popup_rect = ratatui::layout::Rect {
            x: area.width.saturating_sub(40) / 2,
            y: area.height.saturating_sub(5) / 2,
            width: 40.min(area.width),
            height: 5.min(area.height),
        };
        let popup_block = Block::default()
            .borders(Borders::ALL)
            .title(" Event ")
            .style(Style::default().bg(Color::Blue).fg(Color::White));
        let popup_text = Paragraph::new(message.as_str())
            .block(popup_block)
            .alignment(ratatui::layout::Alignment::Center);
        f.render_widget(ratatui::widgets::Clear, popup_rect);
        f.render_widget(popup_text, popup_rect);
    }
}
