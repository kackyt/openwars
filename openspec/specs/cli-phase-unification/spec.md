# cli-phase-unification Specification

## Purpose
UX を向上させるため、従来の生産フェーズと移動・攻撃フェーズを単一のメインフェーズに統合した管理仕様を定義します。

## Requirements

### Requirement: フェーズの統合 (Phase Unification)
システムは、UX フローを改善するために、生産操作（拠点でのユニット生産）と移動・攻撃操作を単一のフェーズで実行可能に統合しなければならない (MUST)。

#### Scenario: メインフェーズへの自動遷移 (Turn starts in Main Phase)
- **WHEN** プレイヤーが自分のターンを開始し、初期化が完了した場合
- **THEN** システムは、移動と生産の両方が自由に実行可能な `Phase::Main` 状態（メインフェーズ）へ直ちに遷移しなければならない (SHALL)。
