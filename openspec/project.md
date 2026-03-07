# Project Context

## Purpose

某ターン制戦略シミュレーションを模したゲームを作ることを目標に据える

## Tech Stack
フロントエンド
- CUI (Rust)
- GUI
  - Tauri (React)

ロジック
- Rust

## Project Conventions

### Code Style
- ソースコードにはロジックの内容がわかるように日本語のコメントをいれること

- 値オブジェクト（Value Object）としての Newtype パターン
Rustのタプル構造体を使って、型安全性を担保します。プリミティブ型（i32やString）の直接利用を避けます。

Rust
// domain/model/unit.rs
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct UnitId(pub uuid::Uuid);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HitPoint {
    current: u32,
    max: u32,
}

impl HitPoint {
    pub fn new(max: u32) -> Self {
        Self { current: max, max }
    }
    pub fn damage(&mut self, amount: u32) {
        self.current = self.current.saturating_sub(amount);
    }
}

- エンティティの参照は「ポインタ」ではなく「ID」で行う
戦略ゲームでは、「マップ上のこの位置に、このユニットがいる」という関係性が発生します。ここで Unit の実体（参照）を Map に持たせると、借用チェッカーとの終わりのない戦いが始まります。必ずIDで関連付けを行ってください。

Rust
// domain/model/map.rs
pub struct HexMap {
    pub id: MapId,
    // &Unit ではなく UnitId を保持する
    cells: HashMap<Position, Option<UnitId>>, 
}

- 依存性の注入（DI）にはジェネリクスか dyn Trait を使う
アプリケーション層がインフラ層に依存しないよう、ドメイン層で定義した trait を使います。

静的ディスパッチ（ジェネリクス）: パフォーマンス重視。コンパイル時間は伸びる。(推奨)

動的ディスパッチ（Arc<dyn Trait> または Box<dyn Trait>）: 記述がシンプルになり、モックの差し替えが容易。

Rust
// domain/repository/unit_repository.rs
pub trait UnitRepository: Send + Sync {
    fn find_by_id(&self, id: &UnitId) -> Result<Option<Unit>, DomainError>;
    fn save(&self, unit: &Unit) -> Result<(), DomainError>;
}

// application/usecase/move_unit.rs
pub struct MoveUnitUseCase<R: UnitRepository> {
    unit_repo: R,
}

impl<R: UnitRepository> MoveUnitUseCase<R> {
    // 依存を注入してユースケースを初期化
    pub fn new(unit_repo: R) -> Self { Self { unit_repo } }

    pub fn execute(&self, unit_id: UnitId, dest: Position) -> Result<(), AppError> {
        // 1. リポジトリから再構築
        let mut unit = self.unit_repo.find_by_id(&unit_id)?.unwrap();
        // 2. ドメインのロジックを実行
        unit.move_to(dest)?;
        // 3. 結果を永続化
        self.unit_repo.save(&unit)?;
        Ok(())
    }
}

- エラーハンドリングに thiserror と anyhow を活用する
ドメイン層・インフラ層: thiserror クレートを使って、型安全で明確なカスタムエラー（DomainError, InfraError）を定義します。

アプリケーション層・プレゼンテーション層: 最終的なエラーの集約として anyhow を使うと、スタックトレースやコンテキストの付与が簡単になります。

### Architecture Patterns
Reactはbullet proof reactでディレクトリは配置

Rustはworkspaceを使ったマルチクレート構成にする
openwars/
├── Cargo.toml               # ワークスペース全体の定義
├── core/                    # 【コア】DDDのDomain, Application, Infra層を含むライブラリ
│   ├── Cargo.toml
│   └── src/
│       ├── domain/          # エンティティ、値オブジェクト、リポジトリのtrait
│       ├── application/     # ユースケース（CLI/GUIから呼ばれる窓口）
│       ├── infrastructure/  # SQLiteやファイル保存などの実装
│       └── lib.rs
├── apps/
│   ├── cli/                 # 【プレゼン層 1】CUIアプリケーション
│   │   ├── Cargo.toml       # coreクレートに依存
│   │   └── src/
│   │       └── main.rs      # clap等を使ったコマンドライン入力の処理
│   └── gui/                 # 【プレゼン層 2】Tauriアプリケーション
│       ├── Cargo.toml       # coreクレートに依存
│       ├── src-tauri/       # Tauriのバックエンド（Rust）
│       │   ├── src/
│       │   │   ├── main.rs  # tauriのセットアップ、DIの注入
│       │   │   └── cmd.rs   # tauri::command群（コントローラーの役割）
│       │   └── tauri.conf.json
│       └── src/             # Tauriのフロントエンド（TypeScript / React / Vue など）

### coreクレートの構成

DDDを意識してモデル、リポジトリ、ユースケースを作る。オニオンアーキテクチャを意識すること

src/
├── domain/                 # 【中心】ドメイン層（外部への依存ゼロ）
│   ├── model/              # エンティティ、値オブジェクト、集約ルート
│   │   ├── game_state.rs   # ゲーム状態
│   │   ├── unit.rs         # ユニット
│   │   ├── map.rs          # マップ
│   │   └── faction.rs      # 勢力
│   ├── repository/         # 永続化や外部アクセスなどのインターフェース（trait）
│   │   ├── game_state_repository.rs
│   │   └── map_repository.rs
│   └── error.rs            # ドメイン固有のエラー定義
│
├── application/            # 【第2層】アプリケーション層（ユースケース）
│   ├── usecase/            # アプリケーションサービス（ドメインを操作する手順）
│   │   └── move_unit.rs    # 例: ユニット移動のユースケース (trait)
│   └── query/              # （必要に応じて）参照専用のDTOやクエリインターフェース (trait)
│
├── infrastructure/         # 【第3層】インフラストラクチャ層（DB、ファイルIO等、マスターデータ管理）
│   ├── persistence/        # repository traitの具体的な実装
│   │   ├── in_memory/      # テスト用のオンメモリ実装
│   │   └── file/           # fileへの保存、読み出し実装 (csvはここに置く)
│   └── external/           # 外部API等のクライアント
└── lib.rs



### Testing Strategy

ロジック単体テストは必ず実装する
GUIは後回し

### Git Workflow
GitHub Flowを採用する

## Domain Context

ゲーム概略
https://gbwn.main.jp/GBWT.htm

マップ一覧
https://gbwn.main.jp/Normal_GBWT.htm


## Important Constraints

## External Dependencies
