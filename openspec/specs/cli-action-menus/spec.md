## ADDED Requirements

### Requirement: Advanced Action UI
MUST: システムは、「搭載 (Load)」「降車 (Drop)」「補給 (Supply)」「合流 (Join)」などの高度なアクションに対して、ターゲットの選択フローとメニューを提供する。

#### Scenario: Unloading a passenger
- **WHEN** ユーザーが、歩兵を搭載した輸送ユニットの「降車 (Drop)」メニューを選択した場合
- **THEN** SHALL: システムは、隣接する有効なタイル上に歩兵を降ろすよう、ユーザーにタイルの選択フローを要求する。

#### Scenario: Supplying an adjacent unit
- **WHEN** ユーザーが、補給ユニットの「補給 (Supply)」メニューを選択した場合
- **THEN** SHALL: システムは、隣接する味方ユニットを対象に取り、UI上の追加プロンプトなしで即座に補給を実行する。対象が複数いる場合はリスト形式で提供する。
