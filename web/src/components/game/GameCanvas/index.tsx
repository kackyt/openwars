import { Container, Sprite, Stage } from "@pixi/react";
import * as PIXI from "pixi.js";
import { useEffect, useState } from "react";
import { PRODUCIBLE_TERRAINS } from "../../../constants/mappings";
import {
  DRAG_THRESHOLD,
  POPUP_MENU_APPROX_HEIGHT,
  POPUP_MENU_APPROX_WIDTH,
  POPUP_MENU_MIN_MARGIN,
  STAGE_BACKGROUND_COLOR,
  TILE_SIZE,
} from "../../../constants/rendering";
import { useGameStore } from "../../../store/gameStore";
import { clampCameraPosition, globalToGrid, gridToGlobal } from "../../../utils/camera";
import { CursorLayer } from "../CursorLayer";
import { MapLayer } from "../MapLayer";
import { ReachableLayer } from "../ReachableLayer";
import { UnitLayer } from "../UnitLayer";

/**
 * ゲームキャンバスコンポーネント
 * PixiJSのStage上にマップやユニット、カーソル等のゲーム盤面を描画し、
 * ドラッグによるカメラ移動やクリックによる操作イベントのハンドリングを行います。
 */
export const GameCanvas = () => {
  // Zustand のセレクタを用いて必要な状態のみを個別に購読する (再描画の抑制)
  const mapData = useGameStore((state) => state.mapData);
  const unitData = useGameStore((state) => state.unitData);
  const interactionState = useGameStore((state) => state.interactionState);
  const reachableCells = useGameStore((state) => state.reachableCells);
  const attackableTargets = useGameStore((state) => state.attackableTargets);
  const topology = useGameStore((state) => state.topology);
  const setHoveredCell = useGameStore((state) => state.setHoveredCell);
  const selectUnit = useGameStore((state) => state.selectUnit);
  const selectMoveTarget = useGameStore((state) => state.selectMoveTarget);
  const executeAction = useGameStore((state) => state.executeAction);
  const cancelInteraction = useGameStore((state) => state.cancelInteraction);
  const openProduceMenu = useGameStore((state) => state.openProduceMenu);
  const executeDropTarget = useGameStore((state) => state.executeDropTarget);

  const [windowSize, setWindowSize] = useState({
    width: window.innerWidth,
    height: window.innerHeight,
  });

  // ウィンドウサイズ変更時にPixiのStageサイズを追従させる
  useEffect(() => {
    const handleResize = () =>
      setWindowSize({ width: window.innerWidth, height: window.innerHeight });
    window.addEventListener("resize", handleResize);
    return () => window.removeEventListener("resize", handleResize);
  }, []);

  const [hoverX, setHoverX] = useState(-1);
  const [hoverY, setHoverY] = useState(-1);

  const [isDragging, setIsDragging] = useState(false);
  const [dragStart, setDragStart] = useState({ x: 0, y: 0 });
  const [pointerStart, setPointerStart] = useState({ x: 0, y: 0 });
  const [cameraPos, setCameraPos] = useState({ x: 0, y: 0 });

  /**
   * 画面上の絶対座標 (px) からグリッド上のセル情報・座標・ユニットを取得する
   * @param globalX 画面絶対X座標
   * @param globalY 画面絶対Y座標
   */
  const getCellData = (globalX: number, globalY: number) => {
    const { gridX, gridY } = globalToGrid(
      globalX,
      globalY,
      cameraPos.x,
      cameraPos.y,
      topology,
      TILE_SIZE,
    );

    // グリッド範囲外チェック
    if (gridY < 0 || gridY >= mapData.length || gridX < 0 || gridX >= (mapData[0]?.length || 0)) {
      return null;
    }

    const cellType = mapData[gridY]?.[gridX] || "unknown";
    const unit = unitData.find((u) => u.x === gridX && u.y === gridY) || null;
    return { gridX, gridY, cellType, unit };
  };

  /**
   * カメラ（スクロール）位置がマップ描画範囲外に行かないようにクランプ（制限）する
   * @param newX クランプ前のX座標
   * @param newY クランプ前のY座標
   */
  const clampCameraPos = (newX: number, newY: number) => {
    const isHex = topology === "hex";
    const mapWidth = (mapData[0]?.length || 0) * TILE_SIZE + (isHex ? TILE_SIZE / 2 : 0);
    const mapHeight = mapData.length * TILE_SIZE;
    return clampCameraPosition(
      newX,
      newY,
      mapWidth,
      mapHeight,
      windowSize.width,
      windowSize.height,
    );
  };

  /**
   * 指定のグリッド座標における、画面上の表示ピクセル位置（ポップアップ用）を取得する
   * @param gridX グリッドX
   * @param gridY グリッドY
   */
  const getScreenPos = (gridX: number, gridY: number) => {
    const { globalX, globalY } = gridToGlobal(
      gridX,
      gridY,
      cameraPos.x,
      cameraPos.y,
      topology,
      TILE_SIZE,
    );
    // メニュー表示位置が画面外（右端・下端）にはみ出さないようクランプ
    const screenX = Math.min(
      Math.max(POPUP_MENU_MIN_MARGIN, globalX + TILE_SIZE / 2),
      windowSize.width - POPUP_MENU_APPROX_WIDTH,
    );
    const screenY = Math.min(
      Math.max(POPUP_MENU_MIN_MARGIN, globalY),
      windowSize.height - POPUP_MENU_APPROX_HEIGHT,
    );
    return { screenX, screenY };
  };

  /** ポインターが押された（ドラッグ開始）時のイベントハンドラー */
  const handlePointerDown = (e: PIXI.FederatedPointerEvent) => {
    setIsDragging(true);
    setDragStart({ x: e.data.global.x - cameraPos.x, y: e.data.global.y - cameraPos.y });
    setPointerStart({ x: e.data.global.x, y: e.data.global.y });
  };

  /** ポインターが動いた時のイベントハンドラー */
  const handlePointerMove = (e: PIXI.FederatedPointerEvent) => {
    if (isDragging) {
      const newX = e.data.global.x - dragStart.x;
      const newY = e.data.global.y - dragStart.y;
      setCameraPos(clampCameraPos(newX, newY));
    }

    const cellData = getCellData(e.data.global.x, e.data.global.y);
    if (cellData) {
      const { gridX, gridY } = cellData;
      if (gridX !== hoverX || gridY !== hoverY) {
        setHoverX(gridX);
        setHoverY(gridY);
        setHoveredCell(gridX, gridY);
      }
    } else {
      setHoverX(-1);
      setHoverY(-1);
    }
  };

  /** ポインターが離された（ドラッグ終了 / クリック判定）時のイベントハンドラー */
  const handlePointerUp = (e: PIXI.FederatedPointerEvent) => {
    setIsDragging(false);

    const dx = e.data.global.x - pointerStart.x;
    const dy = e.data.global.y - pointerStart.y;
    const dist = Math.abs(dx) + Math.abs(dy);

    // ドラッグ距離が閾値未満であれば「クリック（タップ）」とみなす
    if (dist < DRAG_THRESHOLD) {
      const cellData = getCellData(e.data.global.x, e.data.global.y);
      if (!cellData) return;
      const { gridX, gridY, unit, cellType } = cellData;

      // AI思考中は一切のプレイヤー入力を無視
      if (interactionState === "ai_thinking") {
        return;
      }

      // インタラクションの状態マシン
      if (interactionState === "idle") {
        if (unit) {
          // ユニットがタップされたら選択状態に
          selectUnit(unit.id);
        } else if (PRODUCIBLE_TERRAINS.includes(cellType)) {
          // 生産可能な拠点がタップされたら生産メニューを開く
          const { screenX, screenY } = getScreenPos(gridX, gridY);
          openProduceMenu(gridX, gridY, screenX, screenY);
        } else {
          cancelInteraction();
        }
      } else if (interactionState === "unit_selected") {
        const isReachable = reachableCells.some((c) => c.x === gridX && c.y === gridY);
        if (isReachable) {
          // 移動範囲内であれば目的地を選択
          const { screenX, screenY } = getScreenPos(gridX, gridY);
          selectMoveTarget(gridX, gridY, screenX, screenY);
        } else {
          cancelInteraction();
        }
      } else if (interactionState === "target_selection") {
        const target = attackableTargets.find((t) => t.x === gridX && t.y === gridY);
        if (target) {
          // 攻撃可能ターゲットを選択したら攻撃実行
          executeAction("Attack", target.id);
        } else {
          cancelInteraction();
        }
      } else if (interactionState === "drop_target_selection") {
        // 降車可能マスが選択されたら降車処理実行
        const isDroppable = reachableCells.some((c) => c.x === gridX && c.y === gridY);
        if (isDroppable) {
          executeDropTarget(gridX, gridY);
        } else {
          cancelInteraction();
        }
      } else if (
        interactionState === "action_menu" ||
        interactionState === "produce_menu" ||
        interactionState === "drop_unit_selection"
      ) {
        cancelInteraction();
      }
    }
  };

  /** マウスホイール（スクロール）によるカメラ移動のハンドラー */
  const handleWheel = (e: React.WheelEvent<HTMLCanvasElement>) => {
    setCameraPos((prev) => clampCameraPos(prev.x - e.deltaX, prev.y - e.deltaY));
  };

  return (
    <Stage
      width={windowSize.width}
      height={windowSize.height}
      options={{ backgroundColor: STAGE_BACKGROUND_COLOR }}
      onWheel={handleWheel}
    >
      <Container
        interactive={true}
        pointerdown={handlePointerDown}
        pointermove={handlePointerMove}
        pointerup={handlePointerUp}
        pointerupoutside={handlePointerUp}
      >
        <Sprite
          texture={PIXI.Texture.WHITE}
          width={windowSize.width}
          height={windowSize.height}
          alpha={0}
        />
        <Container x={cameraPos.x} y={cameraPos.y}>
          <MapLayer />
          <ReachableLayer tileSize={TILE_SIZE} />
          <UnitLayer />
          {hoverX >= 0 && hoverY >= 0 && <CursorLayer x={hoverX} y={hoverY} tileSize={TILE_SIZE} />}
        </Container>
      </Container>
    </Stage>
  );
};
