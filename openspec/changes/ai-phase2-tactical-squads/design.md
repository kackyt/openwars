## Context

Phase 1.5 まで（現在）の AI は、`engine/src/ai/engine.rs` の `decide_ai_action()` が中核であり、**各ユニットが独立して最善手を選ぶ貪欲法（Greedy）**で動作している。1ターンの全ユニット行動が逐次的に確定し、他のユニットの動きを前提とした協調行動は不可能である。

`TransportMission` / `TransportMissionManager` は既に「複数ユニットの協調」を実現している唯一の仕組みだが、輸送（Helicopter + Infantry）に特化した固定フェーズの設計であり、戦闘・占領・防衛への汎化はできていない。

`evaluate_board()` （`eval.rs`）は「HP割合 × 生産コスト」と「拠点固定値」の合算であり、位置的優位・領域支配・任務進捗を反映していない。

## Goals / Non-Goals

**Goals:**
- 「集中か分散か」という複数部隊の協調判断をビーム探索で自動解決する
- 部隊（Squad）概念を導入し、Attack / Capture / Defense / Transport の各ミッションを統一的に管理する
- ターン数ベースの距離計算（`TurnDistance`）を導入し、全距離評価の精度を向上させる
- `evaluate_board()` に位置補正・任務補正・領域支配を追加し、評価関数を相手の応答を考慮できるレベルに引き上げる
- 既存の `TransportMission` を破壊せず、Squad システムへ統合する

**Non-Goals:**
- 相手ターンのシミュレーション（ミニマックス）は対象外（Phase 3 の M-UCT で実現）
- Squad 内の行動順序最適化（入れ替え攻撃等の B 型ビーム）は Phase 2 では実装しない
- 相性補正（敵兵種構成に基づくユニット価値の動的補正）は Phase 3 で実装
- GUI への表示変更は対象外

## Decisions

### Decision 1: ビーム探索の単位を「Squad 目標割り当てプラン」とする

**選択**: 全 Squad の目標割り当て組み合わせ（`SquadAssignmentPlan`）をビームの 1 状態とする。

**代替案との比較:**
- 案X: ユニット単位でインクリメンタルにビームを展開する → 局所評価で有望な多部隊連携プランが早期に枝刈りされ、「集中か分散か」問題を解けない
- 案Y: 全 Squad の完全な組み合わせを全列挙してから評価 → K^N の組み合わせ爆発（Squad 4つ × 目標 5候補 = 625通り）
- **採用案**: インクリメンタル展開 + ロールアウト評価（残り Squad は貪欲法で補完）で暫定スコアを計算し、ビーム幅 N 以下に絞る。M-UCT の「ヘビープレイアウト」と同じ思想であり、Phase 3 への自然な橋渡しになる。

### Decision 2: 部隊（Squad）はミッション起動で生成する（ミッション駆動型）

**選択**: 「ユニットを集めて部隊を作る」のではなく「達成すべきミッションを認識し、それに必要なユニットを割り当てる」。

**理由:**
- ミッション駆動型は「何を達成すべきか」が先にあるため、ルールの設計が自然かつ検証しやすい
- 「何が足りないか」（目標に対するユニット不足）をビーム探索の評価で自動発見できる
- 既存の `TransportMission`（ミッションが先にあってユニットを割り当てる設計）と思想が一致する

### Decision 3: 部隊の寿命は複数ターンだが毎ターン再編成を検討する

**選択**: Squad は `TransportMission` と同様にターンをまたいで持続する。ただし毎ターン `SquadManager.update()` で以下を実施する:
1. ミッション完了チェック（達成 → 解散）
2. ミッション破綻チェック（メンバー消滅 → 解散 → SoloFallback）
3. 再編成判断（戦況変化 → ターゲット更新 or メンバー入れ替え）

**理由:** 毎ターン完全に再編成すると長期的連携が失われ、輸送ミッションのような複数ターンにわたる協調が不可能になる。

### Decision 4: TurnDistance の精度は「拠点座標のみ BFS」でフェーズ 2 では十分

**選択**: 全マス走査による影響マップではなく、「自軍・敵軍の拠点座標および主要ユニット位置のみ」で `TurnDistance` を計算する。

**代替案との比較:**
- 全マス走査（W×H × ユニット数 の BFS）: 20×20 マップで O(4000 × BFS) → 重すぎる
- Manhattan 距離 / max_movement の切り上げ: 高速だが地形・移動タイプを完全無視
- **採用案**: 拠点・首都・工場の座標のみで BFS を実行（拠点数 × ユニット数 程度）。Phase 3 で全マス走査に精緻化する。

### Decision 5: 領域支配スコアも「拠点座標近似」で実装する

`territorial_control_score()` は全タイルでなく、**各拠点座標における `min(own_reach, enemy_reach)` の比較**で支配者を判定し、自軍支配拠点数 - 敵軍支配拠点数 を評価する。

拠点周辺の「孤立度」は CONSOLIDATION_RADIUS_TURNS（2ターン）以内の自軍支配拠点の割合で算出する。

### Decision 6: 既存 TransportMission は Squad に統合するが互換性を保つ

`TransportMission` は `Squad::Transport(TransportPhase)` として内包し、`TransportMissionManager` は廃止して `SquadManager` に統合する。移行期間中は `TransportMission` 型エイリアスを提供してコンパイルエラーを段階的に解消する。

### Decision 7: SoloFallback ユニットは既存の貪欲法にフォールバックし、復帰インセンティブをスコアに追加する

- **SoloFallback 移行条件**: HP < 60 または主武装弾薬 = 0
- **復帰条件**: HP ≥ 70 かつ弾薬補充済み かつ 最寄り Squad に空きあり
- **復帰インセンティブ**: 貪欲法の評価スコアに「最寄り受入可能 Squad への接近ボーナス」を加算（既存の `is_unit_stranded` ロジックを拡張）

## Risks / Trade-offs

| リスク | 影響 | 軽減策 |
|--------|------|--------|
| ビーム探索の計算量が想定以上に大きい | ターン実行が遅延する | ビーム幅 N のデフォルト値を 3〜5 から始め、MCP ツールで実測チューニング |
| ロールアウト評価の精度が低くビームの絞り込みが誤る | 最適でないプランが実行される | ビーム幅を広くとることで緩和。評価関数改善で自然に解決 |
| Squad 形成ルールが粗く、明後日の方向に Squad が編成される | 行動品質が Phase 1 より低下する | まず AttackMission と CaptureMission の2種のみ実装し段階的に追加 |
| TransportMission → Squad 統合でリグレッションが発生する | 既存の輸送協調行動が壊れる | 既存テストをすべてグリーンに保ちながら段階的に移行 |
| TurnDistance の BFS がターン開始時に重い | レスポンス低下 | 結果をターン内でキャッシュし再計算しない |

## Migration Plan

1. `turn_distance.rs` を新設し、既存コードには影響を与えずに TurnDistance を提供
2. `eval.rs` の `evaluate_board` に動的ユニット価値・領域支配スコアを追加（既存関数シグネチャは維持）
3. `strategy.rs` の `enemy_threatens_property` を TurnDistance ベースに修正
4. `cluster.rs` を新設して敵クラスター検出を実装（既存 engine.rs から独立）
5. `squad.rs` を新設し、まず TransportSquad のみで SquadManager を実装（TransportMission を並行稼働）
6. `beam_search.rs` を新設し、SquadAssignmentPlan のビーム探索を実装
7. `engine.rs` の `execute_ai_turn` に `plan_full_turn` を組み込む（Feature Flag 的に切り替え可能にする）
8. TransportMission を TransportSquad へ完全移行し、`missions.rs` / `planner.rs` を廃止

ロールバック: Feature Flag でフォールバックを Phase 1.5 の `decide_ai_action` に切り替え可能にする

## Open Questions

- ビーム幅 N の初期値をいくつにするか？（3 が妥当そうだが実測が必要）
- `AttackMission` のターゲットが「クラスターの重心」か「クラスター内の最高価値ユニット」か
- Squad の最大メンバー数の上限をいくつに設定するか（2〜4 の範囲が妥当か）
- SquadPlanner のルールで「どんな場合に AttackMission を立てないか」の棄却条件
