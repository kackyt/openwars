# DEPLOYMENT.md - デプロイメント・運用ガイド

## 📖 構成

| ドキュメント | 内容 |
| --- | --- |
| [git-workflow.md](./deployment/git-workflow.md) | AI駆動Git Workflow全体 |
| [self-review.md](./deployment/self-review.md) | セルフレビュー詳細（PR作成前） |
| [knowledge-management.md](./deployment/knowledge-management.md) | ナレッジ体系化（マージ後・cleanup後） |

**本リポジトリ専用（移行設計）**: [NO_GITHUB_ACTIONS_MIGRATION_DESIGN.md](../../docs/NO_GITHUB_ACTIONS_MIGRATION_DESIGN.md)（GitHub Actions を使わない運用。ローカルでの品質チェックを優先）

## 🚀 クイックスタート（30秒で理解）

### AI駆動開発の基本フロー

```text
Issue → Branch → Implement → Test → Self-Review → PR → Review → Merge → Cleanup → ACE → Next Task
```

### よく使うコマンド

```bash
# 1. ブランチ作成
git checkout -b "feature/123-feature-name"

# 2. テスト・ビルド確認（ローカル）
cargo test --all-features --workspace
cargo build --release

# 3. セルフレビュー（AIツールに依頼）
「PROJECT.mdとCONVENTIONS.mdに基づいて、今回の変更をレビューしてください」

# 4. PR作成
gh pr create --base main --title "..." --body "..."
```

## 1. アプリケーションのビルドと配布

本プロジェクトはローカル実行型のCLI（将来的にGUI）アプリケーションです。Webサーバーへのデプロイ（Blue-Greenデプロイ等）は存在しません。

### リリースビルド

本番用のバイナリをビルドする場合は `--release` フラグを使用します。

```bash
cargo build --release
```

ビルドされた実行ファイルは `target/release/` 配下に生成されます。
- Windows: `openwars-cli.exe`
- macOS/Linux: `openwars-cli`

### 配布（GitHub Releases）

バージョンリリース時は、GitHubのReleases機能を利用して各OS向けのコンパイル済みバイナリを配布します。将来的には、GitHub Actions等を用いてクロスコンパイルとリリースアセットへの添付を自動化する予定です。

## 2. 運用とモニタリング

ローカルアプリケーションであるため、サーバー側での24時間監視（CPU/メモリ監視等）は行いません。
代わりに以下の方法で不具合の収集と対応を行います。

### ログ出力
- 実行時のエラーやパニック情報は標準エラー出力（stderr）またはログファイル（将来的に実装）に出力されます。
- ユーザーからのIssue報告に基づき、再現と修正を行います。

## 3. AI仕様駆動Git Workflow

### 概要

Git Flowベースで、**テスト・セルフレビュー（PR前）** と **ナレッジ体系化（マージ後）** を組み込んだワークフローです。

### 主要ステップ

1. **Issue作成** - 作業の起点
2. **ブランチ作成** - `feature/{issue-num}-{name}`
3. **実装・コミット** - AI駆動開発
4. **テスト・検証** - `cargo test` / `cargo clippy`
5. **セルフレビュー** - AIによるコードレビュー
6. **PR作成**
7. **マージ** - Squash推奨
8. **クリーンアップ** - ブランチ削除

## Changelog

### [1.0.0] - YYYY-MM-DD
- Rust製ローカルアプリケーション向けの運用・配布方針に更新
