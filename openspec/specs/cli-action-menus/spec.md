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
- **THEN** システムは、隣接する味方ユニットを自動的に対象とし、即座に補給を実行する。対象が複数存在する場合は、ユーザーに選択リストを提供しなければならない (SHALL)。
