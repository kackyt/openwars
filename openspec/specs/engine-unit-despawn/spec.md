# engine-unit-despawn Specification

## Purpose
HP が 0 になったユニットを安全にゲームワールドから削除し、関連するイベントを発火させるためのエンジン側処理の仕様を定義します。

## Requirements

### Requirement: ユニットのデスポーン処理 (Unit Despawning)
エンジンは、HP が 0 に到達したエンティティを ECS ワールドから完全に削除し、`UnitDestroyedEvent` をパブリッシュしなければならない (MUST)。

#### Scenario: ユニット死亡によるエンティティ削除 (Unit death removes entity)
- **WHEN** 戦闘や特殊な条件下でのダメージ蓄積により、ユニットの HP が 0 以下に減少した場合
- **THEN** エンジンシステムは、ワールド・アーキテクチャから当該ユニット・エンティティを即座に削除（デスポーン）し、破壊イベントをパブリッシュしなければならない (SHALL)。
