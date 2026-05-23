## 1. TurnDistance モジュールの新設

- [ ] 1.1 `engine/src/ai/turn_distance.rs` を新設し、`TurnDistanceCache` 構造体を定義する（`HashMap<(GridPos, GridPos, MovementType, u32), u32>` をラップする）
- [ ] 1.2 `compute_turn_distance(map, occupied, from, to, movement_type, max_movement, registry) -> u32` を実装する（既存 `calculate_reachable_tiles` の BFS を内部で活用）
- [ ] 1.3 到達不可能な場合は `u32::MAX` を返すように実装する
- [ ] 1.4 同一ターン内のキャッシュ機構を実装する（ターン開始時にキャッシュをクリア）
- [ ] 1.5 `TurnDistance` の単体テストを作成する（山岳通過、海を越える航空ユニット、到達不可など）

## 2. 盤面評価関数の精緻化（board-evaluation-v2）

- [ ] 2.1 `engine/src/ai/eval.rs` に `dynamic_unit_value()` 関数を追加する（位置補正・弾薬補正のみ先行実装）
- [ ] 2.2 `dynamic_unit_value()` に任務補正（占領進行度ボーナス）を追加する
- [ ] 2.3 `territorial_control_score()` を実装する（各拠点座標での `TurnDistance` 比較で支配者を判定）
- [ ] 2.4 `property_consolidation_score()` を実装する（孤立度 = `CONSOLIDATION_RADIUS_TURNS` 圏内の自軍拠点割合）
- [ ] 2.5 `evaluate_board()` を更新して `dynamic_unit_value` + `property_consolidation_score` + `territorial_control_score` を統合する
- [ ] 2.6 評価関数の単体テストを作成する（孤立拠点と連続拠点の評価差、弾薬切れユニットの減価など）

## 3. strategy.rs の脅威判定をターン数距離化

- [ ] 3.1 `strategy.rs` の `enemy_threatens_property` を `TurnDistance` ベースに修正する（移動タイプ別のマス距離閾値分岐を廃止）
- [ ] 3.2 `THREAT_THRESHOLD_TURNS = 3` を定数として定義する
- [ ] 3.3 GamePhase 判定で使われている「敵との距離」を TurnDistance ベースに修正する（Contested フェーズ判定の 5 マス閾値を 2 ターン閾値に変更）
- [ ] 3.4 既存の `strategy.rs` テストを更新し、TurnDistance ベースの脅威判定で通るように修正する

## 4. 敵クラスター検出モジュールの新設

- [ ] 4.1 `engine/src/ai/cluster.rs` を新設し、`AttackCluster` 構造体を定義する（`members: Vec<EnemySnapshot>`, `threat_turns: u32`, `value: i32`）
- [ ] 4.2 `detect_clusters(world, player_id, turn_distance_cache) -> Vec<AttackCluster>` を実装する（2ユニット間 TurnDistance ≤ 2 を同一クラスターとする連結成分アルゴリズム）
- [ ] 4.3 `AttackCluster::threat_level()` を実装する（クラスター内最速ユニットから自軍拠点への最短 TurnDistance）
- [ ] 4.4 `AttackCluster::min_turns_to_engage(from, movement_type, max_movement)` を実装する
- [ ] 4.5 クラスター検出の単体テストを作成する（近接2体が同一クラスター、海越えは別クラスターなど）

## 5. Squad システムの新設

- [ ] 5.1 `engine/src/ai/squad.rs` を新設し、`Squad` 構造体・`SquadMission` 列挙体・`SquadTarget` 列挙体を定義する
- [ ] 5.2 `SquadMission` に Attack / Capture / Defense / Transport のフェーズ付き列挙体を実装する（Transport は既存 `TransportPhase` を流用）
- [ ] 5.3 `SquadManager` 構造体を実装する（`squads: Vec<Squad>` を保持するリソース）
- [ ] 5.4 `SquadManager::update()` を実装する（完了チェック・破綻チェック・ターゲット更新の 3 ステップ）
- [ ] 5.5 `UnitStatus::SoloFallback` / `ASSIGNED` / `WAITING_REASSIGNMENT` の状態管理を Component として追加する
- [ ] 5.6 SoloFallback 移行条件（HP < 60 または主武装弾薬 = 0）のチェックを `SquadManager::update()` に組み込む
- [ ] 5.7 SoloFallback 復帰条件（HP ≥ 70 かつ弾薬補充済み）のチェックを実装する

## 6. SquadPlanner のルールベース部隊形成

- [ ] 6.1 `SquadPlanner` 構造体を `squad.rs` に追加し、`form_squads(world, game_phase, cluster_map) -> Vec<Squad>` を実装する
- [ ] 6.2 GamePhase::Defense 時の DefenseMission 形成ルールを実装する（首都 `THREAT_THRESHOLD_TURNS` 圏内の敵クラスターに対して）
- [ ] 6.3 GamePhase::Expansion 時の CaptureMission 形成ルールを実装する（自軍の島の未占領拠点ごとに 1 ミッション）
- [ ] 6.4 GamePhase::Contested / Assault 時の AttackMission 形成ルールを実装する（脅威レベルが低いクラスターを優先）
- [ ] 6.5 ユニットの「ミッション適合スコア」計算を実装する（移動タイプ・HP・TurnDistance・既割当ペナルティ）
- [ ] 6.6 適合スコア順のユニット割り当てアルゴリズムを実装する（貪欲な割り当て）
- [ ] 6.7 SquadPlanner の単体テストを作成する（Expansion フェーズで歩兵 2 体が未占領拠点 2 つに割り当てられるなど）

## 7. TransportMission の Squad 統合

- [ ] 7.1 `TransportMission` を `Squad::Transport(TransportPhase)` として `squad.rs` に統合する
- [ ] 7.2 既存の `TransportMissionManager` リソースを `SquadManager` に統合し、`missions.rs` の TransportMissionManager 参照を置き換える
- [ ] 7.3 既存の TransportMission 関連テストがすべてグリーンのまま動作することを確認する
- [ ] 7.4 `planner.rs` の TransportMission 生成ロジックを SquadPlanner の TransportSquad 形成ルールとして移植する

## 8. AiSimulationState の新設

- [ ] 8.1 `engine/src/ai/simulation.rs` を新設し、`AiSimulationState` 構造体を定義する（軽量な盤面スナップショット: ユニット位置・HP・弾薬、拠点占領状態）
- [ ] 8.2 `AiSimulationState::from_world(world, player_id)` を実装する（World から必要な情報を抽出）
- [ ] 8.3 Squad 別の行動シミュレーション関数を実装する（AttackMission: ターゲットへの移動 + 攻撃、CaptureMission: 拠点への移動 + 占領）
- [ ] 8.4 `AiSimulationState` 上で `evaluate_board()` を呼び出せるように接続する

## 9. ビーム探索エンジンの新設

- [ ] 9.1 `engine/src/ai/beam_search.rs` を新設し、`SquadAssignmentPlan` 構造体と `BeamSearch` 構造体を定義する
- [ ] 9.2 `BEAM_WIDTH: usize = 5` を定数として定義する
- [ ] 9.3 インクリメンタル Squad 割り当て展開ループを実装する（1 Squad ずつターゲットを割り当て、ビーム幅で絞り込む）
- [ ] 9.4 ロールアウト評価を実装する（未割り当て Squad を貪欲法で補完して暫定スコアを算出）
- [ ] 9.5 全 Squad 割り当て完了後の最終 AiSimulationState シミュレーション評価を実装する
- [ ] 9.6 ビーム探索の単体テストを作成する（2 Squad × 3 目標で最適分散プランが選ばれるシナリオ）

## 10. engine.rs のリファクタリング

- [ ] 10.1 `execute_ai_turn()` を `plan_full_turn()` を中核とした構造にリファクタリングする
- [ ] 10.2 `plan_full_turn()` のフローを実装する: (1) SquadManager::update → (2) ClusterMap 生成 → (3) SquadPlanner::form_squads → (4) BeamSearch::plan → (5) Squad 実行 → (6) SoloFallback 実行
- [ ] 10.3 SoloFallback ユニットの貪欲法評価に「最寄り Squad への接近ボーナス」を追加する
- [ ] 10.4 `engine.rs` の全マス距離評価箇所を TurnDistance ベースに置き換える
- [ ] 10.5 Feature Flag 的な切り替え（`USE_SQUAD_PLANNER: bool`）を実装し、フォールバックとして旧 `decide_ai_action` を維持する

## 11. 結合テストと MCP 評価

- [ ] 11.1 `cargo test --all` がグリーンであることを確認する
- [ ] 11.2 `cargo clippy --all-targets --all-features -- -D warnings` が通ることを確認する
- [ ] 11.3 MCP ツール `simulate_ai_turn` を使って既存マップ（normal_map, island_map 等）で AI の行動が改善されていることを確認する
- [ ] 11.4 MCP ツール `evaluate_board` を使って盤面評価スコアの変化が直感に合っていることを検証する
- [ ] 11.5 Squad が形成され、集中・分散の判断が適切に行われていることをログから確認する
