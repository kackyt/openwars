## ADDED Requirements

### Requirement: Movement Range Visualization
MUST: システムは、ユニットが選択された際、エンティティの移動コストと燃料制限に基づいて到達可能なタイルを強調表示する。

#### Scenario: Unit selected highlights movable tiles
- **WHEN** ユーザーが移動させるユニットを選択した場合
- **THEN** SHALL: システムは、ユニットの移動可能タイルを計算し、マップ上の背景をハイライトする。
