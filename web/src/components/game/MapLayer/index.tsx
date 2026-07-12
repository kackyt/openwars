import { Container, Sprite } from '@pixi/react';

const TILE_SIZE = 64;

// モック用のマップデータ (0=平地, 1=森, 2=山)
const MOCK_MAP = [
  [0, 0, 1, 2, 0],
  [0, 1, 1, 0, 0],
  [0, 0, 0, 0, 2],
  [1, 0, 0, 0, 0],
];

const TERRAIN_TEXTURES: Record<number, string> = {
  0: '/assets/terrains/plain.png',
  1: '/assets/terrains/woods.png',
  2: '/assets/terrains/mountain.png',
};

export const MapLayer = () => {
  return (
    <Container>
      {MOCK_MAP.map((row, y) =>
        row.map((cell, x) => (
          <Sprite
            key={`cell-${x}-${y}`}
            image={TERRAIN_TEXTURES[cell]}
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
