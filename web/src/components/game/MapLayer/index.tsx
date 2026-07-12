import { Container, Sprite, Graphics } from '@pixi/react';
import { useGameStore } from '../../../store/gameStore';

const TILE_SIZE = 48;

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
          const offsetX = isHexOddRow ? TILE_SIZE / 2 : 0;
          const px = x * TILE_SIZE + offsetX;
          const py = y * TILE_SIZE;
          const property = propertyData.find(p => p.x === x && p.y === y);

          return (
            <Container key={`cell-${x}-${y}`} x={px} y={py}>
              <Sprite
                image={getTerrainImagePath(cell, property?.owner || 'neutral')}
                width={TILE_SIZE}
                height={TILE_SIZE}
              />
              {property && property.capture_points < property.max_capture_points && property.max_capture_points > 0 && (
                <Graphics
                  x={4}
                  y={TILE_SIZE - 8}
                  draw={(g) => {
                    g.clear();
                    g.beginFill(0x000000, 0.5);
                    g.drawRect(0, 0, TILE_SIZE - 8, 6);
                    g.endFill();
                    g.beginFill(0x00ff00);
                    const ratio = property.capture_points / property.max_capture_points;
                    g.drawRect(1, 1, (TILE_SIZE - 10) * ratio, 4);
                    g.endFill();
                  }}
                />
              )}
            </Container>
          );
        })
      ))}
    </Container>
  );
};
