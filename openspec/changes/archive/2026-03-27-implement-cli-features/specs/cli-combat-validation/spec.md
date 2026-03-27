## ADDED Requirements

### Requirement: Combat Validation
MUST: システムは、CLIから攻撃コマンドを発行する前に、射程、陣営、被間接移動の制約などを検証する。

#### Scenario: Attacking out of range
- **WHEN** ユーザーが武器の射程外の攻撃対象を選択した場合
- **THEN** SHALL: システムは、攻撃を中止して不正なターゲット警告を提供する。
