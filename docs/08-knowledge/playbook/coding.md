---
title: "PLAYBOOK - Coding"
category: "coding"
version: "1.0.0"
status: "approved"
created: "2026-08-15"
updated: "2026-08-15"
owner: "@t_kak"
ace_entry_count: 4
tags: [ace, playbook, coding]
references:
  - docs/08-knowledge/PLAYBOOK.md
---

# ACE Playbook — Coding

> **Parent**: [PLAYBOOK.md](../PLAYBOOK.md)

## 概要

`Category: coding` （コーディングパターン、言語固有・型安全のベストプラクティス）に関する ACE 構造化知見エントリ一覧です。

## エントリ一覧

<!-- ここから下にエントリを追記してください。最新のエントリが末尾になるように追記します。 -->

<a id="ace-67-2"></a>

### ACE-67-2: 固定UIパネルによるマップ遮蔽を防ぐカメラパディング制御

| フィールド | 値 |
| ---------- | --- |
| Category   | coding |
| Origin     | PR #67 / Issue #66 |
| Date       | 2026-07-20 |
| Helpful    | 0 |
| Harmful    | 0 |
| Status     | active |

**Insight**: 画面端や四隅に固定表示されるUIパネル（ターン表示、ユニット情報パネル等）が存在するキャンバス/マップビューでは、単にマップ外周にカメラをクランプするとマップ端のセルが固定UIに隠れて操作不能になる。カメラ座標クランプ処理にUIサイズに応じた可逆なパディング（スクロール余白）を設定することで、すべてのセルを安全域までスクロール可能にする。

**Context**: PR #67 において、Web版で画面右上の TurnIndicator や左下の UnitInfoPanel にマップ端のヘックスが被り、ドラッグ/スクロールを行ってもクリック・操作が困難になる現象が発生したため、`clampCameraPosition` に `CAMERA_PADDING_*` を組み込んで解剖・解決した。

**Action**: ビューポート上にオーバーレイ UI を配置するマップビューやゲームビューのカメラクランプ関数を実装する際は、単にビューポート枠 `[0, mapSize - windowSize]` にクランプするのではなく、固定UIの占有エリアに応じたカメラパディング（`CAMERA_PADDING_TOP` 等）を境界値に加算・減算してスクロール移動範囲を拡張する。

<a id="ace-71-3"></a>

### ACE-71-3: 行動種別は静的能力ではなく実際の実行文脈で判定する

| フィールド | 値 |
| ---------- | --- |
| Category   | coding |
| Origin     | PR #71 |
| Date       | 2026-07-25 |
| Helpful    | 0 |
| Harmful    | 0 |
| Status     | active |

**Insight**: 行動の性質が距離や位置によって変わる場合、マスターデータの静的属性だけで分類してはならない。射程 1〜3 の武器は距離 1 では直接攻撃、距離 2 以上では間接攻撃になるため、移動後制限は実際の攻撃距離から判定する必要がある。

**Context**: PR #71 では間接攻撃を `weapon.range_min > 1` で判定していたため、最小射程 1 の重戦車が移動後でも距離 2 以上へ攻撃できた。`is_indirect_attack(distance)` に置き換え、攻撃可否判定、攻撃対象列挙、武器選択のすべてで実距離に基づく分類へ統一した。

**Action**: 行動種別や制約がランタイム条件で変化する場合は、実際の距離・目的地・状態を受け取る純粋関数へ分類ロジックを抽出し、候補列挙・事前検証・実行・結果計算の全経路で共有する。境界値を挟むテスト（距離 1 / 2、移動前 / 移動後）と、複合射程ユニット・間接専用ユニットの双方を追加する。

<a id="ace-82-2"></a>

### ACE-82-2: Rust テストコードにおける nightly 機能 (let_chains) 回避による stable 互換性維持

| フィールド | 値 |
| ---------- | --- |
| Category   | coding |
| Origin     | PR #82 |
| Date       | 2026-07-27 |
| Helpful    | 0 |
| Harmful    | 0 |
| Status     | active |

**Insight**: テストコード内であっても `let_chains` (`if let ... && let ...`) のような Rust の nightly 限定の実験的構文を使用すると、標準の stable ツールチェーン環境でコンパイルエラーを引き起こす原因となる。

**Context**: PR #82 のテストコード実装時に `if let` の連鎖条件構文を使用したところ、コードレビューにおいて stable Rust で非互換となる nightly機能 (`let_chains`) の使用が指摘され、ネストした `if let` または `match` 構文へ修正された。

**Action**: テストコードを含めすべての Rust コードにおいて nightly 限定機能のうっかり使用を避け、stable ツールチェーンで通過する標準的な構文（ネストされた `if let` や `match` またはパターンマッチの平坦化）を使用する。

<a id="ace-94-1"></a>

### ACE-94-1: ユニット標的価値算出における搭載物および短期的経済阻害効果の複合評価

| フィールド | 値 |
| ---------- | --- |
| Category   | coding |
| Origin     | PR #94 |
| Date       | 2026-08-09 |
| Helpful    | 0 |
| Harmful    | 0 |
| Status     | active |

**Insight**: 敵ユニットの標的価値評価では、単体コストだけでなく、輸送中 cargo のコストや直近の占領・経済阻害効果を合算した「複合価値」で評価することで、全 AI バージョン共通で即効性のある適切な優先度判定が可能になる。

**Context**: PR #94 にて V4 生産 AI の標的評価を改修する際、ユニット本体のコスト評価に加えて cargo コストと直近の占領収入・阻害分を加算するロジックを実装し、それを V1〜V4 の共通戦術評価層（`engine/src/ai/eval.rs` / `squad.rs`）へ横展開した。

**Action**: 戦術 AI の目標判定・優先度評価では、`target_value = unit_cost + cargo_cost + immediate_capture_income` のように波及効果や内部状態の価値を複合評価する関数を実装し、全戦術層で共通利用する。
