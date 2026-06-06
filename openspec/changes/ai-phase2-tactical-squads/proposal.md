## Why

Phase 1.5 までの貪欲法AIは「各ユニットが独立して最善手を選ぶ」設計であり、複数ユニットの協調行動（狭い通路での入れ替え攻撃、集中と分散の戦略判断、拠点を面で押さえていく動き）が原理的に実現できない。また、盤面評価は「HP × 生産コスト + 拠点固定値」に留まっており、位置的優劣・領域支配・任務進捗といった戦術的実態を反映していない。Phase 2 では「部隊（Squad）」という概念を導入し、ビーム探索によって複数部隊の目標割り当てを最適化することで、人間らしい戦術的連携行動を実現する。

## What Changes

- **新規**: `TurnDistance` モジュールの追加（ターン数ベースの距離計算）
  - 既存のマス距離による全距離評価を段階的に置き換える基盤
  - `calculate_reachable_tiles` を内部で活用し、「N ターン以内に到達可能か」を提供
- **新規**: 敵ユニットクラスター検出（`TurnDistance` ベース）
  - 2ターン以内に相互支援できるユニット群を1クラスターとして認識
  - `AttackCluster` 構造体と脅威レベル・価値スコアを定義
- **新規**: 部隊（Squad）システムの導入
  - `Squad` 構造体: メンバー、ミッション種別（AttackMission/CaptureMission/DefenseMission）、目標、フェーズ管理
  - `SquadManager`: 毎ターンの再編成・解散・SoloFallback 判定
  - `SquadPlanner`: GamePhase と ClusterMap からルールベースで部隊を形成
  - 既存の `TransportMission` を `TransportSquad` として統合・内包
- **新規**: ビーム探索エンジン（`BeamSearch`）
  - 探索空間: 全 Squad の目標割り当ての組み合わせ（`SquadAssignmentPlan`）
  - ロールアウト評価: 未割り当て Squad は貪欲法で補完して完成プランを暫定評価
  - 最優秀プランを選択し、各 Squad がミッションフェーズに従って実行
- **改善**: `evaluate_board` の精緻化（静的盤面評価関数の強化）
  - ユニット価値に「位置補正」「任務補正」「弾薬状態補正」を追加
  - 拠点評価に「孤立度補正」（周辺の自軍支配率）を追加
  - 「領域支配スコア」: ターン数距離ベースで各タイルの支配者を判定し、支配面積差を評価
- **改善**: `enemy_threatens_property` のターン数距離化
  - `strategy.rs` 内の脅威判定をマス距離からターン数距離に修正
- **新規**: SoloFallback メカニズム
  - HP < 60 または弾薬切れユニットをフォールバック状態に遷移
  - 貪欲法評価に「最寄り Squad への接近ボーナス」を追加して復帰を促進
  - 回復条件（HP ≥ 70 かつ補給済み）を満たしたら次の SquadPlanner サイクルで再割り当て

## Capabilities

### New Capabilities

- `turn-distance`: ターン数ベースの到達距離計算モジュール。地形コスト・移動タイプを考慮し「N ターン以内に到達できるか」「到達ターン数」を提供する
- `enemy-cluster-detection`: 敵ユニットのターン数距離クラスタリング。AttackCluster として脅威レベル・撃破価値・自軍からの到達ターン数を保持する
- `squad-system`: 部隊（Squad）の概念とライフサイクル管理。Attack/Capture/Defense/Transport の各ミッション種別とフェーズ遷移、SquadManager による再編成・解散・SoloFallback 制御
- `squad-beam-search`: 全 Squad の目標割り当て組み合わせをビーム探索で最適化するエンジン。「集中か分散か」の戦略判断を評価関数で自動解決する
- `board-evaluation-v2`: 精緻化された静的盤面評価関数。動的ユニット価値（位置・任務・弾薬補正）、拠点孤立度補正、領域支配スコアを統合する

### Modified Capabilities

- `tactical-ai-decision`: 意思決定のエントリーポイントが `decide_ai_action()` (1ユニット×1行動) から `plan_full_turn()` (全 Squad の完全ターンプラン) に変わる
- `multi-unit-coordination`: 既存の TransportMission ベースの協調から Squad システムへ統合。TransportMission は TransportSquad として内包される
- `production-strategy-analysis`: `enemy_threatens_property` の距離計算をターン数距離に修正し、航空・海上脅威の誤判定を解消する

## Impact

- `engine/src/ai/` 配下に以下のファイルを新設:
  - `turn_distance.rs`: TurnDistance 計算
  - `cluster.rs`: 敵クラスター検出
  - `squad.rs`: Squad / SquadManager / SquadPlanner
  - `beam_search.rs`: ビーム探索エンジン
  - `simulation.rs`: AiSimulationState（軽量盤面シミュレーション）
- `engine/src/ai/eval.rs`: `evaluate_board` の大幅拡張
- `engine/src/ai/strategy.rs`: `enemy_threatens_property` のターン数距離化
- `engine/src/ai/engine.rs`: `execute_ai_turn` から `plan_full_turn` へのリファクタリング
- `engine/src/ai/missions.rs` / `planner.rs`: TransportMission の Squad システムへの統合
- `cli/src/main.rs`: `execute_ai_turn` 呼び出しの変更に追随
