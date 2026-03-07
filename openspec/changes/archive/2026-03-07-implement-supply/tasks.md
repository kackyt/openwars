# Tasks: implement-supply

- [x] 1. `src/domain/unit_roster.rs` の `UnitStats` に `can_supply: bool` フラグを追加し、`SupplyTruck` と `AircraftCarrier` の mock データで `true` にする。
- [x] 2. `src/domain/game_state.rs` に `supply_unit(supplier_index, target_index)` メソッドを追加する（補給輸送車・単体指定補給）。
    - 補給輸送車が隣接マス（距離 = 1）の味方ユニットを対象とする。
    - 燃料・ammo1・ammo2 を最大値まで回復。
    - `action_completed = true` にする（移動後でも実行可能）。
    - 資金消費なし。
- [x] 3. `src/domain/game_state.rs` に `supply_all_adjacent(supplier_index)` メソッドを追加する（全自動補給）。
    - 全隣接味方ユニットへ一括補給。同じロジックを対象ユニット全員に適用。
    - `action_completed = true` にする。
- [x] 4. `src/domain/game_state.rs` の `advance_turn` / `process_daily_updates` 内で、アクティブプレイヤーの拠点補給処理を行う。
    - アクティブプレイヤーが所有する拠点の上にいる味方ユニットを自動補給。
    - コスト：弾薬差 × 15G + 燃料差 × 5G を `player.funds` から差し引く。
    - 資金が足りない場合は補給しない（部分補給はなし）。
- [ ] 5. 単体テストを追加して `cargo test` を通す。
    - 輸送車単体指定補給：隣接ユニット補給、射程外エラー、行動完了フラグ
    - 輸送車全自動補給：複数ユニット一括補給
    - 拠点補給：資金消費確認、資金不足時の補給なし
    - 空母補給：搭載航空ユニット補給（空母搭載の仕様が未実装のためスキップまたはスタブ）
