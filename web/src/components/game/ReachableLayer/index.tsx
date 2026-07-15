import { Graphics } from "@pixi/react";
import type * as PIXI from "pixi.js";
import { useCallback } from "react";
import {
  REACHABLE_BORDER_ALPHA,
  REACHABLE_BORDER_COLOR,
  REACHABLE_CELL_ALPHA,
  REACHABLE_CELL_COLOR,
  TARGET_BORDER_ALPHA,
  TARGET_BORDER_COLOR,
  TARGET_CELL_ALPHA,
  TARGET_CELL_COLOR,
} from "../../../constants/rendering";
import { useGameStore } from "../../../store/gameStore";

interface ReachableLayerProps {
  tileSize: number;
}

/**
 * 到達可能範囲レイヤーコンポーネント
 * ユニットの移動可能範囲（青色）や攻撃可能対象セル（赤色）のハイライトを描画します。
 */
export const ReachableLayer = ({ tileSize }: ReachableLayerProps) => {
  // Zustand のセレクタを用いて必要な値のみを個別に取得する
  const reachableCells = useGameStore((state) => state.reachableCells);
  const attackableTargets = useGameStore((state) => state.attackableTargets);
  const topology = useGameStore((state) => state.topology);
  const interactionState = useGameStore((state) => state.interactionState);

  const draw = useCallback(
    (g: PIXI.Graphics) => {
      g.clear();

      // 移動可能範囲または降車可能マスのハイライトを描画する
      if (
        interactionState === "unit_selected" ||
        interactionState === "action_menu" ||
        interactionState === "drop_target_selection"
      ) {
        g.beginFill(REACHABLE_CELL_COLOR, REACHABLE_CELL_ALPHA);
        g.lineStyle(1, REACHABLE_BORDER_COLOR, REACHABLE_BORDER_ALPHA);
        for (const cell of reachableCells) {
          const isHexOddRow = topology === "hex" && cell.y % 2 !== 0;
          const offsetX = isHexOddRow ? tileSize / 2 : 0;
          const px = cell.x * tileSize + offsetX;
          const py = cell.y * tileSize;
          g.drawRect(px, py, tileSize, tileSize);
        }
        g.endFill();
      }

      // 攻撃対象の選択可能セルを描画する
      if (interactionState === "target_selection") {
        g.beginFill(TARGET_CELL_COLOR, TARGET_CELL_ALPHA);
        g.lineStyle(2, TARGET_BORDER_COLOR, TARGET_BORDER_ALPHA);
        for (const target of attackableTargets) {
          const isHexOddRow = topology === "hex" && target.y % 2 !== 0;
          const offsetX = isHexOddRow ? tileSize / 2 : 0;
          const px = target.x * tileSize + offsetX;
          const py = target.y * tileSize;
          g.drawRect(px, py, tileSize, tileSize);
        }
        g.endFill();
      }
    },
    [reachableCells, attackableTargets, topology, tileSize, interactionState],
  );

  return <Graphics draw={draw} />;
};
