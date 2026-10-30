#!/bin/bash
# conc18 の実行設定（環境変数）で、基準ログ12局から分岐シミュレーションを回す。
# 使い方: run_conc18_branch.sh <テストバイナリ> <出力名> <分岐ラウンド> <進めるラウンド数> [run_branch_sim.py の追加引数...]
# 出力は .tmp/gun/<出力名>/seedN.json。基準ログは LOGS（既定 .tmp/handoff-conc18/map_20_V200-V4_r0）。
cd "$(dirname "$0")/../../.."
LOGS=${LOGS:-.tmp/handoff-conc18/map_20_V200-V4_r0}
ENVS=""; for e in CONCENTRATE HOLD ROUTE_DEFENSE ARTILLERY_FERRY POSITIONAL ROUTE_CAPTURE_ALL ROUTE_BUDGET_TURNS; do ENVS="$ENVS --env OPENWARS_$e=1"; done
mkdir -p .tmp/gun
python3 scripts/run_branch_sim.py --bin "$1" --logs "$LOGS" --from "$3" --turns "$4" --jobs 4 --out-dir ".tmp/gun/$2" $ENVS "${@:5}"
