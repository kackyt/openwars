## ADDED Requirements

### Requirement: Phase Unification
MUST: システムは、UXフローを改善するために、生産フェーズと移動/攻撃フェーズを単一のメインフェーズに統合する。

#### Scenario: Turn starts in Main Phase
- **WHEN** プレイヤーが自分のターンを開始した場合
- **THEN** SHALL: システムは、移動と生産の両方を自由に実行可能な `Phase::Main` へただちに遷移する。
