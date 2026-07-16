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
 * マップ全体の描画範囲と画面サイズを比較し、カメラが範囲外スクロールしないようにクランプします。
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
  const minX = Math.min(0, windowWidth - mapWidth);
  const minY = Math.min(0, windowHeight - mapHeight);

  return {
    x: Math.max(minX, Math.min(0, newX)),
    y: Math.max(minY, Math.min(0, newY)),
  };
};
