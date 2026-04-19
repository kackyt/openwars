## ADDED Requirements

### Requirement: 自軍拠点の修復メニュー
占領能力を持つユニットが、耐久値の減っている自軍拠点の上にいる場合、「修復」メニューを表示しなければならない (MUST)。
- 条件: `owner_id == ActivePlayer` かつ `capture_points < max`
- アクション名: 「修復」

#### Scenario: 拠点の修復メニュー表示
- **GIVEN** ダメージを受けた自軍拠点のマスに占領能力を持つユニットが移動した
- **WHEN** アクションメニューが開かれるとき
- **THEN** メニュー項目に「修復」が含まれていなければならない (SHALL)
