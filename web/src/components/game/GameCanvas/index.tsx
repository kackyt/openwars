import { Stage, Container, Sprite } from '@pixi/react';
import * as PIXI from 'pixi.js';
import { MapLayer } from '../MapLayer';
import { UnitLayer } from '../UnitLayer';
import { CursorLayer } from '../CursorLayer';
import { ReachableLayer } from '../ReachableLayer';
import { useGameStore } from '../../../store/gameStore';
import { useState, useEffect } from 'react';

const TILE_SIZE = 48;

export const GameCanvas = () => {
  const { 
    mapData, unitData, 
    interactionState, reachableCells, attackableTargets,
    setHoveredCell, selectUnit, selectMoveTarget, executeAction, cancelInteraction, openProduceMenu,
    executeDropTarget,
    topology 
  } = useGameStore();
  
  const [windowSize, setWindowSize] = useState({ width: window.innerWidth, height: window.innerHeight });
  useEffect(() => {
    const handleResize = () => setWindowSize({ width: window.innerWidth, height: window.innerHeight });
    window.addEventListener('resize', handleResize);
    return () => window.removeEventListener('resize', handleResize);
  }, []);

  const [hoverX, setHoverX] = useState(-1);
  const [hoverY, setHoverY] = useState(-1);

  const [isDragging, setIsDragging] = useState(false);
  const [dragStart, setDragStart] = useState({ x: 0, y: 0 });
  const [pointerStart, setPointerStart] = useState({ x: 0, y: 0 });
  const [cameraPos, setCameraPos] = useState({ x: 0, y: 0 });

  const getCellData = (globalX: number, globalY: number) => {
    const localX = globalX - cameraPos.x;
    const localY = globalY - cameraPos.y;
    
    const gridY = Math.floor(localY / TILE_SIZE);
    let offsetX = 0;
    if (topology === 'hex' && gridY % 2 !== 0) {
      offsetX = TILE_SIZE / 2;
    }
    const gridX = Math.floor((localX - offsetX) / TILE_SIZE);
    
    if (gridY < 0 || gridY >= mapData.length || gridX < 0 || gridX >= (mapData[0]?.length || 0)) {
      return null;
    }
    
    const cellType = mapData[gridY]?.[gridX] || 'unknown';
    const unit = unitData.find(u => u.x === gridX && u.y === gridY) || null;
    return { gridX, gridY, cellType, unit };
  };

  const clampCameraPos = (newX: number, newY: number) => {
    const mapWidth = (mapData[0]?.length || 0) * TILE_SIZE;
    const mapHeight = mapData.length * TILE_SIZE;
    const minX = Math.min(0, windowSize.width - mapWidth);
    const minY = Math.min(0, windowSize.height - mapHeight);
    
    return {
      x: Math.max(minX, Math.min(0, newX)),
      y: Math.max(minY, Math.min(0, newY))
    };
  };

  const handlePointerDown = (e: any) => {
    setIsDragging(true);
    setDragStart({ x: e.data.global.x - cameraPos.x, y: e.data.global.y - cameraPos.y });
    setPointerStart({ x: e.data.global.x, y: e.data.global.y });
  };

  const handlePointerMove = (e: any) => {
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

  const handlePointerUp = (e: any) => {
    setIsDragging(false);
    
    const dx = e.data.global.x - pointerStart.x;
    const dy = e.data.global.y - pointerStart.y;
    const dist = Math.abs(dx) + Math.abs(dy);
    
    if (dist < 10) {
      const cellData = getCellData(e.data.global.x, e.data.global.y);
      if (!cellData) return;
      const { gridX, gridY, unit, cellType } = cellData;

      if (interactionState === 'idle') {
        if (unit) {
          selectUnit(unit.id);
        } else if (['factory', 'airport', 'port', 'capital', 'city'].includes(cellType)) {
          openProduceMenu(gridX, gridY);
        } else {
          cancelInteraction();
        }
      } else if (interactionState === 'unit_selected') {
        const isReachable = reachableCells.some(c => c.x === gridX && c.y === gridY);
        if (isReachable) {
          selectMoveTarget(gridX, gridY);
        } else {
          cancelInteraction();
        }
      } else if (interactionState === 'target_selection') {
        const target = attackableTargets.find(t => t.x === gridX && t.y === gridY);
        if (target) {
          executeAction('Attack', target.id);
        } else {
          cancelInteraction();
        }
      } else if (interactionState === 'drop_target_selection') {
        // 降車先マスのタップ: ハイライト済みマスをタップしたら降車コマンドを送信する
        const isDroppable = reachableCells.some(c => c.x === gridX && c.y === gridY);
        if (isDroppable) {
          executeDropTarget(gridX, gridY);
        } else {
          cancelInteraction();
        }
      } else if (interactionState === 'action_menu' || interactionState === 'produce_menu' || interactionState === 'drop_unit_selection') {
        cancelInteraction();
      }
    }
  };

  const handleWheel = (e: any) => {
    // prevent default behavior in native events if necessary
    setCameraPos(prev => clampCameraPos(prev.x - e.deltaX, prev.y - e.deltaY));
  };

  return (
    <Stage width={windowSize.width} height={windowSize.height} options={{ backgroundColor: 0x1099bb }} onWheel={handleWheel}>
      {/* @ts-ignore */}
      <Container 
        interactive={true} 
        pointerdown={handlePointerDown} 
        pointermove={handlePointerMove}
        pointerup={handlePointerUp}
        pointerupoutside={handlePointerUp}
      >
        <Sprite texture={PIXI.Texture.WHITE} width={windowSize.width} height={windowSize.height} alpha={0} />
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
