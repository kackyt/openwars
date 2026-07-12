import { Container, Sprite } from '@pixi/react';
import { useGameStore } from '../../../store/gameStore';

const TILE_SIZE = 64;

const TERRAIN_IMAGE_MAP: Record<string, string> = {
  'plains': 'plain',
  'forest': 'woods',
  'mountain': 'mountain',
  'river': 'river',
  'road': 'road',
  'bridge': 'bridge',
  'sea': 'sea',
  'shoal': 'shoal',
};

const getTerrainImagePath = (cell: string, propertyOwner: string = 'neutral') => {
  if (['city', 'factory', 'airport', 'port', 'capital'].includes(cell)) {
    const filename = cell === 'capital' ? 'hq' : cell;
    return `/assets/properties/${propertyOwner}/${filename}.png`;
  }
  
  const mapped = TERRAIN_IMAGE_MAP[cell] || 'plain';
  return `/assets/terrains/${mapped}.png`;
};

export const MapLayer = () => {
  const { mapData, propertyData, topology } = useGameStore();

  return (
    <Container>
      {mapData.map((row, y) => (
        row.map((cell, x) => {
          const isHexOddRow = topology === 'hex' && y % 2 !== 0;
          const offsetX = isHexOddRow ? 32 : 0; // 32 is TILE_SIZE / 2
          const px = x * 64 + offsetX;
          const py = y * 64;
          const property = propertyData.find(p => p.x === x && p.y === y);

          return (
            <Sprite
              key={`cell-${x}-${y}`}
              image={getTerrainImagePath(cell, property?.owner || 'neutral')}
              x={px}
              y={py}
              width={TILE_SIZE}
              height={TILE_SIZE}
            />
          );
        })
      ))}
    </Container>
  );
};
