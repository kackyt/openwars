import { Container, Sprite, Text, Graphics } from '@pixi/react';
import { TextStyle } from 'pixi.js';
import { useGameStore } from '../../../store/gameStore';

const TILE_SIZE = 48;

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
        const offsetX = isHexOddRow ? TILE_SIZE / 2 : 0;
        const px = unit.x * TILE_SIZE + offsetX;
        const py = unit.y * TILE_SIZE;

        return (
          <Container key={unit.id} x={px} y={py}>
            <Sprite
              image={`/assets/units/${unit.faction}/${UNIT_IMAGE_MAP[unit.type] || 'infantry'}.png`}
              width={TILE_SIZE}
              height={TILE_SIZE}
            />
            {unit.is_loaded && (
              <Container x={TILE_SIZE - 16} y={TILE_SIZE - 16}>
                <Graphics
                  draw={(g) => {
                    g.clear();
                    g.beginFill(0x000000, 0.7);
                    g.drawRoundedRect(0, 0, 16, 16, 4);
                    g.endFill();
                  }}
                />
                <Text text="L" style={new TextStyle({ fill: 'white', fontSize: 12, fontWeight: 'bold' })} x={4} y={1} />
              </Container>
            )}
          </Container>
        );
      })}
    </Container>
  );
};
