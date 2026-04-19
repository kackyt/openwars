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
            Option<&engine::components::ActionCompleted>,
            Option<&engine::components::CargoCapacity>,
        )>();
        for (pos, faction, stats, transporting, action, cargo) in u_query.iter(world) {
            if transporting.is_some() {
                continue;
            }
            let is_completed = action.map(|a| a.0).unwrap_or(false);
            let has_cargo = cargo.map(|c| !c.loaded.is_empty()).unwrap_or(false);

            units.insert(
                (pos.x, pos.y),
                (faction.0.0, stats.unit_type, is_completed, has_cargo),
            );
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

                    if let Some(&(owner, u_type, is_completed, has_cargo)) = units.get(&(x, y)) {
                        symbol = unit_type_to_symbol(&u_type);
                        // 基本色
                        let mut u_style = style
                            .fg(if owner == 1 {
                                Color::LightBlue
                            } else {
                                Color::LightRed
                            })
                            .add_modifier(Modifier::BOLD);

                        // 背景色（優先度の低い順に適用）

                        // 1. 移動可能範囲（青系/グレー）
                        if reachable_tiles.contains(&(x, y)) {
                            u_style = u_style.bg(Color::DarkGray).fg(Color::White);
                        }

                        // 2. 行動済み（反転/減衰系）
                        if is_completed {
                            u_style = u_style
                                .remove_modifier(Modifier::BOLD)
                                .bg(if owner == 1 {
                                    Color::Rgb(40, 80, 160)
                                } else {
                                    Color::Rgb(160, 60, 60)
                                })
                                .fg(Color::Black);
                        }

                        // 3. ターゲット（赤）
                        if target_tiles.contains(&(x, y)) {
                            u_style = u_style
                                .bg(Color::Rgb(150, 0, 0))
                                .fg(Color::White)
                                .add_modifier(Modifier::BOLD);
                        }

                        // 4. カーソル位置（白） - 最優先
                        if x == cx && y == cy {
                            u_style = u_style
                                .bg(Color::White)
                                .fg(Color::Black)
                                .add_modifier(Modifier::BOLD);
                        }

                        if has_cargo {
                            line_spans.push(Span::styled(format!(" {}*", symbol), u_style));
                        } else {
                            line_spans.push(Span::styled(format!(" {} ", symbol), u_style));
                        }
                    } else {
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
                    if let (Ok(stats), Some(h)) = (
                        world
                            .query::<&engine::components::UnitStats>()
                            .get(world, *entity),
                        world.get::<engine::components::Health>(*entity),
                    ) {
                        let mut info = format!("{} ", stats.unit_type.as_str());
                        let display_hp = (h.current.saturating_add(9)) / 10;
                        info.push_str(&format!("HP:{:2} ", display_hp));

                        if let Some(f) = world.get::<engine::components::Fuel>(*entity) {
                            info.push_str(&format!("燃料:{:2}/{:2} ", f.current, f.max));
                        }
                        if let Some(a) = world.get::<engine::components::Ammo>(*entity) {
                            if a.max_ammo1 > 0 {
                                info.push_str(&format!("弾1:{:2}/{:2} ", a.ammo1, a.max_ammo1));
                            }
                            if a.max_ammo2 > 0 {
                                info.push_str(&format!("弾2:{:2}/{:2} ", a.ammo2, a.max_ammo2));
                            }
                        }
                        options.push(info);
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

        let mut menu_width = 30u16;
        for opt in &options {
            // 日本語文字(2byte)を考慮して簡易的に計算
            let width = opt
                .chars()
                .map(|c| if c.is_ascii() { 1 } else { 2 })
                .sum::<u16>()
                + 4;
            if width > menu_width {
                menu_width = width;
            }
        }
        let menu_width = menu_width.min(f.size().width.saturating_sub(menu_x)); // 画面端を超えないように制限

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

    // 右側: 情報 & ログの表示準備
    let mut info_text = String::new();

    if let Some(world) = &mut app.world {
        // プレイヤー情報
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
            info_text.push_str(&format!("ターン: {} (P{} : {})\n", turn, id, name));
            info_text.push_str(&format!("資金: {}\n\n", funds));
        }

        // ユニット情報
        for (
            u_pos,
            u_faction,
            u_stats,
            u_health,
            u_fuel,
            u_ammo,
            transporting,
            action_completed,
            has_moved,
            cargo_capacity,
        ) in world
            .query::<(
                &engine::components::GridPosition,
                &engine::components::Faction,
                &engine::components::UnitStats,
                &engine::components::Health,
                Option<&engine::components::Fuel>,
                Option<&engine::components::Ammo>,
                Option<&engine::components::Transporting>,
                &engine::components::ActionCompleted,
                Option<&engine::components::HasMoved>,
                Option<&engine::components::CargoCapacity>,
            )>()
            .iter(world)
        {
            if transporting.is_some() {
                continue;
            }
            if u_pos.x == cx && u_pos.y == cy {
                info_text.push_str("--- ユニット情報 ---\n");
                info_text.push_str(&format!(
                    "{} (P{})\n",
                    u_stats.unit_type.as_str(),
                    u_faction.0.0
                ));

                let display_hp = (u_health.current.saturating_add(9)) / 10;
                let mut hp_fuel = format!("HP: {}/10", display_hp);
                if let Some(f) = u_fuel {
                    hp_fuel.push_str(&format!("  燃料: {}/{}", f.current, f.max));
                }
                info_text.push_str(&format!("{}\n", hp_fuel));

                if let Some(w) = u_ammo
                    && (w.max_ammo1 > 0 || w.max_ammo2 > 0)
                {
                    let mut ammo_line = String::new();
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
                        ammo_line.push_str(&format!("{}: {}/{}", w_name, w.ammo1, w.max_ammo1));
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
                        if !ammo_line.is_empty() {
                            ammo_line.push_str("  ");
                        }
                        ammo_line.push_str(&format!("{}: {}/{}", w_name, w.ammo2, w.max_ammo2));
                    }
                    info_text.push_str(&format!("{}\n", ammo_line));
                }

                let status = if action_completed.0 {
                    "行動終了"
                } else if has_moved.map(|h| h.0).unwrap_or(false) {
                    "移動済み"
                } else {
                    "未行動"
                };
                let mut status_line = format!("状態: {}", status);
                if let Some(cargo) = cargo_capacity
                    && !cargo.loaded.is_empty()
                {
                    status_line.push_str(&format!("  搭載: {}体", cargo.loaded.len()));
                }
                info_text.push_str(&format!("{}\n", status_line));
                info_text.push_str("-----------------\n\n");
                break;
            }
        }

        // 地形情報の表示
        if let Some(map) = world.get_resource::<engine::resources::Map>()
            && let Some(terrain) = map.get_terrain(cx, cy)
        {
            info_text.push_str("--- 地形情報 ---\n");
            let terrain_name = terrain.as_str();
            info_text.push_str(&format!("地形: {}\n", terrain_name));
            if let Some(master_data) = world.get_resource::<engine::resources::MasterDataRegistry>()
            {
                info_text.push_str(&format!(
                    "防御: +{}%\n",
                    master_data.get_terrain_defense_bonus(terrain)
                ));
            }

            let mut q_prop = world.query::<(
                &engine::components::GridPosition,
                &engine::components::Property,
            )>();
            for (p_pos, prop) in q_prop.iter(world) {
                if p_pos.x == cx && p_pos.y == cy {
                    info_text.push_str(&format!(
                        "占領: {}/{}\n",
                        prop.display_capture_points(),
                        prop.display_max_capture_points()
                    ));
                    break;
                }
            }
            info_text.push_str("-----------------\n\n");
        }
    }
    info_text.push_str("q:終了 / Esc:戻る\n方向キー:移動 / Space:決定\nx:キャンセル");

    // レイアウト計算：表示内容の行数に応じて情報パネルの高さを調整
    // 行数に上下のボーダー分（+2）と、ある程度の余白を加えて動的に計算
    let info_height = (info_text.lines().count() as u16).saturating_add(3);
    let info_height = info_height.max(12); // 最低限の高さを確保
    let right_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(info_height), Constraint::Min(0)])
        .split(chunks[1]);

    let info_block = Block::default().title(" 情報 ").borders(Borders::ALL);
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

    match &app.ui_state.in_game_state {
        crate::app::InGameState::EventPopup { message } => {
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
        crate::app::InGameState::GameOverPopup {
            message,
            condition: _,
        } => {
            let area = f.size();
            let popup_rect = ratatui::layout::Rect {
                x: area.width.saturating_sub(40) / 2,
                y: area.height.saturating_sub(5) / 2,
                width: 40.min(area.width),
                height: 5.min(area.height),
            };
            let title = " ゲームセット ";
            let s = format!("{}\n\n[Esc/Enter] で戻る", message);
            let popup_text = Paragraph::new(s.as_str())
                .block(
                    Block::default()
                        .title(title)
                        .borders(Borders::ALL)
                        .style(Style::default().bg(Color::Yellow).fg(Color::Black)),
                )
                .alignment(ratatui::layout::Alignment::Center)
                .wrap(Wrap { trim: true });
            f.render_widget(ratatui::widgets::Clear, popup_rect);
            f.render_widget(popup_text, popup_rect);
        }
        _ => {}
    }
}
