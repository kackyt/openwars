import { Stage } from '@pixi/react';
import { MapLayer } from '../MapLayer';
import { UnitLayer } from '../UnitLayer';

export const GameCanvas = () => {
  return (
    <Stage width={800} height={600} options={{ backgroundColor: 0x1099bb }}>
      <MapLayer />
      <UnitLayer />
    </Stage>
  );
};
