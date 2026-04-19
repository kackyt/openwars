## 1. Engine Logic & Refactoring

- [x] 1.1 `engine/src/systems/property.rs`: `victory_check_system` をリファクタリングし、`PropertyCapturedEvent`, `UnitDestroyedEvent`, `GamePhaseChangedEvent` の発生時のみ判定を走らせるように変更
- [x] 1.2 `engine/src/systems/property.rs`: `get_capturable_property` を修正し、自軍拠点（ダメージあり）を認識できるようにする
- [x] 1.3 `cli/src/app.rs`: `initialize_world` にて `victory_check_system` をシステムスケジュールに登録する
- [x] 1.4 `engine/src/systems/transport.rs`: `get_loadable_transports` を修正し、隣接マスからの搭載(Load)を許可
- [x] 1.5 `engine/src/systems/transport.rs`: `sync_cargo_health_system` を実装し、輸送ユニットのHPを積載ユニットに同期
- [x] 1.6 `engine/src/systems/turn_management.rs`: `next_phase_system` でユニットの状態リセット（HasMoved/ActionCompleted）とPendingMoveの除去を実装

## 2. CLI Action & Game Flow

- [x] 2.1 `cli/src/app.rs`: `ActionType::Repair` の追加と、自軍拠点上での表示・コマンド発行の実装
- [x] 2.2 `cli/src/main.rs`: `GameOverEvent` を購読し、結果ポップアップをセットする処理の追加
- [x] 2.3 `cli/src/app.rs`: ゲーム終了メッセージ表示中の入力で、タイトル（マップ選択）に戻るロジックを実装
- [x] 2.4 `engine/src/resources/master_data.rs`: `MasterDataRegistry::load` を修正し、`map_*.csv` をすべて動的に読み込むよう実装

## 3. UI Intelligence Overlay

- [x] 3.1 `cli/src/ui.rs`: 地形パネルへの防御ボーナス表示（`MasterDataRegistry` 連携）
- [x] 3.2 `cli/src/ui.rs`: 拠点詳細（所有者、占領HP）の表示
- [x] 3.3 `cli/src/ui.rs`: マップ描画の更新（行動済み:暗調、搭載中:*マーク）
- [x] 3.4 `cli/src/ui.rs`: ユニットパネルへの状態テキスト（燃料、弾薬、武器名）追加

## 4. Verification

- [x] 4.1 `cargo test`: 勝利判定のリファクタリングにより既存テストが壊れていないか、イベントを介して正しく機能するかを確認
- [x] 4.2 手動確認: 拠点の「修復」ができること、情報の表示が正しいこと、拠点占領or全滅でゲームが終了し正常に戻れること
- [x] 4.3 手動確認: マップ選択画面で `map_2` が表示され、ロードできること
- [x] 4.4 手動確認: 隣接マスからヘリ等に搭載できること

