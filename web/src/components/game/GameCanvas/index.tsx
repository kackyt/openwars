import { Stage, Container, Sprite } from '@pixi/react';
import * as PIXI from 'pixi.js';
import { MapLayer, MOCK_MAP } from '../MapLayer';
import { UnitLayer, MOCK_UNITS } from '../UnitLayer';
import { CursorLayer } from '../CursorLayer';

const TILE_SIZE = 64;

interface GameCanvasProps {
  hoverX: number;
  hoverY: number;
  onCellClick?: (x: number, y: number, cellType: number, unitInfo: any | null, clientX: number, clientY: number) => void;
  onCellHover?: (x: number, y: number, cellType: number, unitInfo: any | null) => void;
}

export const GameCanvas = ({ hoverX, hoverY, onCellClick, onCellHover }: GameCanvasProps) => {
  const getCellData = (globalX: number, globalY: number) => {
    const gridX = Math.floor(globalX / TILE_SIZE);
    const gridY = Math.floor(globalY / TILE_SIZE);
    const cellType = MOCK_MAP[gridY]?.[gridX] ?? 0;
    const unit = MOCK_UNITS.find(u => u.x === gridX && u.y === gridY) || null;
    return { gridX, gridY, cellType, unit };
  };

  const handlePointerDown = (e: any) => {
    if (!onCellClick) return;
    const { gridX, gridY, cellType, unit } = getCellData(e.data.global.x, e.data.global.y);
    const clientX = e.data.originalEvent.clientX;
    const clientY = e.data.originalEvent.clientY;
    onCellClick(gridX, gridY, cellType, unit, clientX, clientY);
  };

  const handlePointerMove = (e: any) => {
    if (!onCellHover) return;
    const { gridX, gridY, cellType, unit } = getCellData(e.data.global.x, e.data.global.y);
    onCellHover(gridX, gridY, cellType, unit);
  };

  return (
    <Stage width={800} height={600} options={{ backgroundColor: 0x1099bb }}>
      {/* @ts-ignore */}
      <Container interactive={true} pointerdown={handlePointerDown} pointermove={handlePointerMove}>
        {/* 全面に当たり判定を持たせるための透明なスプライト */}
        <Sprite 
          texture={PIXI.Texture.WHITE} 
          width={800} 
          height={600} 
          alpha={0} 
        />
        <MapLayer />
        <UnitLayer />
        <CursorLayer x={hoverX} y={hoverY} tileSize={TILE_SIZE} />
      </Container>
    </Stage>
  );
};
