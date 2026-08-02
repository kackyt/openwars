## Why

空母に搭載した航空ユニットは盤外座標へ移動するため、既存の拠点補給を受けられません。さらに損傷輸送ユニットへの常時HP同期と日次燃料処理により、損傷した空母が搭載機を回復する移動補給拠点として機能しません。

## What Changes

- 自軍ターン開始時に、生存している空母が搭載ユニットを最大20内部HP修理します。
- 修理費は実回復HPに応じて課金し、資金不足時は購入可能な範囲で部分修理します。
- 空母サービスでは燃料・弾薬を無償で最大化します。
- 搭載中の航空ユニットは日次燃料消費・燃料切れ墜落の対象外にします。
- 輸送ユニットHP同期を新規被弾時だけに限定します。

## Capabilities

### Modified Capabilities

- `unit-supply`: 空母の搭載ユニットへのターン開始時自動サービスを追加します。
- `unit-loading`: 搭載HP同期を被弾イベント駆動へ変更します。
- `engine-fuel-consumption`: 搭載航空ユニットの日次処理除外を追加します。

## Impact

- `engine/src/systems/turn_management.rs`
- `engine/src/systems/transport.rs`
- UI、WASM API、セーブ形式、マスターデータは変更しません。
