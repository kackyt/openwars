import { useState, useEffect, useRef } from 'react';
import { Container, Sprite, Graphics, useTick } from '@pixi/react';
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

  const renderedPointsRef = useRef<Record<string, number>>({});
  const [renderedPoints, setRenderedPoints] = useState<Record<string, number>>({});

  useEffect(() => {
    let changed = false;
    for (const p of propertyData) {
      const key = `${p.x}-${p.y}`;
      if (renderedPointsRef.current[key] === undefined) {
        renderedPointsRef.current[key] = p.capture_points;
        changed = true;
      }
    }
    if (changed) {
      setRenderedPoints({ ...renderedPointsRef.current });
    }
  }, [propertyData]);

  // 耐久値減少のアニメーション
  useTick((delta) => {
    const step = 0.05 * delta;
    let changed = false;

    for (const p of propertyData) {
      const key = `${p.x}-${p.y}`;
      const current = renderedPointsRef.current[key] ?? p.capture_points;

      if (Math.abs(current - p.capture_points) > 0.01) {
        if (current > p.capture_points) {
          renderedPointsRef.current[key] = Math.max(p.capture_points, current - 2 * step);
        } else {
          // 増加（回復）した時は瞬時に戻す
          renderedPointsRef.current[key] = p.capture_points;
        }
        changed = true;
      }
    }

    if (changed) {
      setRenderedPoints({ ...renderedPointsRef.current });
    }
  });

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
                  y={4}
                  draw={(g) => {
                    const key = `${property.x}-${property.y}`;
                    const displayPoints = renderedPoints[key] ?? property.capture_points;

                    g.clear();
                    g.beginFill(0x000000, 0.5);
                    g.drawRect(0, 0, TILE_SIZE - 8, 6);
                    g.endFill();

                    const ratio = displayPoints / property.max_capture_points;
                    let barColor = 0x00ff00; // 緑
                    if (ratio < 0.3) {
                      barColor = 0xff0000; // 赤
                    } else if (ratio < 0.7) {
                      barColor = 0xff9900; // 橙
                    }

                    g.beginFill(barColor);
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
