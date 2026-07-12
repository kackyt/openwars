import { Graphics } from '@pixi/react';
import { useCallback } from 'react';
import * as PIXI from 'pixi.js';
import { useGameStore } from '../../../store/gameStore';

interface CursorProps {
  x: number;
  y: number;
  tileSize: number;
}

export const CursorLayer = ({ x, y, tileSize }: CursorProps) => {
  const topology = useGameStore(state => state.topology);
  const isHexOddRow = topology === 'hex' && y % 2 !== 0;
  const offsetX = isHexOddRow ? tileSize / 2 : 0;

  const draw = useCallback((g: PIXI.Graphics) => {
    g.clear();
    g.lineStyle(4, 0xffeb3b, 1);
    
    const margin = 2;
    // コーナーだけを描画してターゲットスコープのように見せる
    const cornerSize = 12;
    
    // Top-Left
    g.moveTo(margin, margin + cornerSize);
    g.lineTo(margin, margin);
    g.lineTo(margin + cornerSize, margin);
    
    // Top-Right
    g.moveTo(tileSize - margin - cornerSize, margin);
    g.lineTo(tileSize - margin, margin);
    g.lineTo(tileSize - margin, margin + cornerSize);
    
    // Bottom-Right
    g.moveTo(tileSize - margin, tileSize - margin - cornerSize);
    g.lineTo(tileSize - margin, tileSize - margin);
    g.lineTo(tileSize - margin - cornerSize, tileSize - margin);
    
    // Bottom-Left
    g.moveTo(margin + cornerSize, tileSize - margin);
    g.lineTo(margin, tileSize - margin);
    g.lineTo(margin, tileSize - margin - cornerSize);
  }, [tileSize]);

  return (
    <Graphics 
      x={x * tileSize + offsetX} 
      y={y * tileSize} 
      draw={draw} 
    />
  );
};
