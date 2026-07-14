import { Container, Sprite, Text, Graphics, useTick } from '@pixi/react';
import { TextStyle } from 'pixi.js';
import { useGameStore } from '../../../store/gameStore';
import { useState, useEffect, useRef } from 'react';

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

interface ExplosionState {
  x: number;
  y: number;
  progress: number;
}

export const UnitLayer = () => {
  const { 
    unitData, topology, selectedUnitId, selectedTargetPos, interactionState,
    recentlyDestroyedUnitIds, clearRecentlyDestroyedUnits
  } = useGameStore();

  const renderedHpsRef = useRef<Record<string, number>>({});
  const [renderedHps, setRenderedHps] = useState<Record<string, number>>({});

  const explosionsRef = useRef<Record<string, ExplosionState>>({});
  const [explosions, setExplosions] = useState<Record<string, ExplosionState>>({});

  const prevUnitsRef = useRef<typeof unitData>([]);

  useEffect(() => {
    const prevUnits = prevUnitsRef.current;
    const currentIds = new Set(unitData.map(u => u.id));

    // 爆発エフェクト開始判定
    let explosionsChanged = false;
    for (const prevUnit of prevUnits) {
      if (!currentIds.has(prevUnit.id)) {
        if (recentlyDestroyedUnitIds.includes(prevUnit.id)) {
          explosionsRef.current[prevUnit.id] = {
            x: prevUnit.x,
            y: prevUnit.y,
            progress: 0,
          };
          explosionsChanged = true;
        }
      }
    }
    if (explosionsChanged) {
      setExplosions({ ...explosionsRef.current });
      clearRecentlyDestroyedUnits();
    }

    // 初期HP設定
    for (const unit of unitData) {
      if (renderedHpsRef.current[unit.id] === undefined) {
        renderedHpsRef.current[unit.id] = unit.hp;
      }
    }
    setRenderedHps({ ...renderedHpsRef.current });

    prevUnitsRef.current = unitData;
  }, [unitData, recentlyDestroyedUnitIds, clearRecentlyDestroyedUnits]);

  // Pixiフレームアップデート
  useTick((delta) => {
    const step = 0.05 * delta;

    // HP減少補間アニメーション
    let hpChanged = false;
    for (const unit of unitData) {
      const currentRendered = renderedHpsRef.current[unit.id] ?? unit.hp;
      if (Math.abs(currentRendered - unit.hp) > 0.01) {
        if (currentRendered > unit.hp) {
          renderedHpsRef.current[unit.id] = Math.max(unit.hp, currentRendered - 10 * step);
        } else {
          renderedHpsRef.current[unit.id] = unit.hp;
        }
        hpChanged = true;
      }
    }
    if (hpChanged) {
      setRenderedHps({ ...renderedHpsRef.current });
    }

    // 爆発アニメーション更新
    let explosionsChanged = false;
    const nextExplosions = { ...explosionsRef.current };
    for (const id in nextExplosions) {
      const exp = nextExplosions[id];
      if (exp.progress < 1) {
        exp.progress = Math.min(1, exp.progress + step * (300 / 500));
        explosionsChanged = true;
      } else {
        delete nextExplosions[id];
        explosionsChanged = true;
      }
    }
    if (explosionsChanged) {
      explosionsRef.current = nextExplosions;
      setExplosions(nextExplosions);
    }
  });

  return (
    <Container>
      {/* ユニット描画 */}
      {unitData.map((unit) => {
        const isSelectedAndMoving = selectedUnitId === unit.id && 
          ['action_menu', 'target_selection', 'drop_unit_selection', 'drop_target_selection'].includes(interactionState) && 
          selectedTargetPos;
        const targetX = isSelectedAndMoving ? selectedTargetPos.x : unit.x;
        const targetY = isSelectedAndMoving ? selectedTargetPos.y : unit.y;

        const isHexOddRow = topology === 'hex' && targetY % 2 !== 0;
        const offsetX = isHexOddRow ? TILE_SIZE / 2 : 0;
        const px = targetX * TILE_SIZE + offsetX;
        const py = targetY * TILE_SIZE;

        const displayHp = renderedHps[unit.id] ?? unit.hp;

        return (
          <Container key={unit.id} x={px} y={py}>
            <Sprite
              image={`/assets/units/${unit.faction}/${UNIT_IMAGE_MAP[unit.type] || 'infantry'}.png`}
              width={TILE_SIZE}
              height={TILE_SIZE}
            />
            
            {/* L: 積載中 */}
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

            {/* E: 行動完了 */}
            {unit.is_exhausted && (
              <Container x={TILE_SIZE - 16} y={0}>
                <Graphics
                  draw={(g) => {
                    g.clear();
                    g.beginFill(0x333333, 0.8);
                    g.drawRoundedRect(0, 0, 16, 16, 4);
                    g.endFill();
                  }}
                />
                <Text text="E" style={new TextStyle({ fill: 'white', fontSize: 12, fontWeight: 'bold' })} x={4} y={1} />
              </Container>
            )}

            {/* 耐久力バー (下部) */}
            <Graphics
              draw={(g) => {
                g.clear();
                g.beginFill(0x000000, 0.6);
                g.drawRect(2, TILE_SIZE - 6, TILE_SIZE - 4, 4);
                g.endFill();
                
                const ratio = displayHp / 10;
                let color = 0x00ff00; // 緑
                if (displayHp < 3) {
                  color = 0xff0000; // 赤
                } else if (displayHp < 7) {
                  color = 0xff9900; // 橙
                }
                
                g.beginFill(color);
                g.drawRect(2, TILE_SIZE - 6, (TILE_SIZE - 4) * ratio, 4);
                g.endFill();
              }}
            />
          </Container>
        );
      })}

      {/* 爆発エフェクト */}
      {Object.entries(explosions).map(([id, exp]) => {
        const isHexOddRow = topology === 'hex' && exp.y % 2 !== 0;
        const offsetX = isHexOddRow ? TILE_SIZE / 2 : 0;
        const px = exp.x * TILE_SIZE + offsetX;
        const py = exp.y * TILE_SIZE;

        const size = TILE_SIZE * 1.5 * exp.progress;
        const offset = (TILE_SIZE - size) / 2;
        const alpha = exp.progress < 0.8 ? 1.0 : (1.0 - exp.progress) / 0.2;

        return (
          <Sprite
            key={`exp-${id}`}
            image="/assets/misc/boom.png"
            x={px + offset}
            y={py + offset}
            width={size}
            height={size}
            alpha={alpha}
          />
        );
      })}
    </Container>
  );
};
