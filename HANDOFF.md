# HANDOFF: AI再設計v3 段階3 WIP checkpoint

更新: 2026-10-15。ブランチ: `refactor/ai-redesign-v3-stage3-d4`。
開始コミット: `c8ad829`。ユーザー指定の `refactor/ai-design-v3-stage3-d4` は存在せず、
実在する上記ブランチを `pull --ff-only` した。開始treeはclean。

**段階3は未完了。ユーザーの「キリのいいところでHANDOFFを生成してコードをpush」の依頼で中断。**
現在のRustコードはS8までの累積候補。S3〜S8は対戦未受入で、golden回帰3件も未解決。
完了したリファクタとして扱わず、WIPとして継続すること。対戦・ビルドの稼働jobは停止／完了済み。

## このcheckpointに含む実装

| 段 | 実装 | 受入状態 |
|---|---|---|
| S1 | cleanup・hold・Reserve・輸送phase・降車handoffをSquadId-keyed ProposalCommitへ。manager側ProposalStoreとEntity索引を先に更新。本番のunsynced逆算同期を除去 | 28/28履歴本文・勝敗・turn一致で受入 |
| S2 | AdoptedTurnPortfolioを最終commit後の生存する現在bindingへ投影。実所属と生産の専任Capture予約を分離 | 全28条件完了。下記トレードオフを記録して後続比較へ進めた |
| S3 | 継続中の配備Squadをcampaignの転用・解散から保護してIDを維持 | 短期再生済み。28条件は8局で中断、未受入 |
| S4 | 未所属なしなら再接続しない。関係作戦だけを修復し、固定3周を集合変化・循環検出へ | 候補のみ。28条件未実施 |
| S5 | 追加輸送波を通常のSquadDraft→AssignmentProposal→commitへ。手動発番を除去 | 同上 |
| S6 | V4のReserveを通常配分へ解放後、連続Reserveの受入IDを再利用。foreign/mixedは保護 | 同上 |
| S7 | 手番末の既存Reserve全解放を除去し、新たな孤立だけを修復 | 同上 |
| S8 | DAGをSquadOperationId→現在Squadの索引で継続。commitment_membersとRouteSquadのmember複製を除去 | 同上 |

所属の現在正本はUnitOperationRegistry、正規化前の提案正本はProposalStore。
Squad所属fieldは読取り互換viewで、途中には多重候補を含む。一意所属照会にはregistry、
途中判断には `proposal_view()` / storeを使う。resolve→delta→commitの後に現在bindingを投影する。
mutable slice・resource全置換等を閉じるセキュリティ境界ではない。source監査も近似parser。

S2ではMemberを現在のCapture任務の場合だけCaptureへ分類し、Attack歩兵を能力だけで数えない。
Cargoの機能役割は採用時の意図を保持する。生産予約は「採用Capture担当 ∩ 現在の有効Capture実所属」。
追加の実所属で予約を広げず、再投影でも増やさない。元意図で所属を付与・復活させない。
目標・予算・購入不足は戦略意図として保持する。

## 完了した検証と、失敗を隠さない記録

証拠は [checkpoint報告](docs/reports/ai-stage3-checkpoint-20261015/README.md) と同directoryにGit保存。
生の全対局履歴・固定source/bin・巨大targetは `work/stage3/` にローカル保存し、pushには含めない。
別環境での再開は、Git保存した累積patchと開始コミットから再構成できる。

- 開始版: Rust release **961成功・ignore 2**。28条件のV4は通常8＋上限13＝21勝、V200は通常6＋上限1＝7勝。
- S1: 全28条件で**action_history本文全体・勝敗・終了turn・metadataが一致**。
- 修正版S2 (`s2-reserved`): 28局正常完走。履歴7/28・結果20/28・turn15/28一致。
  V4は通常12＋上限8＝**20勝**、V200は通常6＋上限2＝8勝。
  勝局平均43.571→41.000T、通常勝利だけの平均33.125→35.000T。
  **総勝利21→20を改善と表現しない。** 通常勝利8→12、通常敗戦6→6とのトレードオフとして
  主担当は後続比較へ進めた。統計的な非劣後は未証明で、必要なら採否を再評価する。
  2得3失の条件と最初の行動差分は `compare-s2-reserved.json` に全件記録。
- 修正版S2のgolden同条件の短期3シナリオは、開始版と行動履歴本文全体が一致。
- 現在の累積候補の全Rust release検証: **967成功・3失敗・ignore 2**。
  engine単体917件、決定性5件、配備ID・Reserve・DAG・役割/予約範囲等の契約は成功。
  失敗はgolden suiteの次の3件だけ。**goldenを更新していない。**
- 現在sourceでClippy全target/全feature、engine WASM、fmt、diff checkは全て成功。
- レビューは主担当の観点別再読。**独立agent/humanレビューは未実施。**

### 未解決のgolden 3件

| ケース | 最初の差分 |
|---|---|
| v4_small_map1 | T3 P1、Capture(3,7) → Wait(5,5) |
| v4_small_map1_save_load | 同上 |
| v3_vs_v4_map6 | T3 P2、Capture(21,8) → Wait(21,7) |

段階別短期再生で、小マップ2件は**S3から発生**と特定済み。
T3のCapture件数は4のままで順序・移動先が変わる。S3の4T行動列は現在の最終候補と一致。
継続配備IDの維持による順序変化がコード上の原因候補だが、28局の採否確認は未完了。
混合AI map_6は**S3までは開始版と一致**。最終候補ではT3 Captureが2→0なので、
S4以降のどの段で変わるか切り分ける。単なるログ順と決めつけないこと。
意図的変更と確認できないままgolden更新・テスト削除で通さない。

## 不採用・無効な旧候補

- 旧S2: can_capture能力だけでAttack歩兵までCapture計上。V4が21→16勝となり不採用。
  旧S2を基にした旧S3〜S8も根拠にしない。
- `s2-role` 版: 任務分類は修正したが、実所属一覧を生産予約へ流用して予約範囲を拡大。
  小マップT2のInfantry→Tank変化を確認し不採用。`*-reserved` 版で修正した。
- 初回S4/S5のbin: Cargoが別snapshot成果物を誤再利用したため無効。
  `*-reused-invalid` と履歴はローカルに残すが、比較根拠にしない。
- Reserve変更の非V4への波及と旧fixtureのstore迂回も検出し修正済み。関連契約は成功。
- remoteの旧aed backupは既存API削除やDrop/hold条件変更を含み、取り込んでいない。

## 次の再開手順

1. `git status --short --branch` とこのHANDOFF、checkpoint報告を読む。
   今のコードはS8候補であり、S2受入版そのものではない。
2. 指定条件は **Hex、map_1/2/6/25/32/42/10、seed1/2、両手番、V4対V200、50T、全体 `--jobs 8`**。
   同時に複数sweepを8並列ずつ走らせない。
3. `s3-reserved` を `s2-reserved` と全28条件で比較し直す。
   ローカルには8局の部分結果があるが、全28条件の受入には使わない。
   比較は保存した `compare_full.py` で本文全体・条件・エラー・勝敗・turnを確認する。
4. S3の採否をレビュー後、S4→S8を一段ずつ直前採用版と比較する。
   各段に同じ短期3シナリオ（下記報告）を挟み、混合AIの最初の差分を特定する。
   明確な劣後や役割不整合なら修正・再baseし、旧候補の後続を自動採用しない。
5. golden差分の原因・意図・対戦影響を判断する。意図に反するならコード修正、
   意図的変更と確認できた場合だけ理由付きで期待値を更新し、全Rust検証を完了する。
6. 最終fmt・Clippy・WASM・diff check、独立レビュー、設計書・対戦報告を更新する。
   追加commit/pushは今回ユーザーがcheckpoint分を明示許可した。次回分の指示も確認する。

### ローカル環境が残っている場合

`source /workspace/.tools/openwars-env.sh` でRust1.99.0等を有効化。
`work/stage3/source-s2-reserved`〜`source-s8-reserved` と同名binが固定済み。
S2〜S8全てsource前後SHA・Cargo JSONのengine/server `fresh=false`・実executableを確認してビルドした。
S1は実コンパイルlog、固定patch/部分source、bin SHAで記録している。

最終テスト専用targetは `work/stage3/final-target`。
`final-reserved-checks.json` はtestのexit101と、他4項目のexit0を正直に保持する。
旧PROGRESSのsession/PIDは履歴であり、稼働中と解釈しない。再起動waiterや古いevalをそのまま実行しない。
固定sourceをbuild中に変更せず、別snapshotの古いmtimeによるCargo再利用を避けるため
entry sourceをcontent不変でtouchし、実コンパイルと取得元を記録する。

D5、占有モデル統一、施設再予測/交換条件強化、map/seed固有hack・プリセット化・面積gate撤去は範囲外。
Webはpnpmのみ。ユーザー成果物を削除せず、reset/clean/一括addをしない。
