import { Container, Sprite } from '@pixi/react';

const TILE_SIZE = 64;

// モック用のユニットデータ
export const MOCK_UNITS = [
  { id: 'u1', type: 'infantry', faction: 'blue', x: 1, y: 1 },
  { id: 'u2', type: 'medium_tank', faction: 'green', x: 3, y: 2 },
];

export const UnitLayer = () => {
  return (
    <Container>
      {MOCK_UNITS.map((unit) => (
        <Sprite
          key={unit.id}
          image={`/assets/units/${unit.faction}/${unit.type}.png`}
          x={unit.x * TILE_SIZE}
          y={unit.y * TILE_SIZE}
          width={TILE_SIZE}
          height={TILE_SIZE}
        />
      ))}
    </Container>
  );
};
