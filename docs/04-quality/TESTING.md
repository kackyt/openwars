# TESTING.md - テスト戦略ガイド

## 1. テスト戦略概要

### テストピラミッド

```text
         /\
        /E2E\        (GUI/CLI自動操作・手動テスト)
       /------\
      /統合テスト\    (複数システムの連動テスト)
     /----------\
    /単体テスト\      (ドメイン関数の高速フィードバック)
   /--------------\
```

### カバレッジ目標

| テスト種別 | 対象 | カバレッジ目標 | 優先度 |
| --- | --- | --- | --- |
| 単体（Unit）テスト | `engine`クレートのドメインロジック、ユーティリティ | 80%以上 | 高 |
| 統合（Integration）テスト | 複数Systemをまたぐゲームの進行や勝利判定 | 60%以上 | 中 |
| E2Eテスト | 実際のUIを通した操作 | N/A (自動化困難な場合は手動検証を優先) | 低 |

## 2. ユニットテスト (Unit Test)

`engine`クレート内のドメインロジックや値オブジェクトの振る舞いを中心にテストします。UIクレート（`cli`, `gui`）の単体テストは優先度が低いです。

### 実装パターン

```rust
// 例: ヒットポイント計算のテスト
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hit_point_damage() {
        let mut hp = HitPoint::new(10);
        hp.damage(3);
        assert_eq!(hp.current, 7);
        
        // 0未満にならないことの確認
        hp.damage(10);
        assert_eq!(hp.current, 0);
    }
}
```

## 3. 統合テスト (Integration Test) / ECSテスト

Bevy ECSを利用している場合、単一のシステムが正しくコンポーネントを更新するか、または複数のシステムが連動して正しく動作するかをテストします。

### ECSシステムのテストパターン

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use bevy_ecs::prelude::*;

    #[test]
    fn test_move_unit_system() {
        // Arrange: WorldとScheduleのセットアップ
        let mut world = World::new();
        let mut schedule = Schedule::default();
        schedule.add_systems(move_unit_system);

        // テスト用エンティティの生成
        let unit_entity = world.spawn((
            GridPosition { x: 0, y: 0 },
            ActionPoints { current: 3, max: 3 },
        )).id();

        // 疑似的なコマンド/イベントの発行
        world.insert_resource(Events::<MoveCommand>::default());
        let mut events = world.resource_mut::<Events<MoveCommand>>();
        events.send(MoveCommand {
            entity: unit_entity,
            destination: GridPosition { x: 1, y: 1 },
        });

        // Act: システムの実行
        schedule.run(&mut world);

        // Assert: 状態の検証
        let pos = world.get::<GridPosition>(unit_entity).unwrap();
        assert_eq!(pos.x, 1);
        assert_eq!(pos.y, 1);
        
        let ap = world.get::<ActionPoints>(unit_entity).unwrap();
        assert_eq!(ap.current, 2); // 移動でAPが1減ったと仮定
    }
}
```

## 4. モックとスタブ

依存関係（例: リポジトリ）を外部から注入する設計（DI）をとっている場合、テスト用のモック実装を用意します。

```rust
#[cfg(test)]
mod tests {
    use super::*;

    // モックリポジトリの実装
    struct MockUnitRepository {
        units: std::collections::HashMap<UnitId, Unit>,
    }

    impl UnitRepository for MockUnitRepository {
        fn find_by_id(&self, id: &UnitId) -> Result<Option<Unit>, DomainError> {
            Ok(self.units.get(id).cloned())
        }
        // ...
    }
}
```

## 5. 自動化とCI/CD
GitHub ActionsなどのCI環境を用いて、プルリクエスト作成時やメインブランチへのプッシュ時に自動でテストを実行します。

- `cargo test --all-features --workspace`
- Clippyによる静的解析の併用
