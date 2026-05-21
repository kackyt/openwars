# objective-management Specification

## Purpose

複数ターンにわたる攻略計画を可能にするため、拠点の戦略的重要度を定量評価し、AIの意思決定を自動化して戦略の一貫性を担保します。

## Requirements
### Requirement: Objective Management
MUST: AIは盤面上の拠点を戦略目標として評価し、攻略優先度を決定しなければならない。

#### Scenario: Priority Calculation
- **WHEN** AIプランナーが戦略目標を評価するとき
- **THEN** 中立拠点や敵拠点の種類（首都、工場、都市等）と前線からの距離に基づいてスコアを算出し、スコアが最も高い目標に対して行動を計画する。

