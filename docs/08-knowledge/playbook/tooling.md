---
title: "PLAYBOOK - Tooling"
category: "tooling"
version: "1.0.0"
status: "approved"
created: "2026-08-15"
updated: "2026-08-24"
owner: "@t_kak"
ace_entry_count: 4
tags: [ace, playbook, tooling]
references:
  - docs/08-knowledge/PLAYBOOK.md
---

# ACE Playbook — Tooling

> **Parent**: [PLAYBOOK.md](../PLAYBOOK.md)

## 概要

`Category: tooling` （ツール設定、ログ出力・可視化、開発環境・スキル）に関する ACE 構造化知見エントリ一覧です。

## エントリ一覧

<!-- ここから下にエントリを追記してください。最新のエントリが末尾になるように追記します。 -->

<a id="ace-64-3"></a>

### ACE-64-3: useCallback を用いた React ライフサイクル関数の安定化とリンター警告解消

| フィールド | 値 |
| ---------- | --- |
| Category   | tooling |
| Origin     | PR #64 |
| Date       | 2026-07-18 |
| Helpful    | 0 |
| Harmful    | 0 |
| Status     | active |

**Insight**: React の `useEffect` 内でストア操作やリフレッシュ用のコールバック関数を呼び出す際、依存配列にその関数を含めないと Biome 等のリンターで `useExhaustiveDependencies` 警告（エラー）が発生し、状態の同期バグを誘発する。

**Context**: `SaveLoadModal` の `useEffect` 内で、スロットの更新処理 `refreshSlots` を呼んでいたが、依存配列に含まれていなかったため Biome リンターによるビルドエラーが発生した。

**Action**: `useEffect` から参照される外部関数は、親コンポーネント側で `useCallback` を用いてメモ化（参照を安定化）した上で、効果の依存配列（dependency array）に明示的に含める。

<a id="ace-65-2"></a>

### ACE-65-2: AI思考評価ログの可視化とLLMエージェントによる自律敗因分析

| フィールド | 値 |
| ---------- | --- |
| Category   | tooling |
| Origin     | PR #65 |
| Date       | 2026-07-20 |
| Helpful    | 0 |
| Harmful    | 0 |
| Status     | active |

**Insight**: AIの内部評価スコアや行動選択肢を対戦ログのイベントとして出力し、専用のパーススクリプトおよび分析スキルを整備することで、勝敗結果だけでなく「なぜそのターンに不合理な判断をしたか」を LLM/AI エージェントが自律的にデバッグ・評価改善できる。

**Context**: PR #65 では `cli/src/logger.rs` と `scripts/analyze_battle_log.py` に加え、`.rulesync/skills/openwars-battle-analyzer` スキルを追加。AIが対戦ログから特定のターンにおける評価関数の失敗や期待値計算の誤りを自律的に検出・レポーティングできる基盤を確立した。

**Action**: AIゲームエンジンの思考ロジックを開発・検証する際は、結果（勝率）のみを記録するのではなく、AI内部の選択肢評価イベント（`AiEvaluationEvent` 等）を対戦ログに記録し、LLM/AIエージェントが直接ログを分析して改善策を立案できる分析ツールチェーンとスキルを併せて提供する。

<a id="ace-98-2"></a>

### ACE-98-2: 動的緊急ミッション・配備計画の対戦 JSONL トレース記録による AI 検証可能性の確保

| フィールド | 値 |
| ---------- | --- |
| Category   | tooling |
| Origin     | PR #98 |
| Date       | 2026-08-09 |
| Helpful    | 0 |
| Harmful    | 0 |
| Status     | active |

**Insight**: AIが手番中に動的に生成・消滅させる緊急介入計画や配備の整合性チェック（Deployment Audit, Emergency Plan, Factory Relief Missions）の履歴を、対戦ログ（JSONL）の構造化ストリームとして記録・エクスポートすることで、ブラックボックス化しがちな緊急思考プロセスの透明性と検証可能性を担保する。

**Context**: PR #98 にて生産口封鎖解除や配備追跡を導入した際、AI内部で正しく封鎖解除ミッションが発動しているかをバッチシミュレーションや Python テストスクリプト（`test_eval_matchup.py`）から確認できるよう、`factory_relief_history` や `emergency_plan_history` 等を対戦トレースログへ追加した。

**Action**: AIが内部状態（緊急計画、配備追跡、部隊評価など）に基づいて動的制御を行う場合は、その計画・監査情報を手番ごとの対戦トレース（JSONL）にエクスポートする出力インターフェースを整備し、評価ツールやテストから判定可能にする。

<a id="ace-101-3"></a>

### ACE-101-3: 大量マスターデータ検証におけるリソース名・不正値を含めたコンテキスト付きエラー設計

| フィールド | 値 |
| ---------- | --- |
| Category   | tooling |
| Origin     | PR #101 |
| Date       | 2026-08-24 |
| Helpful    | 0 |
| Harmful    | 0 |
| Status     | active |

**Insight**: 多数のシナリオやマップ（53マップ以上等）を一括ロード・パースする際、パース失敗時に単なるフォーマットエラーや定型文を返すのではなく、対象マップ名や不正入力文字列をエラーメッセージ内に埋め込むことで、データ不備の特定と修正作業を劇的に効率化できる。

**Context**: PR #101 において 53 マップ分の ROM シナリオ CSV を導入した際、生産制限パース処理（`parse_rom_production_limits`）やシナリオ登録処理で、エラー時にマップ名と不正値（`format!("{map_name}: invalid production limit '{value}': {error}")`）を含めるよう修正し、大量データ投入時のデバッグ性を向上させた（コミット `734eb37`）。

**Action**: CSV や外部定義から多数のマスターデータを読み込むパーサーでは、パースエラーのバリアントにリソース識別子（ファイル名、マップ名、行番号等）とパース対象の実文字列を含めた詳細なコンテキスト文字列を付与して返し、単体テストでそのエラーメッセージの含有を検証する。

