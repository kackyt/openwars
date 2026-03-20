# 実装タスク

- [x] `engine/Cargo.toml` に `serde` （`derive`機能付き）および `csv` の依存関係を追加する。
- [x] `landscape.csv`、`unit.csv`、`weapon.csv`、`movement.csv`、`load.csv` の各カラムに対応するRust構造体を定義する。
- [x] `map/` 以下のCSVファイルを「素の数値の2次元配列」としてパースする処理を作成する。
- [x] パースしたマップセルの数値を `(PlayerID, TerrainID)` にデコードするユーティリティまたは構造体を実装する（`PlayerID = value / 100`, `TerrainID = value % 100`）。
- [x] パースしたCSVデータおよびマップデータのマップ/参照を管理する `MasterDataRegistry` を実装する。
- [x] エンジン初期化時にCSVを読み込んでレジストリを構築する `load_master_data()` 関数を実装する。
- [x] エンジンのプラグインセットアップ時に、`MasterDataRegistry` をBevyの `Resource` として登録する。
- [x] 既存のすべてのCSVファイルとマップデータが正常にパースおよびデコードされ、想定通りの所有者・地形を返すことを検証する単体テストを追加する。
