import { Container, Graphics, Sprite, useTick } from "@pixi/react";
import { useEffect, useRef, useState } from "react";
import { PRODUCIBLE_TERRAINS, TERRAIN_IMAGE_MAP } from "../../../constants/mappings";
import {
  CAPTURE_BAR_BG_ALPHA,
  CAPTURE_BAR_BG_COLOR,
  CAPTURE_SPEED_COEFF,
  CAPTURE_STEP,
  CAPTURE_THRESHOLD,
  HP_COLOR_DANGER,
  HP_COLOR_GOOD,
  HP_COLOR_WARNING,
  MAP_HP_THRESHOLD_DANGER,
  MAP_HP_THRESHOLD_WARNING,
  TILE_SIZE,
} from "../../../constants/rendering";
import { useGameStore } from "../../../store/gameStore";

/** 占領バー描画用のレイアウト定数 */
const BAR_OFFSET_X = 4;
const BAR_OFFSET_Y = 4;
const BAR_WIDTH_MARGIN = 8;
const BAR_HEIGHT = 6;
const BAR_INNER_MARGIN = 10;
const BAR_INNER_HEIGHT = 4;
const BAR_INNER_OFFSET_X = 1;
const BAR_INNER_OFFSET_Y = 1;

/**
 * 地形のセルタイプと所属からアセット画像パスを取得する
 * @param cell 地形種別
 * @param propertyOwner 所有勢力
 */
const getTerrainImagePath = (cell: string, propertyOwner = "neutral") => {
  if (PRODUCIBLE_TERRAINS.includes(cell)) {
    const filename = cell === "capital" ? "hq" : cell;
    return `/assets/properties/${propertyOwner}/${filename}.png`;
  }

  const mapped = TERRAIN_IMAGE_MAP[cell] || "plain";
  return `/assets/terrains/${mapped}.png`;
};

/**
 * マップレイヤーコンポーネント
 * 地形と占領されている施設の占領度ゲージを描画します。
 */
export const MapLayer = () => {
  // Zustand から必要な状態のみを個別に購読する (セレクタを使用)
  const mapData = useGameStore((state) => state.mapData);
  const propertyData = useGameStore((state) => state.propertyData);
  const topology = useGameStore((state) => state.topology);

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

  // 耐久値減少のアニメーション補間処理
  useTick((delta) => {
    const step = CAPTURE_STEP * delta;
    let changed = false;

    for (const p of propertyData) {
      const key = `${p.x}-${p.y}`;
      const current = renderedPointsRef.current[key] ?? p.capture_points;

      if (Math.abs(current - p.capture_points) > CAPTURE_THRESHOLD) {
        if (current > p.capture_points) {
          // 最大値（p.max_capture_points）に対する割合ベースで減少量を算出し、HP減少アニメーションと速度感を統一する
          const decreaseAmount = Math.max(
            0.1,
            (p.max_capture_points * CAPTURE_SPEED_COEFF * step) / 10,
          );
          renderedPointsRef.current[key] = Math.max(p.capture_points, current - decreaseAmount);
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
      {mapData.map((row, y) =>
        row.map((cell, x) => {
          const isHexOddRow = topology === "hex" && y % 2 !== 0;
          const offsetX = isHexOddRow ? TILE_SIZE / 2 : 0;
          const px = x * TILE_SIZE + offsetX;
          const py = y * TILE_SIZE;
          const property = propertyData.find((p) => p.x === x && p.y === y);

          return (
            // biome-ignore lint/suspicious/noArrayIndexKey: 座標情報を元にしたユニークキーであるためインデックスの利用が妥当です
            <Container key={`cell-${x}-${y}`} x={px} y={py}>
              <Sprite
                image={getTerrainImagePath(cell, property?.owner || "neutral")}
                width={TILE_SIZE}
                height={TILE_SIZE}
              />
              {property &&
                property.capture_points < property.max_capture_points &&
                property.max_capture_points > 0 && (
                  <Graphics
                    x={BAR_OFFSET_X}
                    y={BAR_OFFSET_Y}
                    draw={(g) => {
                      const key = `${property.x}-${property.y}`;
                      const displayPoints = renderedPoints[key] ?? property.capture_points;

                      g.clear();
                      g.beginFill(CAPTURE_BAR_BG_COLOR, CAPTURE_BAR_BG_ALPHA);
                      g.drawRect(0, 0, TILE_SIZE - BAR_WIDTH_MARGIN, BAR_HEIGHT);
                      g.endFill();

                      const ratio = displayPoints / property.max_capture_points;
                      let barColor = HP_COLOR_GOOD; // 緑
                      if (ratio < MAP_HP_THRESHOLD_DANGER) {
                        barColor = HP_COLOR_DANGER; // 赤
                      } else if (ratio < MAP_HP_THRESHOLD_WARNING) {
                        barColor = HP_COLOR_WARNING; // 橙
                      }

                      g.beginFill(barColor);
                      g.drawRect(
                        BAR_INNER_OFFSET_X,
                        BAR_INNER_OFFSET_Y,
                        (TILE_SIZE - BAR_INNER_MARGIN) * ratio,
                        BAR_INNER_HEIGHT,
                      );
                      g.endFill();
                    }}
                  />
                )}
            </Container>
          );
        }),
      )}
    </Container>
  );
};
