## 1. Island Detection Base

- [x] 1.1 `engine/src/ai/islands.rs` を作成し、フラッドフィルによる島（Island）の解析機能を実装する。
- [ ] 1.2 `IslandMap` の計算を適切なタイミング（例: マップ初期化時）で実行し、リソースとして保持する。

## 2. Transport Missions Base

- [ ] 2.1 `engine/src/ai/missions.rs` を作成し、`TransportMission` 構造体と4つのフェーズ（`Pickup`, `Transit`, `Drop`, `Return`）を定義する。
- [ ] 2.2 `engine/src/ai/planner.rs` を作成し、暫定的に輸送機に対してミッションを付与するテストロジックを追加する。
- [ ] 2.3 `engine/src/ai/engine.rs` の `decide_ai_action` を修正し、ミッションを持つユニットが貪欲法よりも優先して行動を決定するフローを追加する。

## 3. System Registration & Verification

- [ ] 3.1 作成したモジュール（`islands`, `missions`, `planner`）を `engine/src/ai/mod.rs` に登録する。
- [ ] 3.2 既存のテストマップ（`map_3`）を用いて、ミッションに基づくユニットの挙動（歩兵を拾って別の島に移動し、降ろす）が機能するか検証する。
