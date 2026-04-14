## 1. エンジン層・マスターデータの改善 (engine/src)

- [x] 1.1 `engine/src/resources/master_data/unit.csv` に `日毎燃料消費量` 列を追加し値を設定
- [x] 1.2 `engine/src/resources/master_data.rs` で `daily_fuel` を CSV から読み込むように修正
- [x] 1.3 `engine/src/components/unit.rs` の `UnitStats` から表示用の武器名フィールドを削除

## 2. CLI の日本語化とリファクタリング (cli/src)

- [x] 2.1 `ActionType` enum を導入し、メニューアクションを型安全に扱う
- [x] 2.2 メニュー判定ロジック（`handle_action_menu_selection` 等）のマジックストリングを排除
- [x] 2.3 `cli/src/ui.rs` にて、`MasterDataRegistry` を使用して武器名を解決・表示する
- [x] 2.4 Infoパネル、ログ、ポップアップなどの全メッセージを日本語化

## 3. 検証・品質確保

- [ ] 3.1 `cargo test` を実行し、既存のテスト（移動、戦闘、合流等）が壊れていないことを確認
- [ ] 3.2 `cargo clippy` を実行し、警告が出ないことを確認
- [ ] 3.3 手動確認: 日本語（全角）による描画崩れがないか、ターミナル幅を狭めて確認
- [ ] 3.4 手動確認: 空中・海上ユニットの燃料消費がマスターデータ通りに行われることを確認
