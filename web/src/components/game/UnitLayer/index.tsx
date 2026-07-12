import { Container, Sprite } from '@pixi/react';
import { useGameStore } from '../../../store/gameStore';

const TILE_SIZE = 64;

const UNIT_IMAGE_MAP: Record<string, string> = {
  'infantry': 'infantry',
  'mech': 'mech_infantry',
  'recon': 'armored_vehicle',
  'tank': 'light_tank',
  'mdtank': 'medium_tank',
  'tankz': 'heavy_tank',
  'artillery': 'artillery',
  'lightspgun': 'light_artillery',
  'heavyspgun': 'heavy_artillery',
  'rockets': 'rocket',
  'antiair': 'anti_air_tank',
  'missiles': 'anti_air_missile',
  'fighter': 'fighter',
  'heavyfighter': 'heavy_fighter',
  'bomber': 'bomber',
  'bcopters': 'battle_copter',
  'transporthelicopter': 'transport_copter',
  'battleship': 'battleship',
  'carrier': 'carrier',
  'lander': 'lander',
  'supplytruck': 'supply_truck',
};

export const UnitLayer = () => {
  const { unitData, topology } = useGameStore();

  return (
    <Container>
      {unitData.map((unit) => {
        const isHexOddRow = topology === 'hex' && unit.y % 2 !== 0;
        const offsetX = isHexOddRow ? 32 : 0;
        const px = unit.x * TILE_SIZE + offsetX;
        const py = unit.y * TILE_SIZE;

        return (
          <Sprite
            key={unit.id}
            image={`/assets/units/${unit.faction}/${UNIT_IMAGE_MAP[unit.type] || 'infantry'}.png`}
            x={px}
            y={py}
            width={TILE_SIZE}
            height={TILE_SIZE}
          />
        );
      })}
    </Container>
  );
};
