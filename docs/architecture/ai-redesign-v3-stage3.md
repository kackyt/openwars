# AI再設計v3 段階3: 所属と生産の更新口の一本化

[v3提案書](ai-redesign-v3-proposal.md) §C・§D と段階3の実装方針。
開始点は `refactor/ai-redesign-v3-stage2` の `c7dd8882b5d029cb12b90caa6975e4ba1475edd1`（S4採用後）。

## 現在の状態

D-4のコード差分と契約テストを作成した。**未ビルド・未受入**。
作業環境のGit取得はプロキシ接続失敗、`cargo` / `rustc` は利用不可。
GitHub接続から開始コミットの対象ソースを取得し、5ファイルすべてのGit blob SHAが
開始コミットと一致することを確認した。元のローカルチェックアウトは更新していない。
GitHub接続で開始コミットを親にしたコミットを作成し、専用ブランチ
`refactor/ai-redesign-v3-stage3-d4` へ保存する。指定ブランチをローカルへcheckoutして
実行検証したものではない。ローカルの抽出用snapshotコミットはpushしない。

## 順序とR/S境界

| 順序 | 範囲 | 区分・受入条件 |
|---|---|---|
| 1 D-4 | 元の要求を保持し、合同パッケージの量を実効要求として別に記録。既存の不足・再開理由と分離 | R候補。配分先・量・再開のタイミングを保持し、全行動列一致で受入 |
| 2 D-3 | 命令・計画工程・配備/占領意図・traceの最終施設を1つの更新口で確定 | 交換規則を保持する部分だけR。継続計画の保護、交換条件追加、再予測はS |
| 3 C | 所属のpropose/resolve/commit入口と、生産イベントの1回だけの解決を段階的に導入 | 現在の勝者・適用順・SquadId発番順を再現できた部分だけR |
| 4 Cの整理 | 固定点、古いcache、Squadの作り直し、輸送波、Reserve、手番末再接続 | S。1件ずつ50T・28条件で直前採用版と比較 |

占有モデルの統一は段階2に残る別のS。段階3のRへ含めない。
発行拒否時の工程取消と配備・占領意図の解放（D-5）も今回へ含めない。
不足理由の細分化で候補の再探索や再開条件を変える場合は、別のSとして評価する。

## 目標構造と所属の移行手順

最終形は、`planning/assignment.rs` の `OperationLedger` が所属の正本を持ち、
`AssignmentProposal → resolve → AssignmentDelta → commit` だけが所属を更新する構造。
`Squad.members` はledgerの投影、campaign・配備・DAGは安定した作戦IDを参照する。
最終形への全面移行を最初のRでは行わない。

1. 現在の更新口・正規化の勝者規則・claimの保護/解放・Squadの列挙/発番順を契約化する。
   生産slotからEntityへ接続する二つの経路と、正規化後の所属を同時に観測する。
2. `AssignmentDelta`（追加・移動・解放）を導入し、既存の更新を同じ順序で適用するadapterへ集める。
   この時点では`UnitOperationRegistry`を正本へ反転させず、既存のSquad更新と再正規化を残す。
3. 生産イベントの受付を1つにする。campaignと配備の意図を収集し、既存の勝者と
   イベント/Squad適用順を再現してから1回だけcommitする。同じイベントの再処理も契約化する。
   台帳の重複を消してもEntityの所属・SquadId・行動順が変わればRとして受け入れない。
4. 更新口を移せた箇所ごとにledger投影との不一致を検査する。読み手・書き手を移し終えてから
   ledgerを正本にする。型名の変更だけを完了扱いにしない。
5. 固定点の削除、cacheをcommit後の一覧へ更新、同じSquadを差分で維持、DAGの安定ID参照は
   個別のSとして実装・評価する。SquadIdを維持すること自体が同点処理やDAG指令を変えうる。

中間段階でも正本と移行済み範囲を明示し、Squadから所属を再構築する経路を
黙って廃止しない。Rで勝者規則を再現できない箇所は、未移行のまま残してSへ切り出す。

## D-4の差分

- `Operation::slots` は作戦分析が導出した元の要求として保持する。
- `DemandLedger::set_effective` は枠ごとの実効要求と `JointPackageSelected` を別に記録する。
  未指定の枠は元の要求を使う。0への縮小も元の要求を超える量も、従来と同じ値として扱う。
- 小規模経路の合同案選択後の `slots.capture_units` 上書きをledger更新へ置き換える。
  `reopen` の位置は維持し、以前の未充足理由だけを取り消す。元の要求は変えない。
- 配分先と未充足率は `allocation_slots` が返す実効要求を使う。
  `effective_slots` は従来どおり閉じた枠を0として扱う。施設選択・探索・予算・購入順を変えない。
- `ProductionPlanTrace.demands` とMCPの生産snapshotへ配分終了時の
  `requested` / `effective` / `filled` / `adjustments` を追加する。
  `unfilled_slots` もMCPへ出力し、元の要求量と既存の未充足理由を観測できるようにする。
  既存の `operations` は配分開始時のsnapshotとして残す。

`effective` は当手番の配分対象の要求であり、生産完了数ではない。
`filled` は購入案として確保した量で、命令発行や `UnitProducedEvent` の成功数ではない。
生産イベント・発行拒否の解決は後続段階の責務。ここで成功扱いに置き換えない。
`NoCandidate` は従来どおり予算・施設・到達性を含む理由であり、推測で予算不足と断定しない。
合同案による実効要求の変更と未充足理由を別々に保持する。

## 契約テスト（追加、未実行）

| テスト | 契約 |
|---|---|
| ledger: 実効要求の保持 | close/reopenで元の要求・実効要求を変更せず、別作戦/別枠に波及しない |
| ledger: 0・拡大・順序 | 0と要求超過を表現し、更新で記録順を変えない |
| 小規模: 占領購入の無い合同案 | requestedを残し、effective=0なら配分しない。未充足traceは元の要求量を持つ |
| 小規模: 従来との配分比較 | effective 0/1/2/6 × filled 0/1/3の12例で、旧上書きと配分先・未充足率を比較 |
| MCP: JSON出力 | requested/effective/filled、変更理由、未充足理由と元の要求量を出力する |

最初の小規模テストはledgerと配分helperの契約検査であり、rolling plan選択まで含む
統合テストではない。統合挙動の不変性は次の全行動列比較で確認する。

## 受入検証

Rust検証・対戦比較はまだ実施していない。goldenは更新しない。

```sh
cargo fmt --all -- --check
cargo test --release --locked --no-fail-fast
cargo clippy --all-targets --all-features -- -D warnings
cargo check --locked -p engine --target wasm32-unknown-unknown
```

基準は段階2末尾の `c7dd8882`。同じtoolchain・ビルド設定で開始コミットのバイナリを
`work/stage3-baseline/release/mcp-server`、候補を `work/stage3-candidate/release/mcp-server` に固定する。
開始コミットで `CARGO_TARGET_DIR=work/stage3-baseline cargo build --release --locked -p mcp-server`、
候補適用後に `CARGO_TARGET_DIR=work/stage3-candidate cargo build --release --locked -p mcp-server`。
同時にソースを切り替えてビルドしない。両バイナリのSHA256を保存する。

```sh
python scripts/run_matchup_sweep.py \
  --variant baseline=work/stage3-baseline/release/mcp-server \
  --variant d4=work/stage3-candidate/release/mcp-server \
  --p1 V4 --p2 V200 --grid-type hex \
  --map map_1,map_2,map_6,map_25,map_32,map_42,map_10 \
  --seeds 1,2 --player-order both --repeats 1 --games 1 \
  --max-turns 50 --jobs 4 --out-dir work/stage3-d4-50t
```

28条件すべてで `games/<版>/.../seed*.json` の **`action_history` 本文全体** を比較する。
summaryの短いhashや勝敗だけの一致では受け入れない。条件欠落・異常・空の行動列は失敗扱い。
勝敗・ターン数も確認し、最初の不一致の手番・命令と原因を特定する。
判断fingerprint（V1〜V4）と既存の固定行動列テストも通す。
Rとして全行動列が一致しない場合は挙動を戻すかSへ分離する。

## この環境で確認したこと

- 開始点のコミットと対象5ファイルのGit blob SHAをGitHub経由で確認。
- ソースの静的読解。配分中の直接要求上書きをledgerへ移す差分を作成。
- patchの空白検査と、取得した基準ソースへの適用確認。

これらはRustの型検査・テスト・フォーマット検証・28局の行動列一致を代替しない。
施設確定（D-3）と所属の正本移行（C）はこのpatchには未実装。
