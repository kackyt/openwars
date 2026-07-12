import { Stage, Container } from '@pixi/react';
import { MapLayer, MOCK_MAP } from '../MapLayer';
import { UnitLayer, MOCK_UNITS } from '../UnitLayer';

const TILE_SIZE = 64;

interface GameCanvasProps {
  onCellClick?: (x: number, y: number, cellType: number, unitInfo: any | null, clientX: number, clientY: number) => void;
}

export const GameCanvas = ({ onCellClick }: GameCanvasProps) => {
  const handlePointerDown = (e: any) => {
    if (!onCellClick) return;
    
    const globalX = e.data.global.x;
    const globalY = e.data.global.y;
    
    const gridX = Math.floor(globalX / TILE_SIZE);
    const gridY = Math.floor(globalY / TILE_SIZE);

    const cellType = MOCK_MAP[gridY]?.[gridX] ?? 0;
    const unit = MOCK_UNITS.find(u => u.x === gridX && u.y === gridY) || null;

    const clientX = e.data.originalEvent.clientX;
    const clientY = e.data.originalEvent.clientY;

    onCellClick(gridX, gridY, cellType, unit, clientX, clientY);
  };

  return (
    <Stage width={800} height={600} options={{ backgroundColor: 0x1099bb }}>
      {/* @ts-ignore */}
      <Container interactive={true} pointerdown={handlePointerDown}>
        <MapLayer />
        <UnitLayer />
      </Container>
    </Stage>
  );
};
