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
  const mapData = useGameStore(state => state.mapData);
  const propertyData = useGameStore(state => state.propertyData);

  const propertyMap = new Map();
  propertyData.forEach(p => {
    propertyMap.set(`${p.x},${p.y}`, p.owner);
  });

  return (
    <Container>
      {mapData.map((row, y) =>
        row.map((cell, x) => (
          <Sprite
            key={`cell-${x}-${y}`}
            image={getTerrainImagePath(cell, propertyMap.get(`${x},${y}`) || 'neutral')}
            x={x * TILE_SIZE}
            y={y * TILE_SIZE}
            width={TILE_SIZE}
            height={TILE_SIZE}
          />
        ))
      )}
    </Container>
  );
};
