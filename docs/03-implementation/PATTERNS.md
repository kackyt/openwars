# PATTERNS.md - 実装パターンガイド

## 1. デザインパターン（Rust & ECS）

### Newtype パターン（値オブジェクト）
Rustのタプル構造体を利用し、型安全性を担保します。プリミティブ型の直接利用（`i32` や `String`）を避けることで、意図しない値の混入を防ぎます。

```rust
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
```

### ECS (Entity Component System) パターン
Bevy ECSを用いたデータとロジックの分離パターンです。

- **Component**: 純粋なデータのみを保持します。（例：`GridPosition`, `Health`）
- **System**: 状態の更新やルールの適用を行う関数です。（例：移動ロジック、戦闘解決）
- **Resource**: ゲーム全体で共有されるデータです。（例：マスターデータ、現在ターン）
- **Event**: 状態変化を非同期に他システムやUI層へ通知します。

### DI（依存性の注入）パターン
アプリケーション層がインフラ層に依存しないよう、ドメイン層で定義した trait を使って依存性を注入します。
基本的には静的ディスパッチ（ジェネリクス）を推奨しますが、モックの切り替えなどで動的ディスパッチを利用する場合もあります。

```rust
// リポジトリのインターフェース
pub trait UnitRepository: Send + Sync {
    fn find_by_id(&self, id: &UnitId) -> Result<Option<Unit>, DomainError>;
    fn save(&self, unit: &Unit) -> Result<(), DomainError>;
}

// ユースケースでのジェネリクスによるDI
pub struct MoveUnitUseCase<R: UnitRepository> {
    unit_repo: R,
}

impl<R: UnitRepository> MoveUnitUseCase<R> {
    pub fn new(unit_repo: R) -> Self { Self { unit_repo } }

    pub fn execute(&self, unit_id: UnitId, dest: Position) -> Result<(), AppError> {
        let mut unit = self.unit_repo.find_by_id(&unit_id)?.unwrap();
        unit.move_to(dest)?;
        self.unit_repo.save(&unit)?;
        Ok(())
    }
}
```

## 2. エラーハンドリングパターン

`thiserror` と `anyhow` を効果的に使い分けます。

### ドメイン層・インフラ層（thiserror）
明確なエラーの型と理由を提供します。

```rust
use thiserror::Error;

#[derive(Error, Debug)]
pub enum DomainError {
    #[error("Unit not found: {0}")]
    UnitNotFound(UnitId),
    #[error("Not enough action points. Required: {required}, Available: {available}")]
    NotEnoughAp { required: u32, available: u32 },
}
```

### プレゼンテーション層・アプリケーション層（anyhow）
エラーを集約し、コンテキスト情報を付与します。

```rust
use anyhow::{Context, Result};

fn handle_user_input(input: &str) -> Result<()> {
    let command = parse_command(input).context("Failed to parse user input")?;
    engine::execute_command(command).context("Engine failed to execute command")?;
    Ok(())
}
```

## 3. UI連携パターン（イベント駆動）

エンジン内の状態変更を直接UIが読み取る（密結合）のではなく、イベントを介して通知します。これによりエンジン側はUIの「どう見えるか」を無視できます。

```rust
// engine crate
#[derive(Event)]
pub struct UnitMovedEvent {
    pub entity: Entity,
    pub from: GridPosition,
    pub to: GridPosition,
}

// engineのsystem内でEventWriterを用いて発行
fn move_system(mut events: EventWriter<UnitMovedEvent>, ...) {
    // ...
    events.send(UnitMovedEvent { entity, from, to });
}

// cli crate
// main loopでEventReaderを用いて購読し、アニメーション等を開始
fn render_movement(mut events: EventReader<UnitMovedEvent>) {
    for event in events.read() {
        // UI側の描画処理
    }
}
```

## 4. UI 状態管理パターン（1フレーム遅延の考慮）

CLIの描画フレームワーク（例: ratatui）とECSエンジンを統合する際、入力処理とエンジンのシステム実行の間にフレームのズレが生じます。

**パターン: Wait状態を経由したUIの再描画**
時間やエンジンの複数システム実行を要するコマンドを送った直後に、直接次のメニュー状態へ遷移させるのではなく、一度 `Wait` 状態を経由させます。エンジンからの処理完了イベント（または状態の確定）を受け取ったタイミングで初めて次のUIステートへ移行させます。
