# VALIDATION.md - 検証・品質保証ガイド

## 1. 検証戦略

本プロジェクトでは、不正な状態を持つゲームデータ（無効な座標、マイナスのHPなど）が発生しないよう、レイヤーごとに検証（バリデーション）を行います。

| レベル | 対象 | 検証方法 | タイミング |
| --- | --- | --- | --- |
| L1: 入力検証 | UI入力（キーボード、マウスクリック） | CLIクレート内での境界チェック | リアルタイム |
| L2: コマンド検証 | UIからエンジンへの指示 | エンジン側のユースケース/システムでの実行可否判定 | 処理実行前 |
| L3: ドメイン検証 | ゲーム状態（HP、座標等） | 型（値オブジェクト）のコンストラクタ | インスタンス生成・変更時 |

## 2. ドメイン検証 (値オブジェクト)

Rustの型システムを利用し、不正な状態をコンパイルレベルやインスタンス生成時に弾く設計（Newtypeパターン等）を推奨します。

```rust
// 例: HPはマイナスにならないことを保証する
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HitPoint {
    current: u32,
    max: u32,
}

impl HitPoint {
    // コンストラクタで不変条件を保証
    pub fn new(max: u32) -> Self {
        Self { current: max, max }
    }

    pub fn current(&self) -> u32 {
        self.current
    }

    pub fn damage(&mut self, amount: u32) {
        // saturating_sub により 0未満にならないことを保証
        self.current = self.current.saturating_sub(amount);
    }
}
```

## 3. コマンド検証（ビジネスルール検証）

UIからエンジンへ送られたコマンド（例: 「ユニットAを座標(3, 4)へ移動せよ」）は、エンジン（System）内で実行可能か検証されます。

```rust
pub fn validate_move(
    unit: &Unit,
    target_pos: GridPosition,
    map: &HexMap,
) -> Result<(), DomainError> {
    // 1. 移動力が残っているか
    if unit.action_points == 0 {
        return Err(DomainError::NotEnoughAp);
    }

    // 2. 移動先がマップ内か
    if !map.contains(target_pos) {
        return Err(DomainError::OutOfBounds);
    }

    // 3. 移動先に他のユニットがいないか
    if map.has_unit_at(target_pos) {
        return Err(DomainError::CellOccupied);
    }

    Ok(())
}
```

## 4. UI状態とエンジンの同期検証

非同期・イベント駆動で描画を行うCLI・GUIにおいては、UI側が保持する「ユーザーが認識している状態」と「エンジンの実際の状態」に乖離が生まれるリスク（同期ズレ）があります。
これを防ぐため、以下の原則を守ります：

1. **Source of Truth**: ゲームの真の状態は必ず `engine` にのみ存在させる。
2. **UI状態の揮発性**: UI側（`cli`）で持つ状態（カーソル位置、メニューの選択項目）はゲームの進行に影響を与えない一時的なものに留める。
3. **Wait状態による確実な同期**: エンジンにコマンドを発行した後はUIをWait状態とし、エンジンから完了のEventを受け取ってからUIを再描画する。
