# cli-action-menus Specification

## Purpose
ユニットの特殊アクション（搭載、降車、補給、合流など）に応じた動的なメニュー表示と、その選択フローの仕様を定義します。

## Requirements

### Requirement: 高度なアクションUI (Advanced Action UI)
システムは、「搭載 (Load)」「降車 (Drop)」「補給 (Supply)」「合流 (Join)」などの高度なアクションに対して、現在のコンテキストに基づいたターゲット選択フローとメニューを提供しなければならない (MUST)。

#### Scenario: ユニットの降車 (Unloading a passenger)
- **WHEN** ユーザーが、歩兵を搭載した輸送ユニットの「降車 (Drop)」メニューを選択した場合
- **THEN** システムは、隣接する有効なタイル上に歩兵を降ろすよう、タイル選択フローを開始しなければならない (SHALL)。

#### Scenario: 隣接ユニットへの補給 (Supplying an adjacent unit)
- **WHEN** ユーザーが、補給ユニットの「補給 (Supply)」メニューを選択した場合
- **THEN** システムは、`TargetSelection` 状態に遷移し、隣接する味方ユニットから補給対象を選択するフローを開始しなければならない (SHALL)。対象が単一の場合でも、ユーザーによる確定操作を必要とする。

### Requirement: 自軍拠点の修復メニュー
占領能力を持つユニットが、耐久値の減っている自軍拠点の上にいる場合、「修復」メニューを表示しなければならない (MUST)。
- 条件: `owner_id == ActivePlayer` かつ `capture_points < max`
- アクション名: 「修復」

#### Scenario: 拠点の修復メニュー表示
- **GIVEN** ダメージを受けた自軍拠点のマスに占領能力を持つユニットが移動した
- **WHEN** アクションメニューが開かれるとき
- **THEN** メニュー項目に「修復」が含まれていなければならない (SHALL)
