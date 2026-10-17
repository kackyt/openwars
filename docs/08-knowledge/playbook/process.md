---
title: "PLAYBOOK - Process"
category: "process"
version: "1.1.0"
status: "approved"
created: "2026-08-15"
updated: "2026-10-17"
owner: "@t_kak"
ace_entry_count: 1
tags: [ace, playbook, process]
references:
  - docs/08-knowledge/PLAYBOOK.md
---

# ACE Playbook — Process

> **Parent**: [PLAYBOOK.md](../PLAYBOOK.md)

## 概要

`Category: process` （開発プロセス、ワークフロー改善、タスク管理）に関する ACE 構造化知見エントリ一覧です。

## エントリ一覧

<!-- ここから下にエントリを追記してください。最新のエントリが末尾になるように追記します。 -->

<a id="ace-103-3"></a>

### ACE-103-3: 構造整理・状態整合化リファクタリングとゲーム挙動・バランス調整の厳格な分離

| フィールド | 値 |
| ---------- | --- |
| Category   | process |
| Origin     | PR #103 |
| Date       | 2026-10-17 |
| Helpful    | 0 |
| Harmful    | 0 |
| Status     | active |

**Insight**: 巨大モジュールの解体や状態遷移の負債返済を行う大規模リファクタリングにおいて、同時にゲーム挙動や兵種バランス等の戦術パラメータ調整を混入させると、動作変化や退行（回帰）の原因特定が極めて困難になる。完全な行動履歴（action history / trace）の一致（差分ゼロ）を完了基準とし、挙動変更は明確に別PRへ分離することで、ゼロリスクで安全に構造負債を返済できる。

**Context**: PR #103 の段階6bにおいて、当初計画に含まれていた「兵種バランス改善（段階6b④）」を試みたところ回帰検証で差分が生じたため、即座にスコープ外として除外・分離（`chore: stage6b-4(失敗)`）。28条件50Tの全 `action_history` 本文・勝敗・終了turnが完全一致する状態を維持したまま、extended.rsの全廃と状態整合化（D-5、占有モデル統一）のみを確実にマージした。

**Action**: 大規模な構造リファクタリングを実施する際は、「構造整理・責務分割・状態整合化」と「機能追加・パラメータ調整」を厳密に別PRに分離する。リファクタリングPRでは行動ログ・テストの完全一致（差分ゼロ）を完了条件とし、途中で挙動変化が生じた場合はパラメータ調整側を即座に切り離す。
