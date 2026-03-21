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
            menu_data = Some(("Action", options.clone(), *selected_index));
        }
        crate::app::InGameState::ProductionMenu {
            options,
            selected_index,
            ..
        } => {
            menu_data = Some(("Produce", options.clone(), *selected_index));
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

    if let Some(world) = &app.world {
        if let (Some(match_state), Some(players)) = (world.get_resource::<openwars_engine::resources::MatchState>(), world.get_resource::<openwars_engine::resources::Players>()) {
            if !players.0.is_empty() {
                let active_player = &players.0[match_state.active_player_index.0];
                info_text.push_str(&format!("Turn: {}\n", match_state.current_turn_number.0));
                info_text.push_str(&format!("Player: {} ({})\n", active_player.name, active_player.id.0));
                info_text.push_str(&format!("Funds: {}\n\n", active_player.funds));
            }
        }
    }
    info_text.push_str("Press [q] to quit.\nPress [Esc] map.\nUse [Arrows] move.\nPress [Space] action.");

    let info_paragraph = Paragraph::new(info_text).block(info_block);
    f.render_widget(info_paragraph, right_chunks[0]);

    // Logs
    let logs_block = Block::default().title(" Logs ").borders(Borders::ALL);
    let logs_text = app.ui_state.log_messages.join("\n");
    let logs_paragraph = Paragraph::new(logs_text).block(logs_block);
    f.render_widget(logs_paragraph, right_chunks[1]);
}
