# 段階的な対戦検証

## 同じものを比較する

実行前に、HEADと差分、必要な未追跡ソース、使用バイナリの識別子、マップ、グリッド、P1/P2、相手、seed、ターン上限を記録する。作業中の基準をソースのコピー/パッチや隔離した作業場所に保持し、候補の反証後に戻れるようにする。既存のユーザー変更は保持する。

`scripts/eval_matchup.py` はreleaseの `mcp-server` を起動するため、実行コードの変更後は `cargo build --release -p mcp-server` の成功を確認する。古いバイナリの結果を新しいソースの結果として報告しない。ビルドとベンチマークの起動は依存順に行う。

この評価スクリプトは起動時に既存のmcp-serverを終了する処理を持つ。**対戦ベンチマークを並列実行しない。** ユーザーが操作中の対局にも影響するので、同じサーバーを使う実行がないことを確認する。

## 安い反証から進める

局面・境界テストで原因を再現した後、代表seedの必要な区間だけ実行し、計画・候補・コマンドの予測差を確認する。狙った行動が変わらない案やルール違反がある案は、この段階で原因を調べ直す。毎回の全対戦を診断の代わりにしない。

短い試験は初撃や占領などの仕組みの検証用。短縮した上限での勝率を、本来の上限の基準と比較しない。仕組みが変わった採用候補を、通常はseed 1〜12の同条件比較へ進める。先後反転や別マップは、その変更が及ぶ範囲とユーザーの目的に応じて追加し、未検証範囲を明記する。

P1を常に改善対象としない。後攻の改善ならP2を維持する。30ターンなどを固定の成功条件にせず、既存基準・指定ログ・ユーザー要件に合わせて上限を選ぶ。

## 実行例（PowerShell）

以下は、既存の基準がMap32・P2=V4・60ターンである場合の例。値は実験条件へ置き換える。`$seedList` を代表seedだけにすれば局所検証にも使える。

```powershell
$runDir = 'reports/<new-run>'
$seedList = 1..12
if (Test-Path -LiteralPath $runDir) { throw '既存の計測結果を上書きしない' }
New-Item -ItemType Directory -Path $runDir | Out-Null
cargo build --release -p mcp-server
if ($LASTEXITCODE -ne 0) { throw 'release build failed' }
foreach ($seed in $seedList) {
    python scripts/eval_matchup.py --mode batch --map map_32 --p1 V200 --p2 V4 --player-order as-given --seed $seed --games 1 --max-turns 60 --grid-type hex --output "$runDir/seed$seed.md" --json-output "$runDir/seed$seed.json" *> "$runDir/seed$seed.log"
    if ($LASTEXITCODE -ne 0) { throw "seed $seed failed; inspect its log" }
}
```

エラー・タイムアウト・欠損seedを敗北やゼロ指標へ置き換えない。比較可能なデータが揃うまで集計結果を確定しない。ログは全量を会話へ貼らず、問題のある区間を抜き出す。

## 比較の読み方

付属 `scripts/compare_benchmarks.py` は各seedファイルに一対局ある結果を扱う。複数マップ・先後両方を一ファイルへ詰めた結果は、対戦条件別に分けてから使う。

```text
python -X utf8 <skill-dir>/scripts/compare_benchmarks.py <baseline-dir> <candidate-dir> --subject V4 --output <local-report.md>
```

- 通常の勝敗、ターン上限判定の勝敗、引き分けを区別する。`P2_Win` の部分一致で `P2_Win_MaxTurns` を通常勝利に含めない。
- 欠損seed、ゲームエラー、手番・対戦条件の不一致は比較エラー。同じ版同士なら `--side` で対象を明示する。
- 終局時のZOC・収入は勝敗と対局長の影響を受けるため、それだけで「微改善」と自動判定しない。`--turn` を指定すると、そのターンの実測指標を比較できる。終局済みで観測がなければ `N/A` とする。
- 生産総数には対局長を併記する。思考時間は記録された計測単位を保ち、固定の100〜300msなどを無根拠に正常範囲としない。
- 同時要件は、[ログ分析ガイド](log-analysis-guide.md)に従って同じ対局・時点で集計する。各前線の別々の最大値では証明できない。
- 修正後に基準で勝ったseedが負けた場合は隠さない。ただし個別の退行だけで総合改善まで否定せず、ユーザーの厳守条件と改善目的に照らして採否を説明する。

## 最終状態の保存

採用したソースと計測時のソースが一致することを確認し、その最終状態でプロジェクト所定のテスト・fmt・Clippyを通す。ロールバック前の結果と復元後の状態を混同しない。既に検証済みの状態への復元なら、同一性を確認して既存の証拠を使える。

検証レポート、計測JSON/JSONL、試案アーカイブはローカル成果物として保存する。ユーザーが別途指定しない限りコードコミットには含めない。再現fixtureは必要な局面に絞り、コードの回帰テストとして管理する。
