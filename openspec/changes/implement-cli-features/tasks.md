## 1. UI State Extended Handling

- [x] 1.1 `InGameState` に `CargoSelection`, `DropTargetSelection` などを追加する
- [x] 1.2 イベントフェーズなどの遷移通知用オーバーレイの状態追加

## 2. Production Menu Update

- [x] 2.1 Factory/Airport/Portの地形を判定して生成可能ユニットリストを動的生成する
- [x] 2.2 首都から3マス以内の距離判定を行い、4マス以上の場合は「遠すぎて生産できない」旨のメッセージをUI表示する
- [x] 2.3 生産メニューのUI描画処理の修正

## 3. Unit Inspector

- [x] 3.1 選択中またはカーソル下のユニット情報を取得するヘルパーの追加
- [x] 3.2 ユニット詳細（HP、弾薬、燃料等）を描画するウィンドウ UI を実装する

## 4. Advanced Action Menus

- [x] 4.1 搭載(Load) 対象の選択フローの実装
- [x] 4.2 降車(Drop) 対象タイル選択フローの実装
- [x] 4.3 補給(Supply) と合流(Join) の対象特定とコマンド送信の実装

## 5. Event Notification Display

- [x] 5.1 戦闘イベント (`UnitAttackedEvent` 等) を購読し、結果を表示する仕組みの追加
- [x] 5.2 通知ポップアップのUIコンポーネント作成

## 6. Movement Range Visualization

- [x] 6.1 コストベースの経路探索アルゴリズム（ダイクストラ法）をUIのヘルパーに実装する、またはEngineの探索機能を利用する
- [x] 6.2 ユニット選択時に到達可能なタイル一覧を計算し `UiState` にキャッシュする
- [x] 6.3 キャッシュされた座標の背景色を変更してマップ描画を行う

## 7. 【追加】Phase Unification & Combat Rules Strictness

- [x] 7.1 `Phase::Production` と `Phase::MovementAndAttack` を `Phase::Main` に統合する
- [x] 7.2 CLIの攻撃ターゲット選択処理(`app.rs`)で射程、陣営、移動履歴の厳密なバリデーションを行う
- [x] 7.3 `DamageChart` のマッピングロジックを修正し、正しく武器データを引き当てられるようにする
- [x] 7.4 Engine側に `remove_destroyed_units_system` を追加し、HP0のユニットをデスポーンさせる
- [x] 7.5 `UnitAttackedEvent` に戦闘前後の正確なHPを含め、CLIで「HP 10 -> 5」のように結果を切り上げ（1〜10）表示する
