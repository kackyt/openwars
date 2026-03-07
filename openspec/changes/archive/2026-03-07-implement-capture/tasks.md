# Tasks: implement-capture

- [x] 1. `openspec` による仕様書（`property-capture`）の作成とレビューを完了する。
- [x] 2. `src/domain/game_state.rs` または `src/domain/map_grid.rs` に、拠点タイプ別の初期・最大耐久値（首都400、空港・港300、都市・工場200）および「中立」プロパティの概念を追加する。`MatchState` の `properties` を `HashMap<(usize, usize), PropertyState>` に変更する。
- [x] 3. `src/domain/unit_roster.rs` の `UnitStats` に、占領能力を持つかどうかのフラグ（`can_capture: bool`）を追加し、歩兵と戦闘工兵のみを true に設定する。
- [x] 4. `src/domain/game_state.rs` に、指定座標で占領または修復を行う `capture_or_repair_property` メソッドを追加する。
    - 中立または敵国拠点の場合：「占領」として HP×10 分耐久値を減らす。0以下になったら所有権移転と最大値リセット。行動完了化。
    - 自国拠点の場合：「修復」として HP×10 分耐久値を回復する（最大値を超えない）。行動完了化。
- [x] 5. 拠点の耐久値低下状態は、ユニットが離れたり破壊されたりしても維持されるように実装する（自動回復しない）。敵首都を占領した場合の即時勝利判定が発動することも確認する。
- [x] 6. 単体テストを追加する（中立都市の占領、首都の占領とHP計算、修復アクション、非歩兵ユニットの占領不可テスト、ユニット離脱後も耐久値が減ったまま維持されること等のテスト）。
