# Change: 合流機能の実装 (Unit Merge)

## Why
同じ種類の味方ユニット同士を合流させ、HPや残弾・燃料を統合することで、消耗したユニットを効果的に再活用し前線を維持する戦略的選択肢を提供するため。

## What Changes
- 同じ種類の味方ユニットが重なった場合に「合流 (Merge)」アクションを実行できるようにする
- 合流時、両ユニットのHP（内部値）を加算し、最大値(100)でキャップする（超過分による資金の還元はしない）
- 燃料と弾薬を加算（各最大値でキャップ）する
- 移動元のユニットは消滅し、合流先のユニットは行動済み(`action_completed = true`)となる

## Impact
- Affected specs: `unit-merge` (新設)
- Affected code: `core/src/systems/action_resolution.rs` 等の統合処理、およびUIのアクションメニュー
