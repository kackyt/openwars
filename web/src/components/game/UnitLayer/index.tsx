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
  const unitData = useGameStore(state => state.unitData);

  return (
    <Container>
      {unitData.map((unit) => (
        <Sprite
          key={unit.id}
          image={`/assets/units/${unit.faction}/${UNIT_IMAGE_MAP[unit.type] || 'infantry'}.png`}
          x={unit.x * TILE_SIZE}
          y={unit.y * TILE_SIZE}
          width={TILE_SIZE}
          height={TILE_SIZE}
        />
      ))}
    </Container>
  );
};
