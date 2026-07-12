# DOMAIN.md - ドメインモデル設計書

## 1. ドメイン概要

### ビジネスドメイン
ターン制戦略シミュレーションゲームにおける、ゲームルールの解決、戦術行動、状態管理。

### コアドメイン
ユニット間の戦闘（Combat）、移動と占領（Tactics）、生産と補給（Logistics）。

## 2. 境界づけられたコンテキスト
`engine` クレート内にすべてのドメインロジックを集約し、UI層（`cli`, `gui`）にはドメインロジックを漏出させないようにします。

## 3. エンティティ定義

### エンティティ一覧 (ECS Components)

| コンポーネント名 | 識別子/所属 | 役割 | 不変条件 |
| --- | --- | --- | --- |
| `Unit` | Entity (ID) | ユニット本体 | HPは0〜最大値の間 |
| `GridPosition` | `(x, y)` | マップ上のセル座標 | マップの範囲内であること |
| `GridTopology` | 列挙型 | マップの構造（Square / Hex） | マップ全体の距離計算や隣接判定のルールを決定する |
| `Faction` | `u32` 等 | 勢力情報 | - |
| `Health` | - | 耐久値・HP | 0になるとデスポーン |
| `ActionPoints`| - | 残り行動力 | ターン終了時に回復 |
| `AttackStat` / `DefenseStat` | - | 攻撃・防御能力 | 静的/動的パラメータ |

## 4. 値オブジェクト (Newtype パターン)

### 主要な値オブジェクト
Rustのタプル構造体を用いて型安全性を担保し、プリミティブ型の直接利用を避けます。

```rust
// 例:
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct UnitId(pub uuid::Uuid);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MapId(pub uuid::Uuid);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HitPoint {
    pub current: u32,
    pub max: u32,
}
```

## 5. 集約
ECSアーキテクチャでは、伝統的なDDDの「集約」の境界は、関連するコンポーネントの束（Entity）やシステム単位でのデータの整合性として表現されます。
Entityの参照はポインタ（参照）ではなく必ずID（例：`UnitId`）で行い、不要なライフサイクル問題を防ぎます。

## 6. ドメインイベント

UIに対して「何が起きたか」を通知するためにイベントを使用します。

### イベント一覧
| イベント名 | トリガー | 用途 |
| --- | --- | --- |
| `UnitMovedEvent` | ユニットの移動完了時 | UI側の移動アニメーションや状態更新 |
| `CombatResolvedEvent` | 戦闘終了時 | UI側のダメージ表示、結果通知 |

## 7. ビジネスルール
- **ルールの非漏出**: プレゼンテーション層（`cli`等）に「首都から3マス以内でしか生産できない」などのゲームルールを書かない。
- **入力の分離**: UIからは「コマンド」だけがEngineに送られ、Engineがルールの可否を判定・適用する。

## 8. リポジトリインターフェース
依存性の注入（DI）を活用するため、ジェネリクスまたは `dyn Trait` を使用します。基本的にはパフォーマンスを考慮しジェネリクス（静的ディスパッチ）を推奨します。

```rust
pub trait UnitRepository: Send + Sync {
    fn find_by_id(&self, id: &UnitId) -> Result<Option<Unit>, DomainError>;
    fn save(&self, unit: &Unit) -> Result<(), DomainError>;
}
```
