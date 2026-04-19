cat << 'PATCH' > /tmp/cli_app.patch
--- cli/src/app.rs
+++ cli/src/app.rs
@@ -201,15 +201,15 @@
         // AIターンの場合は一部のキー（終了など）以外は無視する
         if let Some(world) = &self.world {
-            if let Some(match_state) = world.get_resource::<engine::resources::MatchState>()
-                && let Some(players) = world.get_resource::<engine::resources::Players>()
-            {
+            if let Some(match_state) = world.get_resource::<engine::resources::MatchState>() {
+                if let Some(players) = world.get_resource::<engine::resources::Players>() {
-                if let Some(active_player) = players.0.get(match_state.active_player_index.0)
-                    && let InGameState::Normal = self.ui_state.in_game_state
-                {
-                    if self.ui_state.player_controls.get(&active_player.id.0)
-                        == Some(&PlayerControlType::Ai)
-                    {
-                        match key.code {
-                            crossterm::event::KeyCode::Char('q') => self.should_quit = true,
-                            _ => return, // AIターン中は他の入力を無視
-                        }
-                    }
+                    if let Some(active_player) = players.0.get(match_state.active_player_index.0) {
+                        if let InGameState::Normal = self.ui_state.in_game_state {
+                            if self.ui_state.player_controls.get(&active_player.id.0) == Some(&PlayerControlType::Ai) {
+                                match key.code {
+                                    crossterm::event::KeyCode::Char('q') => self.should_quit = true,
+                                    _ => return, // AIターン中は他の入力を無視
+                                }
+                            }
+                        }
+                    }
                 }
             }
         }
@@ -1071,19 +1071,23 @@
         // AIモードトグルのためのホットキー ('p')
-        if let crossterm::event::KeyCode::Char('p') = key.code
-            && let Some(world) = &self.world
-        {
-            if let Some(match_state) = world.get_resource::<engine::resources::MatchState>()
-                && let Some(players) = world.get_resource::<engine::resources::Players>()
-            {
-                if let Some(active_player) =
-                    players.0.get(match_state.active_player_index.0)
-                {
-                    let pid = active_player.id.0;
-                    let new_ctrl = if let Some(ctrl) = self.ui_state.player_controls.get_mut(&pid) {
-                        *ctrl = match *ctrl {
-                            PlayerControlType::Human => PlayerControlType::Ai,
-                            PlayerControlType::Ai => PlayerControlType::Human,
-                        };
-                        Some(*ctrl)
-                    } else {
-                        None
-                    };
-
-                    if let Some(ctrl) = new_ctrl {
-                        self.ui_state
-                            .add_log(format!("Player {} is now {:?}", pid, ctrl));
-                    }
-                }
-            }
-        }
+        if let crossterm::event::KeyCode::Char('p') = key.code {
+            if let Some(world) = &self.world {
+                if let Some(match_state) = world.get_resource::<engine::resources::MatchState>() {
+                    if let Some(players) = world.get_resource::<engine::resources::Players>() {
+                        if let Some(active_player) = players.0.get(match_state.active_player_index.0) {
+                            let pid = active_player.id.0;
+                            let new_ctrl = if let Some(ctrl) = self.ui_state.player_controls.get_mut(&pid) {
+                                *ctrl = match *ctrl {
+                                    PlayerControlType::Human => PlayerControlType::Ai,
+                                    PlayerControlType::Ai => PlayerControlType::Human,
+                                };
+                                Some(*ctrl)
+                            } else {
+                                None
+                            };
+
+                            if let Some(ctrl) = new_ctrl {
+                                self.ui_state.add_log(format!("Player {} is now {:?}", pid, ctrl));
+                            }
+                        }
+                    }
+                }
+            }
+        }
PATCH
patch cli/src/app.rs < /tmp/cli_app.patch
