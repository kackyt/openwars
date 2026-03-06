# Change: コアゲームロジックの雛形追加

## Why
ターン制戦略シミュレーションゲームを作成するための基盤が必要です。現在のところ `project.md` に全体的な方針が記載されていますが、基礎となるRustのゲームロジックが存在しません。ドメイン駆動設計（DDD）の原則に則った強固なドメインモデルが必要です。

## What Changes
- `GameState`（ゲーム状態）、`Phase`（フェーズ）、`Player`（プレイヤー）のドメインエンティティをセットアップする。
- グリッドベースの戦場を扱うための `Map`（マップ）構造体と、https://gbwn.main.jp/Terrain_GBWs.htm に準拠した13種類の `Terrain`（地形：道路、橋、平地、川、森、山、海、浅瀬、都市、工場、空港、港、首都）構造体を定義する。
- https://gbwn.main.jp/Unit_GBWs.htm に準拠した24種類のユニット一覧（歩兵〜潜水艦）を列挙型または構造体として定義し、各種ステータス（HPは小数点を考慮し10倍した値を保持する等、移動力等の基礎）を実装する。
- ダメージ計算の基礎として、https://gbwn.main.jp/Damage_GBWs.htm に準拠したユニット間ダメージ相性表をCSVなどのマスターデータから読み込める仕組みを構築する。
- これらのモデルに対する初期の単体テストを作成する。

## Impact
- Affected specs: `game-state`, `map-grid`, `unit-roster`
- Affected code: `src/` (ゲームロジック用の新しいRustモジュール)
