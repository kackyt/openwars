# cli-combat-validation Specification

## Purpose
CLI インターフェースにおいて、攻撃コマンドを発行する前のバリデーションロジック（射程、陣営、行動制限のチェック）の仕様を定義します。

## Requirements

### Requirement: 戦闘バリデーション (Combat Validation)
システムは、CLI から攻撃コマンドを発行する前に、武器の射程、ユニットの所属陣営、および間接攻撃ユニットの移動制約などを検証しなければならない (MUST)。

#### Scenario: 射程外への攻撃の拒否 (Attacking out of range)
- **WHEN** ユーザーが選択した武器の射程外にあるユニットを攻撃対象として選択した場合
- **THEN** システムは攻撃処理を中断し、ユーザーに不正なターゲットである旨の警告を表示しなければならない (SHALL)。
