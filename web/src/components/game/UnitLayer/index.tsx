import { Container, Graphics, Sprite, Text, useTick } from "@pixi/react";
import { TextStyle } from "pixi.js";
import { useEffect, useRef, useState } from "react";
import { UNIT_IMAGE_MAP } from "../../../constants/mappings";
import {
  EXHAUSTED_BADGE_BG_ALPHA,
  EXHAUSTED_BADGE_BG_COLOR,
  EXPLOSION_FADE_THRESHOLD_DIV,
  EXPLOSION_FADE_THRESHOLD_START,
  EXPLOSION_MAX_SCALE,
  EXPLOSION_SPEED_COEFF,
  HP_BAR_BG_ALPHA,
  HP_BAR_BG_COLOR,
  HP_COLOR_DANGER,
  HP_COLOR_GOOD,
  HP_COLOR_WARNING,
  HP_SPEED_COEFF,
  HP_STEP,
  HP_THRESHOLD,
  LOADED_BADGE_BG_ALPHA,
  LOADED_BADGE_BG_COLOR,
  TILE_SIZE,
  UNIT_HP_THRESHOLD_DANGER,
  UNIT_HP_THRESHOLD_WARNING,
} from "../../../constants/rendering";
import { useGameStore } from "../../../store/gameStore";

/** バッジの描画レイアウト定数 */
const BADGE_SIZE = 16;
const BADGE_FONT_SIZE = 12;
const BADGE_RADIUS = 4;
const HP_BAR_MARGIN_X = 2;
const HP_BAR_HEIGHT = 4;
const HP_BAR_OFFSET_Y_FROM_BOTTOM = 6;
const HP_MAX = 10;

interface ExplosionState {
  x: number;
  y: number;
  progress: number;
}

/**
 * ユニットレイヤーコンポーネント
 * マップ上の全ユニットを描画し、積載・行動済みステータス、HPバーを表示します。
 * また、ユニット撃破時の爆発エフェクトの描画・制御も行います。
 */
export const UnitLayer = () => {
  // Zustand から必要な状態のみを個別に購読する (セレクタを使用)
  const unitData = useGameStore((state) => state.unitData);
  const topology = useGameStore((state) => state.topology);
  const selectedUnitId = useGameStore((state) => state.selectedUnitId);
  const selectedTargetPos = useGameStore((state) => state.selectedTargetPos);
  const interactionState = useGameStore((state) => state.interactionState);
  const recentlyDestroyedUnitIds = useGameStore((state) => state.recentlyDestroyedUnitIds);
  const clearRecentlyDestroyedUnits = useGameStore((state) => state.clearRecentlyDestroyedUnits);

  const renderedHpsRef = useRef<Record<string, number>>({});
  const [renderedHps, setRenderedHps] = useState<Record<string, number>>({});

  const explosionsRef = useRef<Record<string, ExplosionState>>({});
  const [explosions, setExplosions] = useState<Record<string, ExplosionState>>({});

  const prevUnitsRef = useRef<typeof unitData>([]);

  useEffect(() => {
    const prevUnits = prevUnitsRef.current;
    const currentIds = new Set(unitData.map((u) => u.id));

    // 撃破されたユニットがある場合、爆発エフェクトを開始する
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

    // 初期HPの設定
    for (const unit of unitData) {
      if (renderedHpsRef.current[unit.id] === undefined) {
        renderedHpsRef.current[unit.id] = unit.hp;
      }
    }
    setRenderedHps({ ...renderedHpsRef.current });

    prevUnitsRef.current = unitData;
  }, [unitData, recentlyDestroyedUnitIds, clearRecentlyDestroyedUnits]);

  // Pixiのフレーム毎のアップデート
  useTick((delta) => {
    const step = HP_STEP * delta;

    // HP減少時のスムーズな補間アニメーション
    let hpChanged = false;
    for (const unit of unitData) {
      const currentRendered = renderedHpsRef.current[unit.id] ?? unit.hp;
      if (Math.abs(currentRendered - unit.hp) > HP_THRESHOLD) {
        if (currentRendered > unit.hp) {
          renderedHpsRef.current[unit.id] = Math.max(
            unit.hp,
            currentRendered - HP_SPEED_COEFF * step,
          );
        } else {
          renderedHpsRef.current[unit.id] = unit.hp;
        }
        hpChanged = true;
      }
    }
    if (hpChanged) {
      setRenderedHps({ ...renderedHpsRef.current });
    }

    // 爆発アニメーションの進捗更新
    let explosionsChanged = false;
    const nextExplosions = { ...explosionsRef.current };
    for (const id in nextExplosions) {
      const exp = nextExplosions[id];
      if (exp.progress < 1) {
        exp.progress = Math.min(1, exp.progress + step * EXPLOSION_SPEED_COEFF);
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
      {/* ユニットの描画 */}
      {unitData.map((unit) => {
        const isSelectedAndMoving =
          selectedUnitId === unit.id &&
          [
            "action_menu",
            "target_selection",
            "drop_unit_selection",
            "drop_target_selection",
          ].includes(interactionState) &&
          selectedTargetPos;
        const targetX = isSelectedAndMoving ? selectedTargetPos.x : unit.x;
        const targetY = isSelectedAndMoving ? selectedTargetPos.y : unit.y;

        const isHexOddRow = topology === "hex" && targetY % 2 !== 0;
        const offsetX = isHexOddRow ? TILE_SIZE / 2 : 0;
        const px = targetX * TILE_SIZE + offsetX;
        const py = targetY * TILE_SIZE;

        const displayHp = renderedHps[unit.id] ?? unit.hp;

        return (
          <Container key={unit.id} x={px} y={py}>
            <Sprite
              image={`/assets/units/${unit.faction}/${UNIT_IMAGE_MAP[unit.type] || "infantry"}.png`}
              width={TILE_SIZE}
              height={TILE_SIZE}
            />

            {/* L: 輸送車等に積載中であることを表すバッジ */}
            {unit.is_loaded && (
              <Container x={TILE_SIZE - BADGE_SIZE} y={TILE_SIZE - BADGE_SIZE}>
                <Graphics
                  draw={(g) => {
                    g.clear();
                    g.beginFill(LOADED_BADGE_BG_COLOR, LOADED_BADGE_BG_ALPHA);
                    g.drawRoundedRect(0, 0, BADGE_SIZE, BADGE_SIZE, BADGE_RADIUS);
                    g.endFill();
                  }}
                />
                <Text
                  text="L"
                  style={
                    new TextStyle({ fill: "white", fontSize: BADGE_FONT_SIZE, fontWeight: "bold" })
                  }
                  x={4}
                  y={1}
                />
              </Container>
            )}

            {/* E: ターン内の行動を完了したことを表すバッジ */}
            {unit.is_exhausted && (
              <Container x={TILE_SIZE - BADGE_SIZE} y={0}>
                <Graphics
                  draw={(g) => {
                    g.clear();
                    g.beginFill(EXHAUSTED_BADGE_BG_COLOR, EXHAUSTED_BADGE_BG_ALPHA);
                    g.drawRoundedRect(0, 0, BADGE_SIZE, BADGE_SIZE, BADGE_RADIUS);
                    g.endFill();
                  }}
                />
                <Text
                  text="E"
                  style={
                    new TextStyle({ fill: "white", fontSize: BADGE_FONT_SIZE, fontWeight: "bold" })
                  }
                  x={4}
                  y={1}
                />
              </Container>
            )}

            {/* 耐久力HPバー (ユニット画像の下部) */}
            <Graphics
              draw={(g) => {
                g.clear();
                g.beginFill(HP_BAR_BG_COLOR, HP_BAR_BG_ALPHA);
                g.drawRect(
                  HP_BAR_MARGIN_X,
                  TILE_SIZE - HP_BAR_OFFSET_Y_FROM_BOTTOM,
                  TILE_SIZE - 2 * HP_BAR_MARGIN_X,
                  HP_BAR_HEIGHT,
                );
                g.endFill();

                const ratio = displayHp / HP_MAX;
                let color = HP_COLOR_GOOD; // 緑
                if (displayHp < UNIT_HP_THRESHOLD_DANGER) {
                  color = HP_COLOR_DANGER; // 赤
                } else if (displayHp < UNIT_HP_THRESHOLD_WARNING) {
                  color = HP_COLOR_WARNING; // 橙
                }

                g.beginFill(color);
                g.drawRect(
                  HP_BAR_MARGIN_X,
                  TILE_SIZE - HP_BAR_OFFSET_Y_FROM_BOTTOM,
                  (TILE_SIZE - 2 * HP_BAR_MARGIN_X) * ratio,
                  HP_BAR_HEIGHT,
                );
                g.endFill();
              }}
            />
          </Container>
        );
      })}

      {/* 爆発エフェクトの描画 */}
      {Object.entries(explosions).map(([id, exp]) => {
        const isHexOddRow = topology === "hex" && exp.y % 2 !== 0;
        const offsetX = isHexOddRow ? TILE_SIZE / 2 : 0;
        const px = exp.x * TILE_SIZE + offsetX;
        const py = exp.y * TILE_SIZE;

        const size = TILE_SIZE * EXPLOSION_MAX_SCALE * exp.progress;
        const offset = (TILE_SIZE - size) / 2;
        const alpha =
          exp.progress < EXPLOSION_FADE_THRESHOLD_START
            ? 1.0
            : (1.0 - exp.progress) / EXPLOSION_FADE_THRESHOLD_DIV;

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
