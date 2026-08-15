---
title: "PLAYBOOK - Performance"
category: "performance"
version: "1.0.0"
status: "approved"
created: "2026-08-15"
updated: "2026-08-15"
owner: "@t_kak"
ace_entry_count: 9
tags: [ace, playbook, performance]
references:
  - docs/08-knowledge/PLAYBOOK.md
---

# ACE Playbook — Performance

> **Parent**: [PLAYBOOK.md](../PLAYBOOK.md)

## 概要

`Category: performance` （パフォーマンス最適化、キャッシュ、計算量削減）に関する ACE 構造化知見エントリ一覧です。

## エントリ一覧

<!-- ここから下にエントリを追記してください。最新のエントリが末尾になるように追記します。 -->

<a id="ace-47-2"></a>

### ACE-47-2: ECSクエリのループ内呼び出しによるオーバーヘッド回避

| フィールド | 値 |
| ---------- | --- |
| Category   | performance |
| Origin     | PR #47 |
| Date       | 2026-06-08 |
| Helpful    | 1 |
| Harmful    | 0 |
| Status     | active |

**Insight**: `width * height` のような大規模なループ内で変化しないエンティティのコンポーネント（`UnitStats` など）を毎回 `world.get::<T>` で取得すると、ECSのクエリオフセットが累積してパフォーマンス低下を招く。

**Context**: AIの輸送待ち合わせタイルの計算時、マップ全域のループ（`width * height`）内で、対象となる輸送貨物（カーゴ）の `UnitStats` を毎ループ取得していたため無駄な処理が発生していた。

**Action**: ループ内で変化しない ECS コンポーネントへのアクセスは、必ずループの外で1度だけ取得し、変数にキャッシュして再利用する。

<a id="ace-47-3"></a>

### ACE-47-3: 探索アルゴリズム内の Vec::contains によるパフォーマンス低下の回避

| フィールド | 値 |
| ---------- | --- |
| Category   | performance |
| Origin     | PR #47 |
| Date       | 2026-06-08 |
| Helpful    | 0 |
| Harmful    | 0 |
| Status     | active |

**Insight**: ダイクストラ法などの経路探索アルゴリズムのループ内で `Vec::contains` を多用すると、線形探索（O(N)）となり重大なパフォーマンスのボトルネックになる。

**Context**: AIの距離計算（`calculate_turn_distance`）において、有効なターゲット一覧を `Vec` で保持し、探索ループ内で毎回 `effective_targets.contains(&position)` を呼び出していたため計算量が増大していた。

**Action**: 探索のルックアップ対象となるコレクションは `std::collections::HashSet` を使用し、判定を O(1) に高速化する。

<a id="ace-57-1"></a>

### ACE-57-1: O(N) オーバーヘッドを回避する Vec の pop() による要素取り出し

| フィールド | 値 |
| ---------- | --- |
| Category   | performance |
| Origin     | PR #57 |
| Date       | 2026-07-11 |
| Helpful    | 1 |
| Harmful    | 0 |
| Status     | active |

**Insight**: ベクター（Vec）から条件に合う要素を取り出す際、先頭から `remove(0)` などで取り出すと要素のシフトによる O(N) のコストが発生する。

**Context**: PR #57 のレビューにおいて、部隊割り当ての処理で戦闘ユニットをリストから割り当てる際、ベクターの先頭からの削除が O(N) のシフトを伴い非効率であることが指摘された。

**Action**: 要素を評価値などの「低い順」や「不要な順」に逆順ソート（reverse）し、末尾（最も必要な要素）から `pop()` で取り出すことで、O(1) のコストで効率的に要素を取り出して割り当てる。

<a id="ace-59-1"></a>

### ACE-59-1: Hot pathでの動的ディスパッチ（dyn Trait）回避によるパフォーマンス改善

| フィールド | 値           |
| ---------- | ------------ |
| Category   | performance  |
| Origin     | PR #59       |
| Date       | 2026-07-12   |
| Helpful    | 0            |
| Harmful    | 0            |
| Status     | active       |

**Insight**: 距離計算や隣接判定などの頻繁に呼ばれる hot path において、`dyn Trait` による動的ディスパッチを避けることでパフォーマンスのボトルネックを解消できる。

**Context**: PR #59 のヘックスグリッド対応にて、グリッド形状の抽象化に `dyn GridGeometry` を用いたが、レビューにてパフォーマンス懸念が指摘された。解決策として、`GridTopology` enum 内の `match self` によって各構造体に処理を委譲する静的ディスパッチ方式に変更された。

**Action**: ゲームループ内の hot path ではポリモーフィズムの実現に `dyn Trait`（動的ディスパッチ）を避け、列挙型（enum）による `match` 分岐（静的ディスパッチ）を採用する。

<a id="ace-63-1"></a>

### ACE-63-1: Zustandストア設計での不要な再レンダリング防止（Zustand Selectors）

| フィールド | 値 |
| ---------- | --- |
| Category   | performance |
| Origin     | PR #63 |
| Date       | 2026-07-16 |
| Helpful    | 0 |
| Harmful    | 0 |
| Status     | active |

**Insight**: React × Zustand 構成において、コンポーネントがストアの全体オブジェクトを分割代入するのではなく、個別の `state => state.xxx` をセレクタ経由で購読するように変更し、不要な再描画を抑制する。

**Context**: PR #63 にて、フロントエンドのパフォーマンス改善のため、ストア全体を分割代入して購読していたコンポーネントを、Zustand のセレクタ（例: `const currentTurn = useGameStore(state => state.currentTurn)`）を使用する形にリファクタリングした。これによって、関連のない状態変更時に無駄な再レンダリングが走るのを防いだ。

**Action**: Zustand を使用する場合は、`const { a, b } = useStore()` ではなく、`const a = useStore(s => s.a); const b = useStore(s => s.b)` のように個々のセレクタを使用して状態を購読することを徹底する。

<a id="ace-63-3"></a>

### ACE-63-3: RustのWASMバインディングにおけるヒープアロケーション削減（std::slice::from_ref）

| フィールド | 値 |
| ---------- | --- |
| Category   | performance |
| Origin     | PR #63 |
| Date       | 2026-07-16 |
| Helpful    | 0 |
| Harmful    | 0 |
| Status     | active |

**Insight**: Rust/WASMバインディング層で単一の要素をスライスとして渡す際、`vec![item]` などの動的アロケーションを伴う Vec を構築するのではなく、`std::slice::from_ref(&item)` を使用することで、ヒープアロケーションを回避し、高頻度で実行されるWASM-JS間ブリッジ処理のパフォーマンスを大幅に向上できる。

**Context**: PR #63 のコードレビューにおいて、WasmEngine バインディング層で単一の要素を一時的な Vec に包んで渡していた箇所が指摘された。これを `std::slice::from_ref` による参照のスライス化、および `MasterDataRegistry` などの巨大な読み取り専用データを参照渡し (`&MasterDataRegistry`) に変更することで、アロケーション回数を劇的に削減した。

**Action**: WASM バインディング層やゲームループの Hot path で単一要素をスライス（`&[T]`）として要求する関数に渡す場合は、`vec![x]` や配列アロケーションを避け、`std::slice::from_ref(&x)` を使用してスタック上の参照からスライスを作成する。また、読み取り専用のマスターデータ等は値コピーを避け、必ず参照で引き回す。

<a id="ace-92-2"></a>

### ACE-92-2: 段階的ループ処理から O(1) 直接算術計算への最適化と事前バリデーション

| フィールド | 値 |
| ---------- | --- |
| Category   | performance |
| Origin     | PR #92 |
| Date       | 2026-08-02 |
| Helpful    | 0 |
| Harmful    | 0 |
| Status     | active |

**Insight**: 資金や資源に応じた段階的アロケーション処理を、ループ（O(N)）から算術式による O(1) 計算へ最適化する際、選択フェーズで 0 コストや無効値を事前に検証・フィルタリングしておくことで、除算エラーを防ぎつつパフォーマンスとロジックの安全性を確保できる。

**Context**: PR #92 のコードレビューにて、空母搭載ユニットの部分修理における費用計算ロジック（残資金で購入可能な最大内部HPの算出）で、ループによる段階的算出から O(1) の代数的算術計算式への最適化が提案・導入された。その際、ゼロコスト（単価0）や無効なユニット単価の事前バリデーションを適用する重要性が確認された。

**Action**: 資金や容量を分配する計算処理では、ループによる段階的加算ではなく `repaired_hp = Math.min(max_affordable, needed_hp)` のような O(1) 算術計算式を用い、計算前にゼロ割や境界値チェックのバリデーションを完了させておく。

<a id="ace-99-1"></a>

### ACE-99-1: Native/WASM 共通の順序保持並列化基盤（map_ordered）と決定論的再現性の担保

| フィールド | 値 |
| ---------- | --- |
| Category   | performance |
| Origin     | PR #99 |
| Date       | 2026-08-15 |
| Helpful    | 0 |
| Harmful    | 0 |
| Status     | active |

**Insight**: 並列計算（Rayon 等）を導入する際、WASM（シングルスレッド）と Native（マルチスレッド）の双方で同一インターフェースを持ち、かつ評価結果のインデックス順（入力順序）を完全に保持するマッピング基盤を抽象化することで、タイブレークやスコア同点時における AI の意思決定の決定論的再現性（Determinism）をプラットフォーム間で 100% 維持できる。

**Context**: PR #99 において、AI V4 の候補評価（島嶼作戦やビームサーチ、ターン距離計算など）を並列化する際、マルチスレッド環境とブラウザ（WASM）環境で AI の行動が乖離するのを防ぐため、`deterministic_parallel::map_ordered` を導入した。また、候補数が少ない場合（`items.len() < 4`）はスレッドプールへのディスパッチオーバーヘッドを避けるため直列実行へフォールバックする閾値制御を組み込んだ。

**Action**: マルチプラットフォーム対応かつ決定論が求められるシミュレーション・AI エンジンでは、プラットフォーム差分を吸収する順序保証マッピング関数（`map_ordered`）を設計し、候補数が閾値（例: 4件）未満の場合は直列にフォールバックする制御を組み込む。

<a id="ace-99-3"></a>

### ACE-99-3: 長期ターン進行時の戦術スナップショット再利用と再割り当てループの排除

| フィールド | 値 |
| ---------- | --- |
| Category   | performance |
| Origin     | PR #99 |
| Date       | 2026-08-15 |
| Helpful    | 0 |
| Harmful    | 0 |
| Status     | active |

**Insight**: ユニット数が増大するゲーム後半（40+ターン）において、AI の行動ステップごとに盤面全体の戦術スナップショットや距離マップを再計算したり、変化のないユニットに対して部隊再割り当て（Reassignment）を毎ステップ実行すると計算量が爆発的に増大する（O(N^2)〜O(N^3)）。同一ターン内での戦術スナップショットのキャッシュ・再利用と、すでに有効な任務を持つユニットの再評価スキップ（早期離脱）を徹底することで、後半ターンの思考時間を 1/10 以下（12秒 → 1秒）に短縮できる。

**Context**: PR #99 の map_3 長期戦（40+ターン）において、終盤ターンの思考時間が 12 秒以上に達していた。原因は、ユニット行動ごとに戦術評価と距離マップを全件再構築していたこと、および部隊再割り当てループが毎ステップ走っていたことであった（コミット `81ee354`, `9941ffc`）。

**Action**: AI のターン内意思決定ループでは、行動によって変化しない戦術状況（敵の脅威分布、大域距離マップなど）をスナップショットとしてキャッシュ・共有し、すでにアサイン済みの安定ユニットに対する再評価ループを抑制するガード条件を設ける。
