import { Graphics } from '@pixi/react';
import { useCallback } from 'react';

interface CursorLayerProps {
  x: number;
  y: number;
  tileSize: number;
}

export const CursorLayer = ({ x, y, tileSize }: CursorLayerProps) => {
  const draw = useCallback(
    (g: any) => {
      g.clear();
      if (x < 0 || y < 0) return; // 画面外
      g.lineStyle(4, 0xffff00, 1);
      g.drawRect(x * tileSize, y * tileSize, tileSize, tileSize);
    },
    [x, y, tileSize]
  );

  return <Graphics draw={draw} />;
};
