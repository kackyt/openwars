// レンダリング用の共通定数を定義するモジュール
// 日本語コメントで役割を記述しています。

/** タイルの1辺のサイズ (px) */
export const TILE_SIZE = 48;

/** メインゲーム画面の背景色 */
export const STAGE_BACKGROUND_COLOR = 0x1099bb;

/** ドラッグとクリックを判定する閾値 (px) */
export const DRAG_THRESHOLD = 10;

/** カーソル描画用の線幅 (px) */
export const CURSOR_LINE_WIDTH = 4;
/** カーソルのマージン (px) */
export const CURSOR_MARGIN = 2;
/** カーソルの角のサイズ (px) */
export const CURSOR_CORNER_SIZE = 12;
/** カーソルの色 (黄色) */
export const CURSOR_COLOR = 0xffeb3b;

/** 移動可能セルの塗りつぶし色 */
export const REACHABLE_CELL_COLOR = 0x00aaff;
/** 移動可能セルの塗りつぶし不透明度 */
export const REACHABLE_CELL_ALPHA = 0.4;
/** 移動可能セルの枠線色 */
export const REACHABLE_BORDER_COLOR = 0x0088cc;
/** 移動可能セルの枠線不透明度 */
export const REACHABLE_BORDER_ALPHA = 0.8;

/** 攻撃対象セルの塗りつぶし色 */
export const TARGET_CELL_COLOR = 0xff0000;
/** 攻撃対象セルの塗りつぶし不透明度 */
export const TARGET_CELL_ALPHA = 0.4;
/** 攻撃対象セルの枠線色 */
export const TARGET_BORDER_COLOR = 0xff0000;
/** 攻撃対象セルの枠線不透明度 */
export const TARGET_BORDER_ALPHA = 0.8;

/** 占領ゲージ背景色 */
export const CAPTURE_BAR_BG_COLOR = 0x000000;
/** 占領ゲージ背景不透明度 */
export const CAPTURE_BAR_BG_ALPHA = 0.5;

/** HPバー背景色 */
export const HP_BAR_BG_COLOR = 0x000000;
/** HPバー背景不透明度 */
export const HP_BAR_BG_ALPHA = 0.6;

/** 積載数バッジ背景色 */
export const LOADED_BADGE_BG_COLOR = 0x000000;
/** 積載数バッジ背景不透明度 */
export const LOADED_BADGE_BG_ALPHA = 0.7;

/** 行動済みバッジ背景色 */
export const EXHAUSTED_BADGE_BG_COLOR = 0x333333;
/** 行動済みバッジ背景不透明度 */
export const EXHAUSTED_BADGE_BG_ALPHA = 0.8;

/** 占領バー補間ステップ係数 */
export const CAPTURE_STEP = 0.05;
/** 占領バーのアニメーション完了判定閾値 */
export const CAPTURE_THRESHOLD = 0.01;
/** 占領バーの減少速度係数 */
export const CAPTURE_SPEED_COEFF = 20;

/** HP補間ステップ係数 */
export const HP_STEP = 0.05;
/** HPアニメーション完了判定閾値 */
export const HP_THRESHOLD = 0.01;
/** HP減少速度係数 */
export const HP_SPEED_COEFF = 10;

/** 爆発アニメーション速度係数 */
export const EXPLOSION_SPEED_COEFF = 300 / 500;
/** 爆発エフェクトの最大スケール */
export const EXPLOSION_MAX_SCALE = 1.5;
/** 爆発フェードアウト開始タイミング (0.0〜1.0) */
export const EXPLOSION_FADE_THRESHOLD_START = 0.8;
/** 爆発フェードアウトの除数 */
export const EXPLOSION_FADE_THRESHOLD_DIV = 0.2;

/** 降車メニューの z-index */
export const DROP_MENU_Z_INDEX = 100;
/** 降車メニューの最小横幅 (px) */
export const DROP_MENU_MIN_WIDTH = 200;

/** メインメニューの上部マージン (px) */
export const MAIN_MENU_MARGIN_TOP = 100;

/** ターンインジケーターのカラーサンプルサイズ (px) */
export const COLOR_SWATCH_SIZE = 14;

// HPバー色の定義 (MapLayer と UnitLayer で共通化)
export const HP_COLOR_GOOD = 0x00ff00; // 緑
export const HP_COLOR_WARNING = 0xff9900; // 橙
export const HP_COLOR_DANGER = 0xff0000; // 赤

/** MapLayer用のHP割合閾値 */
export const MAP_HP_THRESHOLD_WARNING = 0.7;
export const MAP_HP_THRESHOLD_DANGER = 0.3;

/** UnitLayer用のHP絶対値閾値 */
export const UNIT_HP_THRESHOLD_WARNING = 7;
export const UNIT_HP_THRESHOLD_DANGER = 3;
