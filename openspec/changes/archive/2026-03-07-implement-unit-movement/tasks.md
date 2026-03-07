# Tasks: update-air-fuel-consumption

- [x] 1. `src/domain/unit_roster.rs` の `UnitStats` に、飛行時燃料消費量フィールド (`daily_fuel_consumption`) を追加する。
- [x] 2. `src/domain/game_state.rs` の ターン（1日）経過処理 (`process_daily_updates`) で、航空ユニット（低空・高空）に対して、固定の`2`ではなく `unit.stats.daily_fuel_consumption` の値を消費するように修正する。
- [x] 3. テストのダミーデータ（`dummy_stats` 等）に `daily_fuel_consumption` を追加してビルドを通す。
- [x] 4. 固定消費だけでなく、可変消費（ヘリの2や航空機の5）が正しく適用されることを確認する単体テスト（`test_air_unit_fuel_and_crash` 等の拡張）を追加・修正する。
