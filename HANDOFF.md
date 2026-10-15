# Handoff: AI再設計v3 段階3の実装・レビュー・対戦検証

**Generated**: 2026-10-15 12:24 JST
**Branch / HEAD**: `refactor/ai-redesign-v3-stage3-d4` / `8ca3701`
**Status**: **In Progress。段階3は未完了。** 大量の実装・検証は済んだが、S1のproposal store移行途中。S2以降は未着手。

## Goal

当初は [段階3設計書](docs/architecture/ai-redesign-v3-stage3.md) のD-4レビューと28局試験。その後ユーザーは「問題なければ整形修正をコミットしてstage3の残りを実装」「プランで止めず、実装→レビュー→対戦結果報告まで」と明示した。

**承認待ちの計画で停止しない。ただし未完了を完了扱いにしない。今後の対戦は `--jobs 8`。**

## Completed

- [x] D-4 (`8424ee6`) を基準 `c7dd8882` と28条件50Tで比較。全action_history本文・勝敗・ターン一致。Rust928成功/ignore2、Clippy/WASM成功。
- [x] fmtの3ファイルだけ修正して **`8ca3701` にコミット**。再比較28/28一致。**このセッションでの新コミットはこれだけ、pushなし。**
- [x] D-3: 施設交換規則を保ったまま、命令・占領意図・Planned工程・Produced trace・当手番rolling purchase traceを同じ施設へ確定。通常経路は交換しない。Rust930成功、28/28一致。
- [x] 生産EventReaderを1受付に統合（配備全件→campaign全件という旧発番順を維持）。Rust931成功、28/28一致。
- [x] C1: 既存の所属正規化へ純粋resolve→delta→commitを実配線。Rust release/debugとも934成功/ignore2、28/28一致。
- [x] rosterの要素書込みをRosterCommitへ、registry書込みをLedgerKey付きapplyへ限定。claim/seed/移管/撃破解放を台帳入口へ。生産所属も提案収集→1commit→SquadId書戻しへ。3段の隣接比較各28/28、累積Rust938成功。
- [x] Squad/Managerのprivate storage＋readonly Deref、構造更新portal、compile_fail doctestを導入。中間候補の降車位置クリア欠落をレビューで発見しR6で復元。red/green再現テスト追加。修正版Rust955成功、R3→R6全28一致。
- [x] S1a: 現在bindingと過去SquadId履歴を分離。投影は現在bindingのみ。同点規則は履歴を補う。Rust956成功、R6→S1a全28一致（S分類維持）。
- [x] R6b: test module後の本番コード・CRLFもsource監査に残す補強。
- [x] S1b2: manager側ProposalStore＋Entity候補索引を導入し、**汎用編成・生産所属の2本番writerをkeyed APIへ実移行**。未移行writerにはunsynced互換同期を残す。
- [x] S1b1のmixed/foreign Entity重複加入を再現・修正。既存squad_is_mutable_by_playerを再利用し、変更不能なroster参照をallocator unavailableへ含める。Rust960成功、S1a→修正S1b2全28一致。
- [x] S1b3: 親がcampaign責務をID事前収集→keyed store適用へ移行。配備側も同様に移行。worktreeでRust961成功、未使用旧APIを削除後Clippy/WASM成功。主作業へ統合・release build成功、S1b2→S1b3全28一致。

## Not Yet Done

- [ ] **S1本体**: 残るupdate/hold/Reserve/輸送phase等のwriterをmanager単位keyed APIへ移行。下記aed worktreeに未検証checkpointあり。
- [ ] roster実体を台帳のproposal storeへ移し、Squad側をcurrent正本からのcommit後projectionだけにする。`unsynced` を使ったSquad→store逆算を全writer移行後に廃止する。
- [ ] 途中の多重候補を読む判断を、明示的なproposal view APIへ移す。即時一意化で旧処理を黙って変えない。
- [ ] S2: AdoptedTurnPortfolioを編成前cacheからcommit後の実所属投影へ。
- [ ] S3: campaignのSquad作り直しを差分維持へ。
- [ ] S4: 固定点最大3周の廃止/縮小。**周回上限だけ下げない**（古いcache/作り直しが原因）。
- [ ] S5〜S7: 輸送波、明示Reserve、手番末再接続の整理を各1件ずつ比較。
- [ ] S8: DAGのcommitment_members引継ぎcacheを安定した作戦ID参照へ。
- [ ] 現行主作業ツリーの最終状態で全Rust検証・独立レビューを再実行。docsとレビュー報告はS1b2/3までの更新が追いついていない。
- [ ] 各Sを直前採用版と比較し、明らかな劣後を採用しない。最終報告まで行う。

## Failed Approaches (Don't Repeat These)

1. **git archiveを親repoのsubdirへ展開して `git -C <src> apply`** → 親repoの相対パス判定でpatchがスキップされ、exit0でも元ソースのままだった。**repo rootから `git apply --directory=<src> <patch>`** にし、宣言/呼出しをGrepで確認。未適用D3 bin `c5c15967…` は採用していない。
2. **未接続OperationLedger wrapperを正本化完了とする案** → 却下。resolveが直接変更し、件数だけのdelta、membersへの追加だけでは移動/解放を表せない。実配線・実体delta・pure resolveを要求した。今もS1は未完。
3. **registry全割当を投影winnerとして再利用** → winnerなしCampaignに残る旧squad_idから古いmemberが復活。Rでは新winnerだけを投影、S1aでcurrent bindingとhistoryを分離。
4. **機械的な既定値代入削除** → C2dで `forming.clone()` 後の `wave.drop_position = None` まで消えた。Some旧降車位置を持つFormingで退行。R6の `promoted_first_wave_does_not_inherit_forming_drop_position` で修正を外すred/戻すgreenを確認。**未修正R4/R5は28一致でも未採用**。
5. **canonical boundだけで未所属判定** → mixed/foreign roster内の自軍Entityが新Reserveにも重複加入。再現テストは `保護されたmixed/foreign参照を新しい自軍Squadへ重複加入させない: None` で失敗。`allocation_unavailable_entities` に既存の変更不能判定を適用して修正。
6. **pub readonly Newtypeだけで全書込みを型封鎖と主張** → field全体のDefault/Copy/duplicate代入は可能。private storage＋Derefに改めた。なお `squads_mut()->&mut[Squad]` ではsort/swap/全Squad置換が可能。portal使用はsource契約であり、完全な型/セキュリティ保証ではない。
7. **sourceを最初の `mod tests` 以降切り捨て** → v4/extended.rsの後半の本番関数まで監査漏れ、CRLFでも誤判定。R6bでtest itemのbrace範囲のみ除外。raw string/複合cfg等は近似であり完全parserではない。
8. **subagent生成patchを正しいbase deltaと信用** → aedのpatchは2度とも既存baseを含む累積（roster.rsが `/dev/null` から新規追加）だった。**固定srcと実ファイルを `git diff --no-index` で比較**して親が必要箇所だけ統合。新checkpointも盲目的にapplyしない。
9. **LF/CRLF混在のunified patch** → default applyが失敗。baseとmainがraw byteで一致すると確認して `--ignore-space-change` で適用したが `git diff --check` が `trailing whitespace` で失敗。`cargo fmt --all -- --config newline_style=Unix` で整形後に成功。config/hookを迂回してはいない。
10. Clippy `error: method replace_members is never used` → keyed移行で不要になった旧RosterCommit APIを削除。allow(dead_code)で隠さない。
11. Claude系実装/ベンチagentが429停止: `All credentials for model claude-opus-5-5 are cooling down ... rate_limit_error`（Haikuも同様）。時間を置いた1回の再開も失敗し、親が直接実装・検証を引継いだ。credential/config変更や再試行ループはしない。

## Key Decisions

| Decision | Rationale |
|---|---|
| R/Sを分け小段階で固定source/bin比較 | 所属winner・適用順・SquadId発番・行動列を変える箇所を特定する |
| current bindingとprevious_squadsを別に保持 | 履歴は同点判定用、行動/DAGやmember投影へ復活させない |
| 多重候補はproposal viewに残す | 途中判断が読むため、writerごとの即時一意化は挙動変更が大きい |
| full action_history本文を比較 | 短いhash/勝敗だけではR受入不可、28一致も未知状態の安全証明ではない |
| 旧未追跡成果物を保存 | 大量のログ/ROM/分析fileはユーザー既存作業。reset/clean/一括add禁止 |
| commitは整形修正だけ | ユーザー明示はfmt commit。後続実装は未コミット、push許可なし |

## Current State

**Working**: mainはS1b3（generic/produced/campaign/deploymentのkeyed移行）まで統合。4群以外はlegacy RosterCommit＋unsynced同期が残る。最新比較は全28一致。golden更新なし。

**Broken / Blocked**: 現在確認済みのmain runtime退行はなし。ただし**stage3未完**。Opus/Haiku backendは429。最新主作業のS1b3について、worktreeで961テスト/Clippy/WASM成功・mainでbuild/56局成功だが、**mainの同じ最終状態で全テストを再実行すること**。

**Uncommitted Changes**: tracked約26 files（+1441/-854行、handoff時点のstat。未追跡新module分は含まない）。重要な未追跡実装:
- `engine/src/ai/planning/assignment.rs`
- `engine/src/ai/planning/assignment/roster.rs`
- `engine/src/ai/v4/production_events.rs`
- `engine/src/ai/v4/production_finalize.rs`

**Worktrees（消さない）**:
1. `.claude/worktrees/agent-a249fd324bd722b2d/`: 元Opus。HEAD8ca3701、indexはS1b1までの累積、unstaged5 filesが親のS1b2/3実装。**mainへ統合済みの部分**。base/dirty状態を確認し上書きしない。
2. `.claude/worktrees/agent-aed04cd6efa99e85b/`: 最新の残writer作業。HEAD8ca3701、**staged3 files**（roster.rs追加969行、squad.rs変更、deployment.rs変更）、unstagedなし。main未統合のupdate/hold/Reserve/detach_deferred_pickup_cargo/update_transport_squad_phase移行を申告。**未検証・正しいbase deltaではない**。handoff依頼でcheckpoint保存後停止した。
   - agent返却: 「環境起因の差分調整中、次agentでS1b2 baseへ統合確認」。全test合格を主張しない。
   - patch: 上記worktree内 `.tmp/stage3-c3-20261015/deployment-keyed-store.patch`。HEAD8caからの累積なのでそのままapply禁止。

**Live tasks**: handoff時点でcargo/対局python processなし。ListAgentsでaedがrunningと出た後、checkpoint返却・停止通知を受信済み。mcp-grafana用pythonは別作業なので触らない。

## Files to Know

| File | Why It Matters |
|---|---|
| [段階3仕様](docs/architecture/ai-redesign-v3-stage3.md) | R/S境界・途中経過。ただし最新S1b2/3反映が一部遅れている |
| [提案書C/D](docs/architecture/ai-redesign-v3-proposal.md) | 最終目標、固定点の実測、D5等の非対象 |
| [assignment](engine/src/ai/planning/assignment.rs) | pure resolve、claim、生産commit、current投影、source監査 |
| [roster/store](engine/src/ai/planning/assignment/roster.rs) | Squad/Manager readonly view、ProposalStore、keyed ProposalCommit、旧RosterCommit |
| [registry](engine/src/ai/operation_assignment.rs) | current binding、previous_squads、逆引き、LedgerKey付きapply |
| [squad](engine/src/ai/squad.rs) | campaign責務・Reserve/輸送/固定点、allocation_unavailable_entities、残writer |
| [汎用commit](engine/src/ai/squad/assignment.rs) | 移行済みgeneric writer、store契約 |
| [配備](engine/src/ai/v4/deployment.rs) | 移行済みprepare writer |
| [単一生産受付](engine/src/ai/v4/production_events.rs) | D全event→C全event→1commit→台帳書戻し |
| [施設確定](engine/src/ai/v4/production_finalize.rs) | D3、交換条件/再予測を変更しない |
| [中間レビュー](review_refactor_ai-redesign-v3-stage3-d4.md) | ローカルfileあり。ただしgitignore対象、最新情報は本handoff優先 |

## Code Context

現物のsignature（細部は上記fileで確認）:

```rust
pub struct UnitOperationAssignment {
    pub owner: OperationOwner,
    pub squad_id: Option<SquadId>, // CURRENTのみ
    pub role: OperationUnitRole,
    pub assigned_turn: u32,
}
// UnitOperationRegistry
pub fn assignment(&self, entity: Entity) -> Option<UnitOperationAssignment>;
pub fn previous_squad(&self, entity: Entity) -> Option<SquadId>;
pub(crate) fn resolution_priors(&self, player_id: PlayerId)
    -> HashMap<Entity, UnitOperationAssignment>;

pub(crate) struct OperationLedger<'a> { registry: &'a mut UnitOperationRegistry }
// 関連関数: OperationLedger::open, ::proposals, ::roster（legacy）, ::squad_list
pub(crate) fn proposals(manager: &mut SquadManager) -> ProposalCommit<'_>;

pub(crate) struct ProposalStore {
    by_squad: HashMap<SquadId, ProposedRoster>,
    by_entity: HashMap<Entity, Vec<(SquadId, OperationUnitRole)>>,
}
pub(crate) struct ProposedRoster {
    pub(crate) members: BTreeSet<Entity>,
    pub(crate) transport: Option<Entity>,
    pub(crate) cargo: Vec<Entity>,
    pub(crate) delivered: Vec<Entity>,
}
// ProposalCommit: manager単位、SquadId-keyed
pub(crate) fn create_owned(&mut self, mission: MissionType, owner: PlayerId,
    members: BTreeSet<Entity>, transport: Option<Entity>, cargo: Vec<Entity>) -> &mut Squad;
pub(crate) fn join(&mut self, id: SquadId, entity: Entity) -> bool;
pub(crate) fn leave(&mut self, id: SquadId, entity: Entity) -> bool;
pub(crate) fn clear_members(&mut self, id: SquadId);
pub(crate) fn replace_members(&mut self, id: SquadId, members: BTreeSet<Entity>);
pub(crate) fn remove_mentions(&mut self, id: SquadId, entities: &HashSet<Entity>);
pub(crate) fn squad_mut(&mut self, id: SquadId) -> Option<&mut Squad>; // metadata用
// Manager
pub(crate) fn sync_proposals(&mut self);
pub(crate) fn synced_proposals(&self) -> &ProposalStore;
```

**重要な不変条件**:
- loaded→arrived→旧具体Squad→同owner→Campaign→その他、各rank/最小IDというwinner規則を維持。winner HashMapの登録列挙順も変えない。
- R時点のwinnerなしCampaignは旧owner/旧ID保持。ただしS1a以降はCURRENT=None、旧IDは履歴に分離。historyは投影に使わない。
- 旧transport bindingを変更しない除去（campaign責務）と、cargo/delivered/transportすべて解放する除去を混同しない。
- `ProposalStore::update` は所有権take→旧索引除去→変更→再索引。joinごとの全roster cloneを避ける。
- 移行済みkeyed writerはstoreとSquad viewへ同じ差分を適用。残writerはunsyncedを立て、次のstore読取前に同期。**この逆算は移行完了後に廃止する対象**。

## Test / Benchmark Evidence

固定source・patch・binは主に `.tmp/stage3-c3-20261015/`。各binのSHAは `bin/SHA256SUMS*.txt`。build中に評価binを上書きしない。

| Comparison | Result file（同directory直下） |
|---|---|
| R3→R6 | `compare-r3-to-r6.json`、28/28 |
| R6→S1a | `compare-r6-to-s1a.json`、28/28 |
| S1a→未修正S1b1 | `compare-s1a-to-s1b1-parent.json`、28/28だがmixed再加入bugのため未採用 |
| S1a→mixed修正＋2writer S1b2 | `compare-s1a-to-s1b2-parent.json`、28/28 |
| S1b2→campaign/deployment S1b3 | `compare-s1b2-to-s1b3-parent.json`、28/28 |

全比較: map_1/map_2/map_6/map_25/map_32/map_42/map_10、seed1,2、先後both、hex、V4対V200、50T。S1b2/3は各両版56局、全action_history本文・結果・turn・metadata一致、異常欠落0。
**全採用版の成績はV4通常8勝＋上限判定13勝、V200通常6勝＋上限判定1勝＝21対7、勝率75%。通常勝利のV4平均33.125T、上限込み43.571T。改善を意味しない。**

古い成果物: `.tmp/d4-review-20261015/`、`.tmp/d4-fmt-review-20261015/`、`.tmp/stage3-d3-20261015/`、`.tmp/stage3-c2-receipt-20261015/`、`.tmp/stage3-c1-20261015/`。

## Resume Instructions

1. `git status --short --branch` / `git log --oneline -5`。**HEAD8ca3701、大量未コミット＋新module未追跡を期待**。旧handoffは `.tmp/HANDOFF-before-stage3-20261015.md` に退避済み（古い2026-10-02作業へ戻らない）。
2. まず現行mainを確認:
   ```sh
   cargo fmt --all -- --check
   cargo test --release --locked --no-fail-fast
   cargo clippy --all-targets --all-features -- -D warnings
   cargo check --locked -p engine --target wasm32-unknown-unknown
   git diff --check
   ```
   期待: 961成功/ignore2前後（追加契約分を数える）。失敗は意図を読んで直し、golden更新/テスト削除で隠さない。
3. aed worktreeのstaged3filesを**mainと実ファイル比較**。そのcheckpointはまだtest完了していない。特に同名ProposalCommit APIと親が追加したreplace_members/remove_mentionsの消失に注意。必要な機能だけmergeし、新しい意味群patchを保存する。whole cumulativeを直接apply/cherry-pickしない。
4. 残writerを `ID収集→OperationLedger::proposals(manager)→keyed適用` へ移す。`&mut Squad` しか持たない関数は呼出し/loopを整理し、未接続storeだけ足して完成と呼ばない。移行ごとにindex/store/viewの一致とmixed/foreign保護を契約化。
5. 候補ごとにfixed sourceを新しい空directoryへ、同toolchain release/lockedでbuild→別名binコピー→SHA保存。2変種例:
   ```sh
   python -X utf8 .tmp/d4-fmt-review-20261015/src-fmt/scripts/run_matchup_sweep.py \
     --variant baseline=<固定bin> --variant candidate=<固定bin> \
     --p1 V4 --p2 V200 --grid-type hex \
     --map map_1,map_2,map_6,map_25,map_32,map_42,map_10 \
     --seeds 1,2 --player-order both --repeats 1 --games 1 \
     --max-turns 50 --jobs 8 --out-dir .tmp/<新規出力>
   python -I -X utf8 .tmp/d4-review-20261015/tools/compare_full.py \
     --sweep .tmp/<出力> --out .tmp/<比較>.json \
     --maps map_1,map_2,map_6,map_25,map_32,map_42,map_10 --seeds 1,2 \
     --baseline baseline --candidate candidate
   ```
   期待: Rは28/28本文・結果・turn・meta一致。Sは差分を最初の手まで調べ、通常/上限勝利を区別し劣後を採用しない。通常経路の揺れが疑われたら3反復以上。
6. S1完了後、S2→S8を各独立patchで評価。最後に独立レビュー・docs/中間報告更新・対戦報告。**追加commit/pushはユーザーの指示を確認**。

## Warnings

- Webはpnpmのみ。返答・報告・コードコメントは日本語。
- D5（発行拒否時の工程取消/意図解放）、占有モデル統一、施設再予測/交換条件強化は今回に混ぜない。
- main本文を基盤にコード単位拡張を残す。map/seed固有hack、プリセット化、面積ゲート撤去は禁止。
- readonly typeはセキュリティ境界ではない。World resource全置換、mutable slice一般操作、全Squad置換の一般能力は残る。source監査は近似（raw strings/複合cfg等）であり証明とは言わない。
- `git diff --stat` には新module未追跡分が出ない。stage3をcommitする時も既存大量未追跡物を一括addしない。
- 多数のarchive配下`.claude/skills`がscoped skillとして見えるが、実装本体はrepo root。検索は必ずpathを狭め、target/.tmp全体のrecursive Globで数十万fileをなめない。
- 最終テスト/比較はfixed sourceの実際のSHA・baseを確認。subagentの「成功」「基準delta」申告だけを証拠にしない。
