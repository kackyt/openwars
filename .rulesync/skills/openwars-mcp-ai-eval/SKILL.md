---
name: openwars-mcp-ai-eval
description: >-
  OpenWarsの評価スクリプト `scripts/eval_matchup.py` を使用して、開発中のAIの動作・対戦結果・戦術的パフォーマンス（勝率、思考時間、ZOC支配面積、経済指標など）を自動評価し、レポートを作成するスキルです。AIロジックの評価やバージョン比較（例: V3 vs V1）を行う際に使用します。
---
# OpenWars AI Matchup Evaluator

`scripts/eval_matchup.py` を使用して、開発・アップデートしたAIロジックの対戦シミュレーションをバッチ実行し、定量的な戦術評価やパフォーマンスレポートを作成するワークフローです。
内部で `mcp-server` バイナリをサブプロセスとして起動して高速シミュレーションを行うため、評価実行前に必ず `mcp-server` の release ビルドを行います。

## 対象となるケース
- 新しいAIロジックや評価関数を実装した際のE2E評価
- 異なるAIバージョン間（例: V3 vs V1, V4 vs V3）の対戦シミュレーションと品質検証
- 特定マップ（例: `map_1`, `map_2`, `map_3`）におけるZOC支配面積・ターン収入・思考時間・勝率等の比較検証

## 評価ワークフロー

### 1. `mcp-server` クレートの release ビルド
`scripts/eval_matchup.py` は `target/release/mcp-server` (Windowsの場合は `mcp-server.exe`) を使用して対戦を行うため、評価前に必ず最新のコードを release ビルドしてください。

```bash
cargo build --release -p openwars-mcp-server
```

### 2. 評価条件の確認と対戦実行
評価対象のマップ、対戦カード（P1 vs P2のAIバージョン）、ターン数、対戦数などの条件を確認し、`scripts/eval_matchup.py` を実行します。AI評価を行う際は、プロンプトContextの節約と決定的なシミュレーション実行のため、必ず `--mode batch` オプションを指定します。

基本実行コマンド例:
```bash
python scripts/eval_matchup.py --mode batch --map map_3 --p1 V3 --p2 V1 --games 1 --max-turns 30 --output matchup_report.md
```

主要なパラメータ:
- `--mode`: 実行モード (`batch` を使用)
- `--map`: テスト対象マップ（カンマ区切りで複数指定可能: `map_1,map_2,map_3`）
- `--p1`, `--p2`: プレイヤー1/2のAIバージョン（例: `V1`, `V2`, `V3`, `V4`）
- `--games`: 1マッチアップあたりの対戦数
- `--max-turns`: 1ゲームの最大ターン数
- `--criteria`: 合否判定基準 (`objective`, `issue54`, `issue58` など)
- `--output`: 最終レポートの出力先パス (デフォルト: `matchup_report.md`)
- `--seed` / `--seeds`: 乱数シードの固定指定

### 3. 結果の確認と評価レポートの作成・分析
スクリプトの実行が完了すると、指定した `--output` (標準では `matchup_report.md`) に詳細な評価レポートが生成されます。
生成されたレポートや標準出力の内容を分析し、ユーザーへ以下のポイントを整理して提示します。

- **総合対戦結果**: 勝敗・引き分け数、勝率、平均勝利ターン数
- **パフォーマンス指標**:
  - 平均思考時間（1ターンあたり）
  - 客観メトリクス（ZOC支配面積、ターン収入、拠点数、NPV等の推移）
- **戦術・戦略の評価**:
  - 資源管理および生産バランス
  - 地形利用・ZOC支配の有効性
  - ユニットの移動・戦闘・拠点占領の連携（停滞や不必要な行動が発生していないか）
- **改善提案**: 分析結果に基づいたAIロジックの弱点や今後の修正方針

## 注意事項

- **事前ビルドの徹底**: `cargo build --release -p openwars-mcp-server` を行わずに評価を実行すると、古いビルドのバイナリが実行されたり、バイナリが存在しないためにエラーとなる可能性があります。
- **バッチモードの使用**: 生成AIによる評価実行時は、TUIモード (`--mode tui`) ではなく必ず `--mode batch` を使用してください。
