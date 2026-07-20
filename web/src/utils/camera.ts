/**
 * @file camera.ts
 * @description カメラの位置クランプや、画面座標からグリッド座標への変換を行う純粋関数ヘルパー。
 */

/**
 * 画面上の位置 (px) をカメラ位置とグリッド形式を考慮してグリッドのセル座標 (X, Y) に変換します。
 * @param globalX 画面X座標
 * @param globalY 画面Y座標
 * @param cameraX カメラXスクロール量
 * @param cameraY カメラYスクロール量
 * @param topology グリッド形式
 * @param tileSize タイルのサイズ (px)
 */
export const globalToGrid = (
  globalX: number,
  globalY: number,
  cameraX: number,
  cameraY: number,
  topology: "square" | "hex",
  tileSize: number,
): { gridX: number; gridY: number } => {
  const localX = globalX - cameraX;
  const localY = globalY - cameraY;

  const gridY = Math.floor(localY / tileSize);
  let offsetX = 0;
  if (topology === "hex" && gridY % 2 !== 0) {
    offsetX = tileSize / 2;
  }
  const gridX = Math.floor((localX - offsetX) / tileSize);

  return { gridX, gridY };
};

/**
 * グリッドセル座標 (X, Y) から画面上のピクセル位置 (px) を算出します。
 * @param gridX グリッドX座標
 * @param gridY グリッドY座標
 * @param cameraX カメラXスクロール量
 * @param cameraY カメラYスクロール量
 * @param topology グリッド形式
 * @param tileSize タイルのサイズ (px)
 */
export const gridToGlobal = (
  gridX: number,
  gridY: number,
  cameraX: number,
  cameraY: number,
  topology: "square" | "hex",
  tileSize: number,
): { globalX: number; globalY: number } => {
  let offsetX = 0;
  if (topology === "hex" && gridY % 2 !== 0) {
    offsetX = tileSize / 2;
  }
  const globalX = gridX * tileSize + offsetX + cameraX;
  const globalY = gridY * tileSize + cameraY;
  return { globalX, globalY };
};

/** UI要素（TurnIndicatorやUnitInfoPanelなど）による遮蔽を回避するための画面余白（px） */
const PADDING_TOP = 120;
const PADDING_BOTTOM = 140;
const PADDING_LEFT = 60;
const PADDING_RIGHT = 120;

/**
 * マップ全体の描画範囲と画面サイズを比較し、カメラが範囲外スクロールしないようにクランプします。
 * UIパネルによる遮蔽を避けるため、上・下・左右にスクロール可能な余裕（パディング）を持たせます。
 * @param newX スクロール後のX位置
 * @param newY スクロール後のY位置
 * @param mapWidth マップ全体の横幅 (px)
 * @param mapHeight マップ全体の縦幅 (px)
 * @param windowWidth 表示画面の横幅 (px)
 * @param windowHeight 表示画面の縦幅 (px)
 */
export const clampCameraPosition = (
  newX: number,
  newY: number,
  mapWidth: number,
  mapHeight: number,
  windowWidth: number,
  windowHeight: number,
): { x: number; y: number } => {
  const maxX = PADDING_LEFT;
  const minX = Math.min(PADDING_LEFT, windowWidth - mapWidth - PADDING_RIGHT);

  const maxY = PADDING_TOP;
  const minY = Math.min(PADDING_TOP, windowHeight - mapHeight - PADDING_BOTTOM);

  return {
    x: Math.max(minX, Math.min(maxX, newX)),
    y: Math.max(minY, Math.min(maxY, newY)),
  };
};
