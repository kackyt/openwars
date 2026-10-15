# AI再設計v3 段階3: 所属と生産の更新口の一本化

[v3提案書](ai-redesign-v3-proposal.md) §C・§D と段階3の実装方針。
開始点は `refactor/ai-redesign-v3-stage2` の `c7dd8882b5d029cb12b90caa6975e4ba1475edd1`（S4採用後）。

## 現在の状態

2026-10-15、D-4（`8424ee6`）をローカル環境で検証し、Rとして受け入れた。
基準 `c7dd8882` と28条件・50Tで比較し、全 `action_history` 本文・勝敗・ターン数が
28/28一致した。欠落・空の行動列・対局エラーは0。
Rust全テスト928件成功（ignore 2）、Clippy・WASMチェック成功。
フォーマット検査で検出した3ファイルの整形差分は `8ca3701` で修正した。
整形コミット後の再比較、D-3、Cの生産受付統合はそれぞれ28/28一致した。
C1の正規化delta接続は全Rust934件成功、28/28一致でR受入済み。
その後のroster書込み口限定・registry書込み口限定・生産所属1commitの3段も、
4変種112局の隣接比較で各28/28一致し、累積統合後のRust全検証は938件成功。
Cの構造変更口限定・正本反転・C整理Sはまだ実装中であり、段階3全体は未完了。

D-4作成時にはGit取得とRustツールチェーンが利用できず、GitHubから開始ソースを取得し
対象5ソースファイルのblob SHAを照合した。現在の検証環境は
`rustc 1.99.0` / `cargo 1.99.0` であり、当時の未ビルド状態とは区別する。

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

## D-4契約テスト（追加、実行成功）

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

D-4のRust検証・28条件比較は実施済み。goldenは更新していない。
受入結果は `.tmp/d4-review-20261015/tools/full_compare.json`、対戦の詳細は
同ディレクトリの `sweep/`、固定バイナリのSHA256は `bin/SHA256SUMS.txt` に保存した。
以下は各段階を比較するための受入手順であり、今後の並列数は `--jobs 8` とする。

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
  --max-turns 50 --jobs 8 --out-dir work/stage3-d4-50t
```

28条件すべてで `games/<版>/.../seed*.json` の **`action_history` 本文全体** を比較する。
summaryの短いhashや勝敗だけの一致では受け入れない。条件欠落・異常・空の行動列は失敗扱い。
勝敗・ターン数も確認し、最初の不一致の手番・命令と原因を特定する。
判断fingerprint（V1〜V4）と既存の固定行動列テストも通す。
Rとして全行動列が一致しない場合は挙動を戻すかSへ分離する。

## D-3実装・検証状況

`v4/production_finalize.rs` に最終施設の確定口を追加した。
拡張経路の32周・i→j順・ETA/距離の厳密改善という交換規則は変更していない。
通常経路は同じ確定口を使用するが、施設を交換しない。
交換前に購入traceとの対応を保存し、最終命令から占領意図・Planned工程・
Produced系trace・当手番のrolling purchase traceを更新する。
配備pendingは従来どおり共通 `commit_turn_production` が最終命令から生成する。
将来購入の施設や保存済み戦闘予測は変更せず、再予測も行わない。

同型2購入の相互交換、交換なし、将来購入の非変更を契約テストで検証した。
追加fixtureの最初の実行は、工場一覧に対して実地形が平地だったため交換せず失敗した。
実地形を工場へ揃えて再実行し成功した。交換規則側は変更していない。
全Rustテスト930件成功（ignore 2）、Clippy・WASMチェック・release build成功。
ソース固定は `.tmp/stage3-d3-20261015/src/` と `d3.patch` に保存した。
直前の整形版との28条件比較は全行動列本文・勝敗・ターン数が28/28一致した。
異常・欠落・空の行動列は0。D-3はRとして受け入れた。
結果は同ディレクトリの `sweep/` と比較JSONへ保存した。

## Cの移行状況

生産完了のEventReaderを `v4/production_events.rs` の1受付にまとめた。
R移行段階では配備の全イベント処理→campaignの全イベント処理という順序を保持し、
同じイベントで生成される両系統のSquadと、その後の発番順を変えない。
両台帳の事実記録は残し、所属競合の最終解決は既存の正規化更新口で行う。
これは受付の統合であり、所属の正本反転や1回だけの所属commitの完了を意味しない。
受付統合の直前D-3版との28条件比較は全行動列本文・勝敗・ターン数が28/28一致した。
結果は `.tmp/stage3-c2-receipt-20261015/sweep/` に保存した。

C1では `planning/assignment.rs` の提案→resolve→delta→commitを、現行の正規化入口へ
実際に接続した。resolve中は台帳とSquadを変更せず、commitは従来の順で
敗者参照除去→勝者登録→旧戦術owner解放を適用する。
Campaignに勝者がなくても旧owner・旧squad_idは保持するが、それを投影勝者に混ぜて
古いmemberを復活させない。積荷・到着・旧具体Squad・最小IDの優先、正式handoff、
異player参照の保護、戦術owner解放を契約テストで検証した。
C1はrelease/debugとも全Rust934件成功（ignore 2）、Clippy・WASM・fmt成功。
debugの全実行ログは `.tmp/stage3-c1-20261015/debug-tests.log` に保存した。
固定ソースは `.tmp/stage3-c1-20261015/src/` と `c1.patch`。
直前の受付統合版との28条件は全行動列本文・勝敗・ターン数・条件metadataが一致した。
判定は各段階の `full_compare_meta.json` に保存した。
全受入済みRの対戦成績はV4通常8勝・上限判定13勝、V200通常6勝・上限判定1勝で不変。

### Cの追加R（受入済み）

1. **roster更新口**: `MemberRoster` / `CargoRoster` / `TransportBinding` を導入し、
   個々のmembers・cargo・delivered・transportの書込みを `RosterCommit` へ移した。
   汎用編成、campaign再編、hold、配備、輸送波、Reserve、物理状態handoffの集合演算と
   適用順を維持する。本番にはDerefMut/Fromを提供せず、fixtureだけcfg(test)で書込みを許す。
2. **registry更新口**: mutation methodを `LedgerKey` 必須の内部 `apply_*` に限定した。
   claimは純粋な `CampaignClaimProposal → resolve → ClaimDelta → commit_claims`、
   生産seed・輸送波のowner移管・撃破解放も台帳の入口を通す。
3. **生産所属1commit**: 各台帳は照合事実と所属提案だけを記録し、配備全件→campaign全件の
   提案列を `commit_produced` で1回適用してSquadIdを各台帳へ書き戻す。
   同じEntityを両台帳が提案した際も両候補と旧発番を保持し、一意化は既存resolveに任せる。

この3段の固定src/patch/bin/判定は `.tmp/stage3-c3-20261015/` に保存した。
`src-r1` / `src-r2` / `src-r3` を4変種112局で比較し、各隣接版の全行動列・勝敗・
ターン数・条件metadataが28/28一致。累積を主作業ツリーへ統合し、全Rust938件成功
（ignore 2）、fmt・Clippy全target全feature・WASMチェック成功。

### 未完了の境界

実行binding（Squadのroster）と正規化所属・旧Squad履歴（registry）の2層を保持するR移行であり、
現在のcanonical bindingをledgerだけに持ちSquadへ導出する正本反転はまだ行っていない。
正規化前に複数Squadへ入った途中状態を読む処理があるため、即時一意化はSとして扱う。

rosterの要素変更は更新口へ移したが、pub field全体へのDefault/Copy/duplicateの代入や、
Squad一覧のretain/remove/extend/sort/pushなどの構造変更はまだ完全には閉じていない。
readonly viewと構造変更口を導入するC2cで対応中。
台帳のopen/roster入口はcrate内のcaller責務であり、ECS World全置換を防ぐセキュリティ境界ではない。
生産の同record重複・異player混在・managerなし等の追加契約とfallback不変条件の検出も対応中。

内部書込みAPIは意図的に縮小し、旧pubの `release_operation` は本番外部crateから直接呼べなくなった。
型の書込みを閉じる変更なので、外部ソースAPIの完全互換は主張しない。
C2d/C2cの次候補ではprivate storage＋readonly Deref、compile_fail契約、構造変更口と
追加生産契約を実装したが、独立レビューで輸送波の旧 `drop_position = None` の欠落を
発見した。この未修正版は対戦が一致してもRとして採用しない。旧処理復元と再現テスト、
`squads_mut` の公開範囲・source監査の補強を行って再比較する。採用R3版には欠落はない。
未修正のsealed-roster/sealed-listも3変種84局では隣接各28/28一致したが、
この試験で退行状態が発生したとは確認できないため、未修正版の受入根拠にはしない。
### R6修正版とS1aの受入

R6で旧降車位置クリアを復元し、外すと失敗・戻すと成功する
`promoted_first_wave_does_not_inherit_forming_drop_position` を追加した。
`squads_mut` はcrate内公開に縮小、fallback debug/release契約も追加。
主作業ツリーで全Rust955件（ignore 2）・fmt・Clippy・WASM成功。
受入済みR3対R6の28条件は全行動列・勝敗・ターン・metadataが一致した。
これにより、field/list封じ込みを含む修正版一式をRとして受け入れた。

S1aはcurrent bindingと過去Squad履歴の分離である。
Campaignに勝者がなければ現在 `squad_id=None` とし、旧IDは `previous_squads` に保持する。
次回の同点規則だけは `resolution_priors` が履歴を補う。
正本の現在bindingを確定してからrosterを投影し、履歴をmemberへ復活させない。
旧Squadの戦術/DAGを引いていた読み手がNone/Errを得る可能性はSとして扱う。
主作業ツリーで全Rust956件（ignore 2）・fmt・Clippy・WASM成功。
R6対S1aの28条件でも全行動列・勝敗・ターン・metadataが一致し、この範囲で劣後は観測されなかった。
改善を示す結果ではなく、別入力での潜在的挙動差があるためS分類を維持する。

結果は `.tmp/stage3-c3-20261015/compare-r3-to-r6.json` と `compare-r6-to-s1a.json`。
S1aのrosterはまだSquad内の提案viewを読む中間段階であり、提案store化は未完了。

### 次段階

R6bでsource監査のtest module範囲・CRLF対応を補強し、test後の本番関数を残す契約を追加。
S1b-1で、正規化直後の固定点・Reserve・輸送波の割当済み集合を現在bindingから読む候補を作った。
この候補のレビュー・28条件比較は進行中。
台帳側proposal storeへの実体移動、全writerのkeyed API化、Squadのcommit後projection化、
cache・Squad差分維持・固定点等のS整理を続けて実装・検証する。

## D-4作成時に確認したこと

- 開始点のコミットと対象5ファイルのGit blob SHAをGitHub経由で確認。
- ソースの静的読解。配分中の直接要求上書きをledgerへ移す差分を作成。
- patchの空白検査と、取得した基準ソースへの適用確認。

作成時のこれらの確認はRust検証・対戦比較の代替ではない。
その後のローカル実行検証結果は本書の現在状態と受入検証に記録した。
