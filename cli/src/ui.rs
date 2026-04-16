use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph, Wrap},
};

use crate::app::{App, CurrentScreen};

pub fn ui(f: &mut Frame, app: &mut App) {
    match app.ui_state.current_screen {
        CurrentScreen::MapSelection => draw_map_selection(f, app),
        CurrentScreen::InGame => draw_in_game(f, app),
    }
}

fn unit_type_to_symbol(unit_type: &engine::resources::UnitType) -> &'static str {
    unit_type.symbol()
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
    let title = Paragraph::new("プレイするマップを選択してください").block(title_block);
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

    let maps_list =
        List::new(items).block(Block::default().borders(Borders::ALL).title(" マップ一覧 "));
    f.render_widget(maps_list, chunks[1]);

    let footer = Paragraph::new("方向キー(↑/↓)で選択、Enterで決定、qで終了")
        .block(Block::default().borders(Borders::ALL));
    f.render_widget(footer, chunks[2]);
}

fn draw_in_game(f: &mut Frame, app: &mut App) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .margin(1)
        .constraints([Constraint::Percentage(75), Constraint::Percentage(25)])
        .split(f.size());

    // 左側: マップ表示
    let map_block = Block::default().title(" マップ ").borders(Borders::ALL);

    let mut map_lines = vec![];
    let cx = app.ui_state.cursor_pos.0;
    let cy = app.ui_state.cursor_pos.1;

    if let Some(world) = &mut app.world {
        // ユニット/不動産情報の収集
        let mut factions = std::collections::HashMap::new();
        let mut units = std::collections::HashMap::new();

        // 不動産のクエリ
        let mut p_query = world.query::<(
            &engine::components::GridPosition,
            &engine::components::Property,
        )>();
        for (pos, prop) in p_query.iter(world) {
            if let Some(owner) = prop.owner_id {
                factions.insert((pos.x, pos.y), owner.0);
            }
        }

        // ユニットのクエリ
        let mut u_query = world.query::<(
            &engine::components::GridPosition,
            &engine::components::Faction,
            &engine::components::UnitStats,
            Option<&engine::components::Transporting>,
        )>();
        for (pos, faction, stats, transporting) in u_query.iter(world) {
            if transporting.is_some() {
                continue;
            }
            units.insert((pos.x, pos.y), (faction.0.0, stats.unit_type));
        }

        // 到達可能タイルの収集
        let mut reachable_tiles = std::collections::HashSet::new();
        if let crate::app::InGameState::UnitSelected {
            reachable_tiles: rt,
            ..
        } = &app.ui_state.in_game_state
        {
            for pos in rt {
                reachable_tiles.insert(*pos);
            }
        }

        // ターゲットタイルの収集
        let mut target_tiles = std::collections::HashSet::new();
        match &app.ui_state.in_game_state {
            crate::app::InGameState::TargetSelection { targets, .. } => {
                let mut q_pos = world.query::<&engine::components::GridPosition>();
                for entity in targets {
                    if let Ok(pos) = q_pos.get(world, *entity) {
                        target_tiles.insert((pos.x, pos.y));
                    }
                }
            }
            crate::app::InGameState::DropTargetSelection { targets, .. } => {
                for pos in targets {
                    target_tiles.insert(*pos);
                }
            }
            _ => {}
        }

        if let Some(map_res) = world.get_resource::<engine::resources::Map>() {
            for y in 0..map_res.height {
                let mut line_spans = vec![];
                for x in 0..map_res.width {
                    let terrain = map_res
                        .get_terrain(x, y)
                        .unwrap_or(engine::resources::Terrain::Plains);

                    let mut symbol = terrain.symbol();

                    let mut style = Style::default().fg(Color::DarkGray);

                    if let Some(owner) = factions.get(&(x, y)) {
                        style = style.fg(if *owner == 1 { Color::Blue } else { Color::Red });
                    }

                    if let Some((owner, u_type)) = units.get(&(x, y)) {
                        symbol = unit_type_to_symbol(u_type);
                        style = style
                            .fg(if *owner == 1 {
                                Color::LightBlue
                            } else {
                                Color::LightRed
                            })
                            .add_modifier(Modifier::BOLD);
                    }

                    if reachable_tiles.contains(&(x, y)) {
                        style = style.bg(Color::DarkGray).fg(Color::White);
                    }

                    if target_tiles.contains(&(x, y)) {
                        style = style
                            .bg(Color::Rgb(150, 0, 0))
                            .fg(Color::White)
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
            let labels: Vec<String> = options.iter().map(|o| o.label().to_string()).collect();
            menu_data = Some(("アクション".to_string(), labels, *selected_index));
        }
        crate::app::InGameState::ProductionMenu {
            options,
            selected_index,
            ..
        } => {
            menu_data = Some(("生産".to_string(), options.clone(), *selected_index));
        }
        crate::app::InGameState::TargetSelection { action, .. } => {
            menu_data = Some((
                action.clone(),
                vec!["[カーソルで対象を選択]".to_string()],
                0,
            ));
        }
        crate::app::InGameState::CargoSelection {
            passengers,
            selected_index,
            ..
        } => {
            let mut options = Vec::new();
            if let Some(world) = &mut app.world {
                for entity in passengers {
                    let mut q = world.query::<&engine::components::UnitStats>();
                    if let Ok(stats) = q.get(world, *entity) {
                        options.push(stats.unit_type.as_str().to_string());
                    } else {
                        options.push(format!("{:?}", entity));
                    }
                }
            } else {
                options = passengers.iter().map(|e| format!("{:?}", e)).collect();
            }
            if options.is_empty() {
                options.push("なし".to_string());
            }
            menu_data = Some(("何を降ろしますか？".to_string(), options, *selected_index));
        }
        crate::app::InGameState::DropTargetSelection { .. } => {
            menu_data = Some((
                "どこに降ろしますか？".to_string(),
                vec!["[カーソルで対象を選択]".to_string()],
                0,
            ));
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

        // カーソル付近へのメニューのオーバーレイ表示
        let map_x = chunks[0].x + 1;
        let map_y = chunks[0].y + 1;

        // カーソル座標 (x, y) はマップ内の相対座標。これを絶対座標に変換
        let mut menu_x = map_x + (cx as u16 * 3) + 4; // 記号が " X " なので 3マス分
        let mut menu_y = map_y + cy as u16;

        let menu_width = 30;
        let menu_height = (options.len() as u16) + 2;

        // 画面端の考慮
        if menu_x + menu_width > chunks[0].x + chunks[0].width {
            menu_x = (map_x + (cx as u16 * 3)).saturating_sub(menu_width);
        }
        if menu_y + menu_height > chunks[0].y + chunks[0].height {
            menu_y = (chunks[0].y + chunks[0].height).saturating_sub(menu_height);
        }

        let menu_rect = ratatui::layout::Rect {
            x: menu_x,
            y: menu_y,
            width: menu_width,
            height: menu_height,
        };
        f.render_widget(ratatui::widgets::Clear, menu_rect);

        let menu_list =
            List::new(menu_items).block(Block::default().borders(Borders::ALL).title(title));
        f.render_widget(menu_list, menu_rect);
    }

    // 右側: 情報 & ログの分割表示
    let right_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(chunks[1]);

    // 情報パネル
    let info_block = Block::default().title(" 情報 ").borders(Borders::ALL);
    let mut info_text = String::new();

    if let Some(world) = &mut app.world {
        if let (Some(match_state), Some(players)) = (
            world.get_resource::<engine::resources::MatchState>(),
            world.get_resource::<engine::resources::Players>(),
        ) && !players.0.is_empty()
        {
            let active_player = &players.0[match_state.active_player_index.0];
            let turn = match_state.current_turn_number.0;
            let name = active_player.name.clone();
            let id = active_player.id.0;
            let funds = active_player.funds;
            info_text.push_str(&format!("ターン: {}\n", turn));
            info_text.push_str(&format!("プレイヤー: {} ({})\n", name, id));
            info_text.push_str(&format!("資金: {}\n\n", funds));
        }

        let cx = app.ui_state.cursor_pos.0;
        let cy = app.ui_state.cursor_pos.1;
        let mut u_query = world.query::<(
            &engine::components::GridPosition,
            &engine::components::Faction,
            &engine::components::UnitStats,
            &engine::components::Health,
            Option<&engine::components::Fuel>,
            Option<&engine::components::Ammo>,
            Option<&engine::components::Transporting>,
        )>();

        for (u_pos, u_faction, u_stats, u_health, u_fuel, u_ammo, transporting) in
            u_query.iter(world)
        {
            if transporting.is_some() {
                continue;
            }
            if u_pos.x == cx && u_pos.y == cy {
                info_text.push_str("--- ユニット情報 ---\n");
                info_text.push_str(&format!("種別: {}\n", u_stats.unit_type.as_str()));
                info_text.push_str(&format!("勢力: P{}\n", u_faction.0.0));

                let display_hp = (u_health.current.saturating_add(9)) / 10;
                info_text.push_str(&format!("HP: {}/10\n", display_hp));

                if let Some(f) = u_fuel {
                    info_text.push_str(&format!("燃料: {}/{}\n", f.current, f.max));
                }

                if let Some(w) = u_ammo {
                    if w.max_ammo1 > 0 {
                        let mut w_name = "武器1";
                        if let Some(record) =
                            app.master_data
                                .get_unit(&engine::resources::master_data::UnitName(
                                    u_stats.unit_type.as_str().to_string(),
                                ))
                            && let Some(name) = &record.weapon1
                        {
                            w_name = name;
                        }
                        info_text.push_str(&format!("{}: {}/{}\n", w_name, w.ammo1, w.max_ammo1));
                    }
                    if w.max_ammo2 > 0 {
                        let mut w_name = "武器2";
                        if let Some(record) =
                            app.master_data
                                .get_unit(&engine::resources::master_data::UnitName(
                                    u_stats.unit_type.as_str().to_string(),
                                ))
                            && let Some(name) = &record.weapon2
                        {
                            w_name = name;
                        }
                        info_text.push_str(&format!("{}: {}/{}\n", w_name, w.ammo2, w.max_ammo2));
                    }
                }

                info_text.push_str("-----------------\n\n");
                break;
            }
        }
    }
    info_text.push_str(
        "q: 終了 / Esc: マップ選択へ戻る\n方向キー: カーソル移動 / Space: アクション\nx: 戻る・キャンセル",
    );

    let info_paragraph = Paragraph::new(info_text)
        .block(info_block)
        .wrap(Wrap { trim: true });
    f.render_widget(info_paragraph, right_chunks[0]);

    // ログパネル
    let logs_block = Block::default().title(" ログ ").borders(Borders::ALL);
    let logs_text = app
        .ui_state
        .log_messages
        .iter()
        .rev()
        .cloned()
        .collect::<Vec<_>>()
        .join("\n");
    let logs_paragraph = Paragraph::new(logs_text)
        .block(logs_block)
        .wrap(Wrap { trim: true });
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
            .title(" イベント ")
            .style(Style::default().bg(Color::Blue).fg(Color::White));
        let popup_text = Paragraph::new(message.as_str())
            .block(popup_block)
            .alignment(ratatui::layout::Alignment::Center)
            .wrap(Wrap { trim: true });
        f.render_widget(ratatui::widgets::Clear, popup_rect);
        f.render_widget(popup_text, popup_rect);
    }
}
