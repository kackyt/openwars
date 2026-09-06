#!/usr/bin/env python3
"""
OpenWars ベンチマーク差分比較スクリプト

2つのベンチマーク結果ディレクトリ（JSONファイル群）を読み込み、
勝敗・ターン数・ZOC・収入・生産内訳の差分サマリーを出力する。

使用例:
    python scripts/compare_benchmarks.py reports/baseline_map32 reports/new_map32 --subject V4
"""

import argparse
import glob
import json
import os
import sys
from collections import defaultdict


def load_seed_result(directory, seed):
    path = os.path.join(directory, f"seed{seed}.json")
    if not os.path.exists(path):
        return None
    try:
        with open(path, "r", encoding="utf-8") as f:
            data = json.load(f)
        results = data.get("results", [])
        if not results:
            return None
        return results[0]
    except Exception as e:
        print(f"Error loading {path}: {e}", file=sys.stderr)
        return None


def main():
    parser = argparse.ArgumentParser(description="OpenWars Benchmark Comparison Tool")
    parser.add_argument("baseline_dir", help="比較元のベンチマークディレクトリ (例: reports/baseline_map32)")
    parser.add_argument("target_dir", help="比較対象のベンチマークディレクトリ (例: reports/new_map32)")
    parser.add_argument("--subject", default="V4", help="評価対象のAIバージョン (デフォルト: V4)")
    parser.add_argument("--seeds", type=int, default=12, help="シード数 (デフォルト: 12)")
    args = parser.parse_args()

    subject = args.subject.upper()

    base_wins = 0
    target_wins = 0
    total_matches = 0

    base_prod = defaultdict(int)
    target_prod = defaultdict(int)

    rows = []

    for seed in range(1, args.seeds + 1):
        base_game = load_seed_result(args.baseline_dir, seed)
        target_game = load_seed_result(args.target_dir, seed)

        if not base_game or not target_game:
            continue

        total_matches += 1

        # プレイヤー特定 (subjectがP1かP2か)
        p1_ver = target_game.get("p1", "").upper()
        p2_ver = target_game.get("p2", "").upper()
        subject_side = "1" if p1_ver == subject else ("2" if p2_ver == subject else None)
        side_key = f"p{subject_side}" if subject_side else "p2"

        # 勝敗判定
        def is_win(game, side):
            res = game.get("result", "")
            return f"P{side}_Win" in res

        b_win = is_win(base_game, subject_side) if subject_side else False
        t_win = is_win(target_game, subject_side) if subject_side else False

        if b_win:
            base_wins += 1
        if t_win:
            target_wins += 1

        # メトリクス取得
        b_metrics = base_game.get("metrics", [])
        t_metrics = target_game.get("metrics", [])
        b_last = b_metrics[-1] if b_metrics else {}
        t_last = t_metrics[-1] if t_metrics else {}

        b_obj = b_last.get(f"{side_key}_obj", {})
        t_obj = t_last.get(f"{side_key}_obj", {})

        b_zoc = b_obj.get("zoc_area", 0)
        t_zoc = t_obj.get("zoc_area", 0)
        b_inc = b_obj.get("income_per_turn", 0)
        t_inc = t_obj.get("income_per_turn", 0)
        b_prop = b_obj.get("owned_properties", 0)
        t_prop = t_obj.get("owned_properties", 0)

        # 生産内訳集計
        for utype, count in base_game.get("action_counts", {}).get(subject_side or "2", {}).items():
            base_prod[utype] += count
        for utype, count in target_game.get("action_counts", {}).get(subject_side or "2", {}).items():
            target_prod[utype] += count

        # 個別シード判定
        if not b_win and t_win:
            eval_tag = "✅ 改善 (逆転勝)"
        elif b_win and not t_win:
            eval_tag = "❌ 悪化 (敗北)"
        elif b_win and t_win:
            eval_tag = "✅ 勝利維持"
        else:
            if t_zoc > b_zoc or t_inc > b_inc:
                eval_tag = "微改善"
            elif t_zoc < b_zoc and t_inc < b_inc:
                eval_tag = "微悪化"
            else:
                eval_tag = "同等"

        b_summary = f"{'WIN' if b_win else 'LOSE'} ({base_game.get('turns')}T) [ZOC:{b_zoc:.0f}, 拠:{b_prop}, 収:{b_inc}]"
        t_summary = f"{'WIN' if t_win else 'LOSE'} ({target_game.get('turns')}T) [ZOC:{t_zoc:.0f}, 拠:{t_prop}, 収:{t_inc}]"

        rows.append((seed, b_summary, t_summary, eval_tag))

    print(f"# 📊 ベンチマーク差分比較レポート ({subject})")
    print(f"- **比較元 (Baseline)**: `{args.baseline_dir}`")
    print(f"- **比較先 (Target)**:   `{args.target_dir}`")
    print(f"- **対象AI手番**: P{subject_side} ({subject})\n")

    base_pct = (base_wins / total_matches * 100) if total_matches else 0
    target_pct = (target_wins / total_matches * 100) if total_matches else 0
    diff_pct = target_pct - base_pct

    diff_sign = "+" if diff_pct >= 0 else ""
    print(f"## 🏆 総合勝率サマリー")
    print(f"- **Baseline 勝率**: {base_pct:.1f}% ({base_wins}/{total_matches} 勝)")
    print(f"- **Target   勝率**: {target_pct:.1f}% ({target_wins}/{total_matches} 勝)")
    print(f"- **勝率差分**:     **{diff_sign}{diff_pct:.1f}%**\n")

    print("## 📍 シード別詳細対戦結果")
    print("| Seed | Baseline (結果/T/ZOC/拠点/収入) | Target (結果/T/ZOC/拠点/収入) | 判定 |")
    print("|:---|:---|:---|:---|")
    for r in rows:
        print(f"| {r[0]} | {r[1]} | {r[2]} | {r[3]} |")
    print()

    print("## 🛠 兵種別生産内訳の差分")
    all_units = sorted(set(list(base_prod.keys()) + list(target_prod.keys())))
    print("| 兵種 | Baseline 総数 | Target 総数 | 差分 |")
    print("|:---|---:|---:|---:|")
    for u in all_units:
        b_c = base_prod.get(u, 0)
        t_c = target_prod.get(u, 0)
        diff = t_c - b_c
        sign = "+" if diff > 0 else ""
        print(f"| {u} | {b_c} | {t_c} | {sign}{diff} |")
    print()


if __name__ == "__main__":
    main()
