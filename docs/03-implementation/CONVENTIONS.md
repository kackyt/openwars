# CONVENTIONS.md - コーディング規約

## 0. ドキュメント命名規則

このプロジェクトでは、AIツールが効率的に理解できるよう、統一されたドキュメント命名規則を採用しています。

### ディレクトリ構造

```
docs/
├── 00-planning/              # 数字-英語小文字（ハイフン区切り）
├── 01-context/
├── 02-design/
├── 03-implementation/
├── 04-quality/
├── 05-operations/
├── 06-reference/
├── 07-project-management/
└── MASTER.md                  # トップレベルは大文字
```

### ディレクトリ命名規則
- **形式**: `数字-英語小文字（ハイフン区切り）`
- **目的**: AIツールが順序を理解しやすい

### ファイル命名規則
- **メインドキュメント**: `英語大文字.md` (例: `MASTER.md`, `ARCHITECTURE.md`)
- **特殊ファイル（例外）**: `README.md`, `CLAUDE.md`, `AGENTS.md`, `.cursorrules` など

---

## 1. 一般原則

### 基本理念
- **可読性優先**: コードは書く時間より読む時間の方が長い
- **一貫性重視**: プロジェクト全体で統一されたスタイル（`cargo fmt`の利用）
- **日本語コメント**: ロジックの内容がわかるように日本語のコメントを入れること
- **明示性**: 暗黙の型変換や動作より明示的に記述する

### マジックナンバー禁止
設定値や固定のパラメータは定数（`const`）として定義し、意図を明確にする。

## 2. ファイル・ディレクトリ構成

### ワークスペース構成
本プロジェクトはCargoのワークスペース機能を用いたマルチクレート構成です。
```
openwars/
├── engine/              # コアロジック（ECSベース）
│   ├── src/
│   │   ├── components/  # ECSコンポーネント
│   │   ├── systems/     # ECSシステム
│   │   ├── events/      # イベント定義
│   │   ├── resources/   # リソース定義
│   │   └── lib.rs
├── cli/                 # CUIフロントエンド
│   └── src/
│       └── main.rs
└── gui/                 # GUIフロントエンド (将来)
```

## 3. Rust コーディング規約

### 命名規則
Rustの標準的な命名規則（RFC 430）に従います。

- **クレート/モジュール**: `snake_case`
- **型/構造体/列挙型/トレイト**: `UpperCamelCase`
- **関数/メソッド/変数**: `snake_case`
- **定数/静的変数**: `SCREAMING_SNAKE_CASE`

### フォーマットとLint
コードをコミットする前に必ず以下のコマンドがエラーなく通ることを確認します。

- `cargo fmt --all -- --check`
- `cargo clippy --all-targets --all-features -- -D warnings`

### エラーハンドリング
ドメイン層（`engine`）とアプリケーション層（`cli`, `gui`）でエラーハンドリングの手法を分けます。

- **`engine` クレート**: `thiserror` を用いて、型安全で明確なカスタムエラー（例：`DomainError`）を定義します。
- **`cli`/`gui` クレート**: `anyhow` を用いて、エラーを集約・伝播し、スタックトレースやコンテキストを付与します。

### メモリとライフサイクル
- 戦略ゲームではユニットの数が多くなるため、無駄なコピーや `clone` は避ける。
- 解放タイミングが不明瞭な `Box` を無闇に定義しない。
- エンティティの参照は「ポインタ（参照）」ではなく「ID（値オブジェクト）」で行う。（借用チェッカー対策）

## 4. Git規約

### ワークフロー
GitHub Flowを採用します。
- `main` ブランチを保護し、常にデプロイ可能な状態に保つ。
- 機能追加・修正は `feature/*` または `bugfix/*` 等のブランチを作成してPull Requestを行う。

### コミットメッセージ
変更の意図が明確に伝わるように記述します。
例： `feat: ユニット移動システムの実装`, `fix: 戦闘ダメージ計算のバグ修正`

## 5. テスト規約

### テスト戦略
- **ロジック単体テスト**: 必須。`engine` クレートのシステムや純粋なドメイン関数に対するテストは必ず実装する。
- **GUI/UIテスト**: 後回しでよい。プレゼンテーション層のテストよりコアルールの網羅性を優先する。

### テストコードの配置
Rustの標準に従い、ユニットテストは対象ファイル内の `#[cfg(test)]` モジュールに記述し、統合テストは `tests/` ディレクトリに配置します。

---

## 6. TypeScript / React コーディング規約

### 技術スタック

| カテゴリ | 技術 | バージョン | 備考 |
| --- | --- | --- | --- |
| ビルドツール | Vite | ^6.x | SWCプラグイン使用 |
| UIフレームワーク | React | ^18.x | 関数コンポーネント + Hooks |
| 状態管理 | Zustand | ^5.x | グローバルストア |
| UIライブラリ | Mantine | ^7.x | ダークモードデフォルト |
| 2D描画 | PixiJS + @pixi/react | ^7.x | ゲームキャンバス描画 |
| CSS-in-TS | vanilla-extract | ^1.x | ゼロランタイムCSS |
| Worker通信 | Comlink | ^4.x | WASMエンジンとの通信 |
| リンター/フォーマッター | Biome | ^2.x | ESLint/Prettierの代替 |
| パッケージマネージャー | pnpm | latest | **`npm` / `yarn` の使用は厳禁** |
| 言語 | TypeScript | ~5.6 | `strict: true` 必須 |

### 一般原則

- **可読性優先**: コードは書く時間より読む時間の方が長い
- **一貫性重視**: プロジェクト全体で統一されたスタイル（Biomeの利用）
- **日本語コメント**: ロジックの内容がわかるように日本語のコメントを入れること
- **明示性**: 暗黙の型変換や動作より明示的に記述する

### 命名規則

| 種類 | パターン | 例 |
| --- | --- | --- |
| コンポーネント | `PascalCase` | `GameCanvas`, `TurnIndicator` |
| 関数・変数 | `camelCase` | `handleClick`, `isReady` |
| 定数 | `UPPER_SNAKE_CASE` | `MAX_ITEMS_PER_PAGE`, `DEFAULT_MAP_NAME` |
| 型・インターフェース | `PascalCase` | `UnitData`, `GameState` |
| ストアHook | `use` + `PascalCase` + `Store` | `useGameStore` |
| カスタムHook | `use` + `PascalCase` | `useEngine`, `useHexLayout` |
| vanilla-extractスタイル | `camelCase` | `container`, `headerWrapper` |
| ファイル名（コンポーネント） | `PascalCase.tsx` | `GameCanvas.tsx`, `ActionMenu.tsx` |
| ファイル名（ユーティリティ） | `camelCase.ts` | `dateHelpers.ts`, `hexMath.ts` |
| ファイル名（スタイル） | `index.css.ts` | `index.css.ts`（コンポーネントディレクトリ内） |
| ファイル名（ストア） | `camelCase.ts` | `gameStore.ts` |
| ファイル名（Worker） | `camelCase.ts` | `engineWorker.ts` |
| ディレクトリ名（コンポーネント） | `PascalCase` | `GameCanvas/`, `ActionMenu/` |
| ディレクトリ名（その他） | `camelCase` | `store/`, `worker/`, `wasm/` |

### パッケージマネージャー

```bash
# ✅ 正しい例
pnpm install
pnpm add react
pnpm dev
pnpm build

# ❌ 禁止（使用厳禁）
npm install
yarn add react
```

> **重要**: `npm` や `yarn` の使用はいかなる場合も禁止です。CI/CDパイプラインやドキュメント内のコマンド例も含めて、すべて `pnpm` を使用してください。

## 7. ファイル・ディレクトリ構成

### Webアプリケーション構成

```
web/
├── biome.jsonc           # Biome設定（リンター・フォーマッター）
├── package.json
├── tsconfig.json
├── vite.config.ts
├── index.html
├── public/               # 静的アセット
└── src/
    ├── main.tsx          # エントリーポイント
    ├── App.tsx           # ルートコンポーネント
    ├── vite-env.d.ts     # Vite型定義
    │
    ├── components/       # UIコンポーネント
    │   ├── game/         # ゲーム描画系コンポーネント
    │   │   ├── GameCanvas/
    │   │   │   ├── index.tsx         # コンポーネント本体
    │   │   │   └── GameCanvas.css.ts # スタイル定義
    │   │   ├── MapLayer/
    │   │   ├── UnitLayer/
    │   │   ├── CursorLayer/
    │   │   └── ReachableLayer/
    │   └── ui/           # 汎用UIコンポーネント
    │       ├── ActionMenu/
    │       ├── TurnIndicator/
    │       ├── UnitInfoPanel/
    │       ├── ProduceMenu/
    │       ├── DropMenu/
    │       └── MainMenu/
    │
    ├── store/            # Zustand ストア
    │   └── gameStore.ts
    │
    ├── wasm/             # WASMバインディング
    │
    ├── worker/           # Web Worker
    │   └── engineWorker.ts
    │
    └── assets/           # 画像・フォント等
```

### コンポーネントディレクトリの規則

各コンポーネントは **PascalCase のディレクトリ** に配置し、以下のファイルで構成します：

```
ComponentName/
├── index.tsx       # コンポーネント本体（名前付きexport）
├── index.css.ts    # スタイル定義（vanilla-extract、必要な場合のみ）
└── index.test.tsx  # テスト（将来）
```

```typescript
// ✅ 良い例: index.tsx からアロー関数で名前付きexport
export const GameCanvas = () => { ... };

// ❌ 悪い例: default export
export default function GameCanvas() { ... }
```

> **名前付きexport**: コンポーネントは必ず名前付きexportを使用します。default exportは使用しません（ただし `App.tsx` 等のルートコンポーネントは例外）。

> **定数の重複定義に注意**: `TILE_SIZE` 等の描画定数は現在複数ファイルで重複定義されています。共通定数は `src/constants/` 等に集約し、単一定義の原則（DRY）を守ってください。

## 8. TypeScript 型定義規約

### 型 vs インターフェース

- **データの形状定義**: `interface` を使用（拡張・マージの可能性がある場合）
- **ユニオン型・交差型・ユーティリティ型**: `type` を使用
- **コンポーネントProps**: `interface` を使用

```typescript
// ✅ データ構造には interface
interface UnitData {
  id: string;
  type: string;
  faction: string;
  x: number;
  y: number;
  hp: number;
}

// ✅ ユニオン型やリテラル型には type
type InteractionState =
  | "idle"
  | "unit_selected"
  | "action_menu"
  | "target_selection";

// ✅ コンポーネントPropsには interface
interface TurnIndicatorProps {
  turn: number;
  phase: string;
  funds: number;
  onEndTurn: () => void;
  isAiThinking: boolean;
}
```

### 型定義の配置

| 種類 | 配置場所 | 例 |
| --- | --- | --- |
| コンポーネントProps | コンポーネントファイル内 | `interface GameCanvasProps { ... }` |
| ストアの状態型 | ストアファイル内 | `interface GameState { ... }` |
| 共有ドメインモデル | `src/types/` に分離（将来） | `UnitData`, `TurnInfo` |
| WASMの型定義 | `src/wasm/` 配下 | WASMバインディング型 |

### 禁止事項

```typescript
// ❌ any型の使用（やむを得ない場合はコメントで理由を明記）
const data: any = fetchData();

// ✅ 具体的な型を使用
const data: UnitData[] = fetchData();

// ❌ 型アサーションの乱用
const unit = data as UnitData;

// ✅ 型ガードを使用
function isUnitData(data: unknown): data is UnitData {
  return typeof data === "object" && data !== null && "id" in data;
}
```

### マジックナンバー・マジックストリング禁止

```typescript
// ❌ 悪い例
if (turnInfo.turn > 50) { ... }
if (unit.faction === "green") { ... }

// ✅ 良い例
const MAX_TURNS = 50;
const FACTION = {
  GREEN: "green",
  BLUE: "blue",
} as const;

if (turnInfo.turn > MAX_TURNS) { ... }
if (unit.faction === FACTION.GREEN) { ... }
```

## 9. React コーディング規約

### コンポーネント設計

#### 関数コンポーネント + Hooks

クラスコンポーネントは使用禁止。必ず関数コンポーネントとHooksを使用します。

```typescript
// ✅ 良い例: アロー関数で定義
export const TurnIndicator = ({ turn, phase, funds, onEndTurn }: TurnIndicatorProps) => {
  return (
    <div>
      <span>ターン {turn}</span>
      <button onClick={onEndTurn}>ターン終了</button>
    </div>
  );
};

// ❌ 悪い例（クラスコンポーネント）
class TurnIndicator extends React.Component { ... }
```

#### コンポーネントの責務分離

```
components/
├── game/     # ゲーム描画に直接関わるコンポーネント（PixiJS等）
│             # 例: マップ、ユニット、カーソル、到達可能範囲の描画
└── ui/       # 汎用UIコンポーネント（Mantine等）
              # 例: メニュー、情報パネル、ターン表示
```

- **`game/`**: PixiJS (`@pixi/react`) を使ったゲーム描画コンポーネント。ドメインロジックは含めない。
- **`ui/`**: Mantine等を使った汎用UIコンポーネント。ゲーム描画ロジックは含めない。

#### ドメインロジックの非漏出

Rust/engine側と同様に、ゲームルールの判定ロジックをReactコンポーネントに直接記述してはなりません。
ドメインロジックはWASMエンジン側に委譲し、Reactコンポーネントはその結果を表示するのみに留めます。

```typescript
// ❌ 悪い例：コンポーネント内でゲームルールを判定
function ActionMenu({ unit }: Props) {
  // ゲームルールがUI層に漏出している
  const canAttack = unit.weapons.some(w => w.ammo > 0) && unit.actionPoints > 0;
  ...
}

// ✅ 良い例：エンジンから取得した利用可能アクションを使用
function ActionMenu({ actions, onSelect, onClose }: ActionMenuProps) {
  // エンジンが判定した結果を表示するだけ
  return (
    <div>
      {actions.map(action => (
        <button key={action} onClick={() => onSelect(action)}>{action}</button>
      ))}
    </div>
  );
}
```

### 状態管理（Zustand）

#### ストア設計原則

1. **単一ストア**: ゲーム全体の状態は `useGameStore` に集約する
2. **セレクタ使用**: コンポーネントでは必要な状態のみを購読する
3. **アクション集約**: 状態変更はストア内のアクション関数を通じてのみ行う

```typescript
// ✅ 良い例: セレクタで必要な状態のみ取得
const turnInfo = useGameStore(state => state.turnInfo);
const endTurn = useGameStore(state => state.endTurn);

// ❌ 悪い例: ストア全体を取得（不要な再描画の原因）
const store = useGameStore();
```

> **将来の分割**: ストアが肥大化した場合、機能ドメインごとにスライスパターンで分割を検討します。

#### Storeファイル構成

```typescript
// gameStore.ts の構成

// 1. インポート
import { create } from "zustand";

// 2. 型定義（ストアの状態とアクション）
interface GameState {
  // 状態フィールド
  mapData: string[][];
  unitData: UnitData[];
  // ...

  // アクション
  initEngine: (mapName: string) => Promise<void>;
  syncGameState: () => Promise<void>;
  // ...
}

// 3. ストア作成
export const useGameStore = create<GameState>((set, get) => ({
  // 初期値
  mapData: [],
  unitData: [],

  // アクション実装
  initEngine: async (mapName) => {
    // ...
  },
}));
```

### Hooksの規約

- カスタムHookは `use` プレフィックスを必ず付ける
- Hooksの呼び出し順序はコンポーネント内で一定にする（条件分岐内での呼び出し禁止）
- `useEffect` のクリーンアップを必ず実装する（リスナー登録やタイマー等）

```typescript
// ✅ 良い例: クリーンアップ付きuseEffect
useEffect(() => {
  const handleResize = () => { /* リサイズ処理 */ };
  window.addEventListener("resize", handleResize);
  return () => window.removeEventListener("resize", handleResize);
}, []);
```

### イベントハンドラの命名

```typescript
// ✅ ハンドラ名は handle + 対象 + 動作
const handleActionSelect = (action: string) => { ... };
const handleProduceSelect = (unitType: string) => { ... };

// ✅ Props経由のコールバックは on + 動作
interface Props {
  onSelect: (item: string) => void;
  onClose: () => void;
  onEndTurn: () => void;
}
```

## 10. フォーマット / リント / ビルド

### Biome（リンター・フォーマッター）

ESLint/Prettierの代わりに **Biome** を使用します。設定は `web/biome.jsonc` に記載されています。

```bash
# リント確認
pnpm lint

# リント + 自動修正
pnpm lint:fix

# フォーマット
pnpm format
```

#### Biomeの主要設定

- **インデント**: スペース2つ
- **行幅**: 100文字
- **クォートスタイル**: ダブルクォート (`"`)
- **未使用インポート**: エラー
- **Hooksのトップレベル使用**: エラー

### TypeScript 型チェック

`tsconfig.json` で `strict: true` が有効です。以下の設定が含まれます：

- `noUnusedLocals`: 未使用ローカル変数はエラー
- `noUnusedParameters`: 未使用パラメータはエラー
- `noFallthroughCasesInSwitch`: switchのfall-throughはエラー

### ビルドコマンド

```bash
# 開発用起動
pnpm dev
# または ワークスペースルートから
pnpm -C web dev

# ビルド
pnpm build
# または ワークスペースルートから
pnpm -C web build
```

### コミット前チェックリスト

コードをコミットする前に、以下がすべてパスすることを確認します：

```bash
# 1. Biomeリント
pnpm -C web lint

# 2. TypeScript型チェック
pnpm -C web exec tsc --noEmit

# 3. ビルド確認
pnpm -C web build
```

## 11. Web Worker / WASM 連携規約

### アーキテクチャ

```
React (メインスレッド) ←→ Comlink ←→ Web Worker ←→ WASM Engine
```

- Reactコンポーネントはメインスレッドで動作する
- ゲームエンジン（WASM）はWeb Worker上で動作し、メインスレッドをブロックしない
- **Comlink** を使用してWorkerとの通信を型安全に行う

### Worker通信の規則

```typescript
// ✅ 良い例: Workerへの操作はストアのアクション経由で行う
const initEngine = async (mapName: string) => {
  const worker = new Worker(new URL("../worker/engineWorker.ts", import.meta.url), { type: "module" });
  const proxy = Comlink.wrap<EngineWorker>(worker);
  await proxy.init(mapName);
  set({ engineWorker: proxy, isEngineReady: true });
};

// ❌ 悪い例: コンポーネントから直接Workerを操作
function GameCanvas() {
  const worker = new Worker(...); // コンポーネント内でWorkerを生成しない
}
```

### エラーハンドリング

```typescript
// ✅ Worker呼び出しにはtry-catchを必ず付ける
try {
  const result = await engineWorker.moveUnit(unitId, x, y);
  // 結果の処理
} catch (error) {
  console.error("エンジンとの通信でエラーが発生:", error);
  // エラー状態の設定やユーザーへの通知
}
```

## 12. スタイル規約（vanilla-extract）

### ゼロランタイムCSS

本プロジェクトでは **vanilla-extract** を使用します。ランタイムオーバーヘッドなしに型安全なスタイル定義が可能です。

### スタイルファイルの命名

```
index.css.ts   # コンポーネントディレクトリ内に配置
```

### 使用例

```typescript
// ActionMenu/index.css.ts
import { style } from "@vanilla-extract/css";

// スタイル変数名は camelCase
export const actionMenuContainer = style({
  position: "absolute",
  zIndex: 200,
  width: "150px",
});

export const menuItem = style({
  cursor: "pointer",
  padding: "4px 8px",
});
```

```typescript
// ActionMenu/index.tsx
import { actionMenuContainer, menuItem } from "./index.css";

export const ActionMenu = ({ actions, onSelect }: ActionMenuProps) => {
  return <div className={actionMenuContainer}>...</div>;
};
```

### MantineとのCSS共存

- **レイアウト・装飾**: vanilla-extract を使用
- **UIコンポーネント（ボタン、モーダル等）**: Mantine のコンポーネントを使用
- Mantine のスタイルオーバーライドが必要な場合は、vanilla-extract で `className` を上書きする
