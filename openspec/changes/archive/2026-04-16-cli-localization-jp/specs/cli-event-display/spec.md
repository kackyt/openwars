## MODIFIED Requirements

### Requirement: 戦闘通知の表示 (Battle Notification Display)
MUST: システムは、攻撃の解決後に専用の UI ポップアップを使用して、戦闘結果（被ダメージ、与ダメージ）を日本語で明確に表示しなければならない。

#### Scenario: 攻撃と反撃の結果表示 (Unit attacks and receives counter-attack)
- **WHEN** ユニットが敵ユニットを攻撃し、戦闘が解決された場合
- **THEN** システムは、攻撃側が与えたダメージと、反撃によって受けたダメージを示す通知ポップアップを日本語で描画しなければならない (SHALL)。
