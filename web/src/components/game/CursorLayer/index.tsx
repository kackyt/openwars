import { Graphics } from "@pixi/react";
import type * as PIXI from "pixi.js";
import { useCallback } from "react";
import {
  CURSOR_COLOR,
  CURSOR_CORNER_SIZE,
  CURSOR_LINE_WIDTH,
  CURSOR_MARGIN,
} from "../../../constants/rendering";
import { useGameStore } from "../../../store/gameStore";

interface CursorProps {
  x: number;
  y: number;
  tileSize: number;
}

/**
 * カーソルレイヤーコンポーネント
 * 選択中/ホバー中のセルを強調表示するカーソル(枠線)を描画します。
 */
export const CursorLayer = ({ x, y, tileSize }: CursorProps) => {
  // Zustand のセレクタを用いて必要な値のみを取得する
  const topology = useGameStore((state) => state.topology);
  const isHexOddRow = topology === "hex" && y % 2 !== 0;
  const offsetX = isHexOddRow ? tileSize / 2 : 0;

  const draw = useCallback(
    (g: PIXI.Graphics) => {
      g.clear();
      g.lineStyle(CURSOR_LINE_WIDTH, CURSOR_COLOR, 1);

      // コーナーだけを描画してターゲットスコープのように見せる
      // Top-Left
      g.moveTo(CURSOR_MARGIN, CURSOR_MARGIN + CURSOR_CORNER_SIZE);
      g.lineTo(CURSOR_MARGIN, CURSOR_MARGIN);
      g.lineTo(CURSOR_MARGIN + CURSOR_CORNER_SIZE, CURSOR_MARGIN);

      // Top-Right
      g.moveTo(tileSize - CURSOR_MARGIN - CURSOR_CORNER_SIZE, CURSOR_MARGIN);
      g.lineTo(tileSize - CURSOR_MARGIN, CURSOR_MARGIN);
      g.lineTo(tileSize - CURSOR_MARGIN, CURSOR_MARGIN + CURSOR_CORNER_SIZE);

      // Bottom-Right
      g.moveTo(tileSize - CURSOR_MARGIN, tileSize - CURSOR_MARGIN - CURSOR_CORNER_SIZE);
      g.lineTo(tileSize - CURSOR_MARGIN, tileSize - CURSOR_MARGIN);
      g.lineTo(tileSize - CURSOR_MARGIN - CURSOR_CORNER_SIZE, tileSize - CURSOR_MARGIN);

      // Bottom-Left
      g.moveTo(CURSOR_MARGIN + CURSOR_CORNER_SIZE, tileSize - CURSOR_MARGIN);
      g.lineTo(CURSOR_MARGIN, tileSize - CURSOR_MARGIN);
      g.lineTo(CURSOR_MARGIN, tileSize - CURSOR_MARGIN - CURSOR_CORNER_SIZE);
    },
    [tileSize],
  );

  return <Graphics x={x * tileSize + offsetX} y={y * tileSize} draw={draw} />;
};
