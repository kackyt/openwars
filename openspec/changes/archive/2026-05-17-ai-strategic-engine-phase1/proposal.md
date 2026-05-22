## Why

現在の1ターン貪欲（Greedy）AIの原理的限界を突破し、離島マップ（例: `map_3`）において「輸送機が歩兵を迎えに行き、海を越えて目標の島へ降ろす」という複数ターンにまたがる協調行動（ミッション）の土台を構築するため。貪欲法では「意味のない待機」や「海を渡る」行動を評価できないため、ミッション管理レイヤーが不可欠。

## What Changes

本Phase 1では、以下に焦点を当てた**最小実装**を行う：
1. **島の概念（IslandMap）の導入**: マップ上の地形をフラッドフィルで解析し、「どこが連続した陸地（島）か」を認識する。
2. **輸送ミッション（TransportMission）の導入**: `MovingToPickup` → `InTransit` → `Dropping` → `Returning` のフェーズを持つミッション構造を定義する。
3. **ミッション優先実行モード**: 既存の貪欲AI（`decide_ai_action`）の前に、ミッションを持つユニットはそのミッションの次のフェーズを実行する専用ロジックを優先して通すようにする。

## Capabilities

### New Capabilities
- `island-detection`: フラッドフィルによるマップ上の島の自動検出
- `transport-missions`: 輸送機と搭載ユニットによる、フェーズベースの輸送ミッション実行機能

### Modified Capabilities

## Impact

- `engine/src/ai/islands.rs` および `engine/src/ai/missions.rs` などの新規戦略モジュール追加
- `engine/src/ai/engine.rs` のメインループ分岐の拡張
