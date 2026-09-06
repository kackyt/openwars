# ログ分析・敗因診断ガイド（Log Analysis Guide）

本ドキュメントは、`reports/<run_name>/seed*.json` や対戦ログ `battle.jsonl` を深掘り分析し、AIの行動差分と敗因を客観的に特定するための手順書です。

---

## 1. ログファイルの種類と役割

1. **`reports/<run_name>/seed*.json`**:
   - `eval_matchup.py` が出力するベンチマーク完全記録。
   - `results[0]['action_counts']`: プレイヤーごとの兵種別生産総数、移動・攻撃・占領コマンド回数。
   - `results[0]['metrics']`: ターンごとのZOC面積、収入、拠点数、NPV、主観スコアの時系列推移。
   - `results[0]['initial_state']` / `final_state`: 開幕と決着時の盤面配置。
2. **`reports/<run_name>/seed*.md`**:
   - 人間可読なサマリーレポート。合否判定、生産内訳、平均思考時間。
3. **`battle.jsonl` / traceログ**:
   - 1手番1行の低レベルイベントログ。

---

## 2. 差分比較・集計の標準手順

Pythonのワンライナーまたは集計スクリプトで、過去の安定版（Baseline）と変更後（New）を比較します。

### 2.1 勝敗・ZOC・収入のサマリー比較
```bash
python -X utf8 -c "
import json

def load(run, s):
    d = json.load(open(f'reports/{run}/seed{s}.json', encoding='utf-8'))
    g = d['results'][0]
    return g['result'], g['turns'], g['metrics'][-1]

base_run = 'baseline_map32'
new_run = 'new_run_map32'

print('| Seed | Baseline | New | 判定 |')
print('|:---|:---|:---|:---|')
for s in range(1, 13):
    br, bt, bm = load(base_run, s)
    nr, nt, nm = load(new_run, s)
    p2_b = bm['p2_obj']
    p2_n = nm['p2_obj']
    print(f'| {s} | {br} ({bt}T, ZOC:{p2_b[\"zoc_area\"]}) | {nr} ({nt}T, ZOC:{p2_n[\"zoc_area\"]}) |')
"
```

### 2.2 生産内訳の比較
特定兵種の極端な偏り（例: 歩兵過多による前線崩壊、高額兵種の買いすぎによる数的不利）を可視化します。

```bash
python -X utf8 -c "
import json
for run in ['baseline_map32', 'new_run_map32']:
    counts = {}
    for s in range(1, 13):
        d = json.load(open(f'reports/{run}/seed{s}.json', encoding='utf-8'))
        p2_acts = d['results'][0]['action_counts'].get('2', {})
        for k, v in p2_acts.items():
            counts[k] = counts.get(k, 0) + v
    print(run, counts)
"
```

---

## 3. 典型的な敗因パターンと診断チェックポイント

### パターンA: 施設配置・ロジスティクスの逆転（出撃渋滞）
- **現象**: 鈍足な歩兵が後方・袋小路の工場から出撃し、前線の高速ユニットや味方と衝突して前線到達が1〜2ターン遅れる。
- **確認方法**: `OPENWARS_ROLLING_AUDIT=1 cargo test ...` で初手（T1）の `purchases` における工場座標 `(x, y)` と役割を確認。
- **根本原因**: 工場割当のコスト関数で、前線工場が高速ユニット（装甲車等）に奪われ、歩兵が後方に押し出されている。

### パターンB: 占領役と戦闘役のトレードオフ破綻（過剰な壁/過剰な火力）
- **現象**:
  - 安価な壁歩兵を戦闘役に混ぜた結果、評価関数が「安いこと」を優先して快速スクリーンや迎撃火力を壁歩兵に置き換えてしまう。
  - 逆に高額兵種ばかり選んで自陣拠点の同時占領が疎かになり、経済基盤で大差をつけられる。
- **確認方法**: T1〜T3 のターン収入推移、および自陣都市（中立拠点）の占領完了ターン。

### パターンC: 戦力集中と前線維持の破綻
- **現象**: 敵の第一波突撃に対して、HP削りだけで撃破できず、反撃で大損害を受けて自軍前線が壊滅する。
- **確認方法**: `surviving_combat_value`（生存戦闘価値）および `combat_value_dealt / combat_value_received`（戦闘ROI）の推移。
