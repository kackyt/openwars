## ADDED Requirements

### Requirement: Unit Despawning
MUST: エンジンは、HPが0に低下したエンティティをECSワールドからクリーンアップし、`UnitDestroyedEvent` を発火させる。

#### Scenario: Unit death removes entity
- **WHEN** ユニットへ蓄積されたダメージによってHPが0以下へと減少した場合
- **THEN** SHALL: システムは、ワールド・アーキテクチャからただちにそのユニット・エンティティをデスポーンさせ、破壊イベントをパブリッシュする。
