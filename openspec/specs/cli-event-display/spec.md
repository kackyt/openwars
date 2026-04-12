# cli-event-display Specification

## Purpose
戦闘結果などの重要なゲーム内イベントを、CLI UI 上でユーザーに通知するための表示仕様を定義します。

## Requirements

### Requirement: 戦闘通知の表示 (Battle Notification Display)
システムは、攻撃の解決後に専用の UI ポップアップを使用して、戦闘結果を明確に表示しなければならない (MUST)。

#### Scenario: 攻撃と反撃の結果表示 (Unit attacks and receives counter-attack)
- **WHEN** ユニットが敵ユニットを攻撃し、戦闘が解決された場合
- **THEN** システムは、攻撃側が与えたダメージと、反撃によって受けたダメージを示す通知ポップアップを描画しなければならない (SHALL)。
