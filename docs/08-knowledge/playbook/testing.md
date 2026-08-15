---
title: "PLAYBOOK - Testing"
category: "testing"
version: "1.0.0"
status: "approved"
created: "2026-08-15"
updated: "2026-08-15"
owner: "@t_kak"
ace_entry_count: 4
tags: [ace, playbook, testing]
references:
  - docs/08-knowledge/PLAYBOOK.md
---

# ACE Playbook — Testing

> **Parent**: [PLAYBOOK.md](../PLAYBOOK.md)

## 概要

`Category: testing` （テスト戦略、テストパターン、モック・シミュレーション設計）に関する ACE 構造化知見エントリ一覧です。

## エントリ一覧

<!-- ここから下にエントリを追記してください。最新のエントリが末尾になるように追記します。 -->

<a id="ace-52-1"></a>

### ACE-52-1: AI評価における主観メトリクスと客観メトリクスの分離

| フィールド | 値 |
| ---------- | --- |
| Category   | testing |
| Origin     | PR #52 |
| Date       | 2026-06-13 |
| Helpful    | 0 |
| Harmful    | 0 |
| Status     | active |

**Insight**: AIの評価において、主観的メトリクス（AI内部の計算式に依存）と客観的メトリクス（バージョン非依存）を分離することで、AIの強さを定量的かつ客観的に比較・検証可能にする。

**Context**: PR #52 でAIの評価関数を更新した際、新旧AIの性能を定量的に比較するために、AI内部のスコア内訳（主観）と、支配面積・ターン収入・拠点数といったバージョンに依存しないメトリクス（客観）を分離して出力する基盤を構築した。

**Action**: AIの性能向上を検証する基盤を設計する際は、AI自身が計算するスコアに頼るのではなく、支配面積や収入などバージョン非依存の客観的指標（Objective Metrics）を定義し、それで合否や優劣を判定する。

<a id="ace-82-1"></a>

### ACE-82-1: エンティティID追跡による複合行動パイプラインのシーケンス成立検証

| フィールド | 値 |
| ---------- | --- |
| Category   | testing |
| Origin     | PR #82 |
| Date       | 2026-07-27 |
| Helpful    | 0 |
| Harmful    | 0 |
| Status     | active |

**Insight**: 勝敗やスコアなどのマクロな結果指標ではなく、エンティティIDをキーにしたイベントトレース（Load → Unload → Attack/Capture）を収集し、特定の行動シーケンスが一定ターン内に成立したかを判定基準とすることで、複合AIパイプラインを決定的に検証できる。

**Context**: PR #82 にて島嶼侵攻AIの性能検証を行う際、ゲームの勝敗や最終スコアは相手の挙動や盤面全体に左右されるため合否判定から除外し、`InvasionTraceCollector` により「同一カーゴIDの搭載・敵島上陸・攻撃/被攻撃/占領開始」という一連のライフサイクルが達成されたか否かを判定基準（Criteria）として評価・テストした。

**Action**: 勝敗や全体スコアでは検証しづらい多段階の行動・協力パイプライン（例: ピストン輸送〜敵地展開〜拠点攻撃）を検証する際は、構成ユニットの Entity ID で関連イベントを相関させるトレース収集メカニズムを実装し、その状態遷移シーケンスの成否を明示的な評価基準として定義する。

<a id="ace-83-3"></a>

### ACE-83-3: ターン限定キャッシュにおける状態更新（mark/set）の呼び出しと実効性検証

| フィールド | 値 |
| ---------- | --- |
| Category   | testing |
| Origin     | PR #83 |
| Date       | 2026-08-01 |
| Helpful    | 0 |
| Harmful    | 0 |
| Status     | active |

**Insight**: ターン内限定のキャッシュ構造体（`TurnScopedCache` 等）を追加した際、キャッシュの参照（get/check）処理のみを実装し、状態更新（set/mark）メソッドの呼び出しを忘れると、キャッシュが無効化されて重い計算が繰り返し実行される。

**Context**: PR #83 の初期コードレビューにて、ターン内キャッシュ `AiTurnStrategyCache` を参照する処理は追加されたものの、`mark_squads_planned` や `set_campaign_portfolio` の呼び出しが抜け落ちており、キャッシュの恩恵が得られないバグが指摘された。

**Action**: キャッシュ機構を導入する際は、更新メソッドの呼出確認を行うとともに、「同一ターン内の2回目以降の呼び出しで重い計算がスキップされるか」を検証する単体テストを必ず追加する。

<a id="ace-93-2"></a>

### ACE-93-2: vi.hoisted と vi.mock を用いた Web Worker / WASM ブリッジ層のモックテスト

| フィールド | 値 |
| ---------- | --- |
| Category   | testing |
| Origin     | PR #93 |
| Date       | 2026-08-02 |
| Helpful    | 0 |
| Harmful    | 0 |
| Status     | active |

**Insight**: WebAssembly や Web Worker を統合したフロントエンドのブリッジ層（`EngineWorker` 等）をテストする際、`vi.hoisted` とモッククラスで WASM エンジンの内部 API をシミュレートすることで、実際の WASM バイナリや Worker 環境に依存せず API 呼び出しとデータ変換のテストを高速かつ安定して実施できる。

**Context**: PR #93 において `engineWorker.ts` に補給 API を追加する際、Vitest の `vi.hoisted` を利用して WASM Engine インスタンスのモックメソッドと `WasmEngine` モッククラスを宣言し、`engineWorker.test.ts` にて WASM モジュールへのパラメータ伝達やレスポンス JSON のパース動作を完全に検証した。

**Action**: Worker や WASM モジュールと通信するブリッジ層のテストを構築する場合は、`vi.hoisted` でモック関数群を事前定義し、`vi.mock` を用いて疑似モジュールとして注入することで、ブラウザ特有のバイナリロードやインスタンス化の依存を切り離してブリッジロジックを分離テストする。
