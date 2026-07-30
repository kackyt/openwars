# Issue #58 Local Child-Issue Drafts

## [AI V3] 上陸した占領要員を敵島の拠点占領へ確実に引き渡す

- Parent: Issue #58
- Cause: 上陸後に占領可能ユニットが Capture 行動へ移行しない
- Affected map: `map_3`
- Affected orders: 先攻・後攻
- Affected seeds: `58001`, `58002`, `58003`, `58004`

### Failing baseline metrics

- 敵初期島の占領完了: 0件 / 8試合
- 初期島外の獲得拠点: 0件 / 8試合
- map_3 先攻平均: ZOC 55.5 < 80.75、収入 23,250 < 26,500、拠点 14.25 < 16.75
- map_3 後攻平均: ZOC 68.75 < 106.75、収入 24,000 < 36,750、拠点 15.0 < 24.25

### Trace evidence

- 全8試合で占領可能ユニット1-3体が敵島へ上陸済み。
- 占領開始を確認できたのは2試合のみ。
  - seed 58001 / 先攻: first_drop T8、first_capture_start T13、完了なし
  - seed 58003 / 後攻: first_drop T12、first_capture_start T14、完了なし
- 残る6試合は占領開始0件。
- 占領要員投資は98,000-124,000G、侵攻用輸送（輸送ヘリ・輸送船）の容量は2-12であり、生産不足・輸送能力不足ではない。

### Scope

降車済みの占領可能ユニットを、対象島の未占領拠点へ向かう Capture 部隊へ決定的に引き渡す。占領中または占領地点へ移動中のユニットを前線攻撃へ再割当しない。対象島・拠点を失う局所候補を抑制する。

### Implicated engine files

- `engine/src/ai/squad.rs`
- `engine/src/ai/objectives.rs`
- `engine/src/ai/beam_search.rs`

### Acceptance

- [ ] 敵島へ降車済みの占領可能ユニットと到達可能な敵拠点を用意し、修正前は占領へ移行しない決定的テストを追加する。
- [ ] 降車要員が Capture 部隊へ引き渡され、占領地点への移動または占領コマンドを選ぶ。
- [ ] 占領中・占領地点へ移動中の要員を Attack 部隊へ再割当しない。
- [ ] map_3 の座標、手番、seed を production AI ロジックへ埋め込まない。
- [ ] 固定 seed result で敵島の占領完了数を baseline の0件から1件以上へ改善する。
- [ ] map_3 後攻の固定 seed 試合で初期島外拠点を1件以上獲得する。
- [ ] map_2 の先攻・後攻 PASS を維持し、map_1 の baseline 指標を悪化させない。
- [ ] `cargo test`、clippy、fmt が成功する。

---

## [AI V3] 上陸占領要員を護衛して占領開始まで生存させる

- Parent: Issue #58
- Cause: 上陸直後の交戦で占領要員が失われ、拠点占領へ到達できない
- Affected map: `map_3`
- Affected orders: 先攻・後攻
- Affected seeds: `58001`, `58002`, `58003`, `58004`

### Failing baseline metrics

- 上陸占領要員の生存率:
  - seed 58001 / 先攻 0.0、後攻 0.0
  - seed 58002 / 先攻 0.0、後攻 0.0
  - seed 58003 / 先攻 0.0、後攻 0.0
  - seed 58004 / 先攻 0.333、後攻 0.0
- 敵島の占領完了: 0件 / 8試合
- map_3 勝率: 0/8（0.0%）

### Trace evidence

- 先攻は全 seed で first_drop T8、first_combat T8。上陸と同じターンに交戦が発生する。
- 後攻は first_drop T12, T12, T12, T18、first_combat T13, T13, T14, T19。上陸の1-2ターン以内に交戦が発生する。
- 上陸占領要員は1-3体存在するが、8試合中7試合で最終盤面まで1体も生存しない。

### Scope

対象島へ上陸済みの戦闘ユニットを、占領要員へ到達可能な敵の排除または占領地点周辺の遮蔽へ割り当てる。占領要員自身を、より安全な戦闘ユニットが存在する状況で前線攻撃へ使用しない。

### Implicated engine files

- `engine/src/ai/squad.rs`
- `engine/src/ai/objectives.rs`

### Acceptance

- [ ] 上陸占領要員、同島の自軍戦闘ユニット、占領要員を攻撃可能な敵ユニットを用意し、修正前は護衛されない決定的テストを追加する。
- [ ] 自軍戦闘ユニットが脅威の排除または遮蔽へ割り当てられる。
- [ ] 占領要員が占領地点へ移動中または占領中なら前線攻撃へ再割当しない。
- [ ] map_3 の座標、手番、seed に依存する護衛分岐を追加しない。
- [ ] 固定 seed result で生存率0.333超の試合数を baseline より増やす。
- [ ] 固定 seed result で敵島の占領完了数を baseline の0件から改善する。
- [ ] map_2 の先攻・後攻 PASS を維持し、map_1 の baseline 指標を悪化させない。
- [ ] `cargo test`、clippy、fmt が成功する。

---

## [AI V3] 海上侵攻需要を装甲車で誤充足せず侵攻用輸送を先行生産する

- Parent: Issue #58
- Cause: `max_cargo > 0` の装甲車が海上侵攻の輸送需要を満たした扱いになり、後攻の輸送ヘリ・輸送船生産が T5-T7 まで遅れる
- Affected map: `map_3`
- Affected order: 後攻
- Affected seeds: `58001`, `58002`, `58003`, `58004`

### Failing baseline metrics

- 後攻の侵攻用輸送生産: T5, T5, T5, T7（先攻は全 seed T1）
- 後攻 first_load: T7, T7, T7, T9（先攻は全 seed T3）
- 後攻 first_drop: T12, T12, T12, T18（先攻は全 seed T8）
- 後攻平均収入: 24,000 < V2 36,750
- 後攻平均拠点数: 15.0 < V2 24.25
- 後攻初期島外拠点獲得: 0件 / 4試合

### Trace evidence

- 先攻は全 seed で輸送ヘリを T1 に生産し、同じ輸送ヘリが T3 に first_load を実行する。
- 後攻は全 seed で T1 に装甲車（`max_cargo=1`）を生産するが、この装甲車は海を越えられない。
- 後攻の実際の侵攻用輸送は seed 58001-58003 で輸送ヘリ T5、seed 58004 で輸送船 T7。その2ターン後に first_load が発生する。
- 後攻でも占領要員投資は101,000-124,000Gあり、遅延原因はカーゴ不足ではなく海上輸送の生産優先度である。

### Scope

海上侵攻に由来する `light_transport_demand` / `heavy_transport_demand` は、対象島へ到達できる輸送ヘリ・輸送船だけで充足・減衰させる。装甲車の局地輸送能力は地上輸送需要にのみ反映し、既存の Load・Transit・Drop ルールは変更しない。

### Implicated engine files

- `engine/src/ai/production.rs`
- `engine/src/ai/strategy.rs`

### Acceptance

- [ ] 海で分断された侵攻目標、装甲車を生産可能な工場、輸送ヘリまたは輸送船を生産可能な施設を用意し、修正前は装甲車が侵攻輸送需要を誤充足する決定的テストを追加する。
- [ ] 海上侵攻需要があるとき、装甲車より侵攻用輸送を優先して生産する。
- [ ] 装甲車生産後も海上侵攻需要を減衰させない。
- [ ] V1 輸送 AI と Load・Transit・Drop のドメインルールを変更しない。
- [ ] map_3 の座標、後攻専用分岐、seed 固有分岐を追加しない。
- [ ] 固定 seed result で後攻の侵攻用輸送生産 T5-T7、first_load T7-T9、first_drop T12-T18 を短縮する。
- [ ] map_3 後攻の平均収入・拠点数・外部拠点獲得を baseline より改善する。
- [ ] map_2 の先攻・後攻 PASS を維持し、map_1 の baseline 指標を悪化させない。
- [ ] `cargo test`、clippy、fmt が成功する。
