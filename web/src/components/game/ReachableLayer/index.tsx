import { Graphics } from "@pixi/react";
import type * as PIXI from "pixi.js";
import { useCallback } from "react";
import { useGameStore } from "../../../store/gameStore";

export const ReachableLayer = ({ tileSize }: { tileSize: number }) => {
  const { reachableCells, attackableTargets, topology, interactionState } = useGameStore();

  const draw = useCallback(
    (g: PIXI.Graphics) => {
      g.clear();

      // Draw Reachable Cells
      if (interactionState === "unit_selected" || interactionState === "action_menu") {
        g.beginFill(0x00aaff, 0.4);
        g.lineStyle(1, 0x0088cc, 0.8);
        for (const cell of reachableCells) {
          const isHexOddRow = topology === "hex" && cell.y % 2 !== 0;
          const offsetX = isHexOddRow ? tileSize / 2 : 0;
          const px = cell.x * tileSize + offsetX;
          const py = cell.y * tileSize;
          g.drawRect(px, py, tileSize, tileSize);
        }
        g.endFill();
      }

      // Draw Target Selection Cells
      if (interactionState === "target_selection") {
        g.beginFill(0xff0000, 0.4);
        g.lineStyle(2, 0xff0000, 0.8);
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
