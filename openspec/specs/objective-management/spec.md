# objective-management Specification

## Purpose

複数ターンにわたる攻略計画を可能にするため、拠点の戦略的重要度を定量評価し、AIの意思決定を自動化して戦略の一貫性を担保します。

## Requirements
### Requirement: Objective Management
MUST: AIは盤面上の拠点を戦略目標として評価し、攻略優先度を決定しなければならない。

#### Scenario: Priority Calculation
- **WHEN** AIプランナーが戦略目標を評価するとき
- **THEN** 中立拠点や敵拠点の種類（首都、工場、都市等）と前線からの距離に基づいてスコアを算出し、スコアが最も高い目標に対して行動を計画する。

### Requirement: Single Deterministic Island Invasion Objective
MUST: V3 は地上移動で到達できない敵所有拠点がある場合、同時に1つの敵島だけを侵攻目標として選択しなければならない。

#### Scenario: Multiple Enemy Islands
- **WHEN** 複数の敵島が侵攻候補になるとき
- **THEN** Island ID、拠点種別、座標の明示的な順序で1島を決定し、同一盤面では常に同じ目標を選択する。

#### Scenario: Beam Search Assignment
- **WHEN** 複数ターンの輸送侵攻が進行しているとき
- **THEN** 単ターンの Beam Search は輸送部隊の対象島を上書きせず、上陸後の部隊も対象島内の目標を維持する。

