import { create } from 'zustand';
import * as Comlink from 'comlink';
import type { EngineWorker } from '../worker/engineWorker';

interface UnitData {
  id: string;
  type: string;
  faction: string;
  x: number;
  y: number;
  hp: number;
  is_loaded: boolean;
}

interface TurnInfo {
  turn: number;
  phase: string;
  funds: number;
}

interface PropertyData {
  x: number;
  y: number;
  type: string;
  owner: string;
  capture_points: number;
  max_capture_points: number;
}

interface ActionMenuState {
  x: number;
  y: number;
  unitId: string;
  actions: string[];
}

interface ProduceMenuState {
  x: number;
  y: number;
  units: { type: string, name: string, cost: number }[];
}

interface GameState {
  appState: 'menu' | 'playing';
  topology: 'square' | 'hex';
  p1IsAi: boolean;
  p2IsAi: boolean;
  isEngineReady: boolean;
  engineWorker: Comlink.Remote<EngineWorker> | null;
  mapData: string[][];
  unitData: UnitData[];
  turnInfo: TurnInfo | null;
  propertyData: PropertyData[];
  terrainDefs: Record<string, number>;
  gameOver: { winner: number } | { draw: boolean } | null;
  
  // UI / Interaction State
  interactionState: 'idle' | 'unit_selected' | 'action_menu' | 'target_selection' | 'produce_menu' | 'drop_unit_selection' | 'drop_target_selection' | 'ai_thinking';
  selectedUnitId: string | null;
  reachableCells: { x: number, y: number }[];
  attackableTargets: { id: string, x: number, y: number }[];
  selectedTargetPos: { x: number, y: number } | null;
  produceMenu: ProduceMenuState | null;
  // 降車フロー用: 積載ユニット一覧と選択されたユニットIDを保持する
  loadedUnits: { id: string, type: string }[];
  dropCargoId: string | null;

  hoveredCellX: number;
  hoveredCellY: number;
  hoveredTerrain: { type: string, def?: number } | null;
  hoveredUnit: UnitData | null;
  actionMenu: ActionMenuState | null;

  // Actions
  initEngine: (mapName: string, topology: string, p1IsAi: boolean, p2IsAi: boolean) => Promise<void>;
  syncGameState: () => Promise<void>;
  tickAiTurn: () => Promise<void>;
  setHoveredCell: (x: number, y: number) => void;
  openActionMenu: (x: number, y: number, unitId: string, actions: string[]) => void;
  closeActionMenu: () => void;
  
  // Interaction Actions
  selectUnit: (unitId: string) => Promise<void>;
  selectMoveTarget: (x: number, y: number) => Promise<void>;
  cancelInteraction: () => void;
  executeAction: (actionType: string, targetId?: string) => Promise<void>;
  openProduceMenu: (x: number, y: number) => Promise<void>;
  closeProduceMenu: () => void;
  executeProduce: (unitType: string, x: number, y: number) => Promise<void>;
  // 降車フロー用アクション
  openDropMenu: () => Promise<void>;
  selectDropCargo: (cargoId: string) => Promise<void>;
  executeDropTarget: (x: number, y: number) => Promise<void>;
  endTurn: () => Promise<void>;
}

export const useGameStore = create<GameState>((set, get) => ({
  appState: 'menu',
  topology: 'square',
  p1IsAi: false,
  p2IsAi: false,
  isEngineReady: false,
  engineWorker: null,
  mapData: [],
  unitData: [],
  turnInfo: null,
  propertyData: [],
  terrainDefs: {},
  gameOver: null,
  
  interactionState: 'idle',
  selectedUnitId: null,
  reachableCells: [],
  attackableTargets: [],
  selectedTargetPos: null,
  produceMenu: null,
  loadedUnits: [],
  dropCargoId: null,

  hoveredCellX: -1,
  hoveredCellY: -1,
  hoveredTerrain: null,
  hoveredUnit: null,
  actionMenu: null,

  initEngine: async (mapName, topology, p1IsAi, p2IsAi) => {
    try {
      const worker = new Worker(new URL('../worker/engineWorker.ts', import.meta.url), {
        type: 'module',
      });
      const engineClass = Comlink.wrap<typeof EngineWorker>(worker);
      const engineWorker = await new engineClass();
      await engineWorker.init(mapName, topology);
      
      set({ 
        engineWorker, 
        isEngineReady: true, 
        appState: 'playing', 
        topology: topology as 'square' | 'hex',
        p1IsAi,
        p2IsAi
      });
      await get().syncGameState();
      
      const { turnInfo, p1IsAi: isP1Ai, p2IsAi: isP2Ai } = get();
      if (turnInfo) {
        const isAiTurn = (turnInfo.phase === 'P1' && isP1Ai) || (turnInfo.phase === 'P2' && isP2Ai);
        if (isAiTurn) {
          get().tickAiTurn();
        }
      }
    } catch (e) {
      console.error("Failed to initialize engine:", e);
    }
  },

  syncGameState: async () => {
    const { engineWorker } = get();
    if (!engineWorker) return;

    try {
      const [mapData, unitData, turnInfo, propertyData, terrainDefs, gameOver] = await Promise.all([
        engineWorker.getMap(),
        engineWorker.getUnits(),
        engineWorker.getTurnInfo(),
        engineWorker.getProperties(),
        engineWorker.getTerrainDefs(),
        engineWorker.checkGameOver(),
      ]);

      set({ mapData, unitData, turnInfo, propertyData, terrainDefs, gameOver });
    } catch (e) {
      console.error("Failed to sync game state:", e);
    }
  },

  setHoveredCell: (x: number, y: number) => {
    const { mapData, unitData, terrainDefs } = get();
    const cellType = mapData[y]?.[x] || 'unknown';
    const unit = unitData.find(u => u.x === x && u.y === y) || null;
    
    set({
      hoveredCellX: x,
      hoveredCellY: y,
      hoveredTerrain: { type: cellType, def: terrainDefs[cellType] || 0 },
      hoveredUnit: unit
    });
  },

  openActionMenu: (x: number, y: number, unitId: string, actions: string[]) => {
    set({ actionMenu: { x, y, unitId, actions } });
  },

  closeActionMenu: () => {
    set({ actionMenu: null });
  },

  selectUnit: async (unitId: string) => {
    const { engineWorker } = get();
    if (!engineWorker) return;
    const cells = await engineWorker.getReachableCells(unitId);
    set({
      interactionState: 'unit_selected',
      selectedUnitId: unitId,
      reachableCells: cells,
    });
  },

  selectMoveTarget: async (x: number, y: number) => {
    const { engineWorker, selectedUnitId } = get();
    if (!engineWorker || !selectedUnitId) return;
    const actions = await engineWorker.getAvailableActions(selectedUnitId, x, y);
    set({
      interactionState: 'action_menu',
      selectedTargetPos: { x, y },
    });
    get().openActionMenu(x, y, selectedUnitId, actions);
  },

  cancelInteraction: () => {
    set({
      interactionState: 'idle',
      selectedUnitId: null,
      reachableCells: [],
      attackableTargets: [],
      selectedTargetPos: null,
      produceMenu: null,
      actionMenu: null,
      loadedUnits: [],
      dropCargoId: null,
    });
  },

  executeAction: async (actionType: string, targetId?: string) => {
    const { engineWorker, selectedUnitId, selectedTargetPos } = get();
    if (!engineWorker || !selectedUnitId || !selectedTargetPos) return;

    if (actionType === 'Wait') {
      await engineWorker.submitMoveCommand(selectedUnitId, selectedTargetPos.x, selectedTargetPos.y);
      await engineWorker.submitWaitCommand(selectedUnitId);
    } else if (actionType === 'Attack') {
      if (!targetId) {
        const targets = await engineWorker.getAttackableTargets(selectedUnitId, selectedTargetPos.x, selectedTargetPos.y);
        set({
          interactionState: 'target_selection',
          attackableTargets: targets,
          actionMenu: null,
        });
        return;
      } else {
        await engineWorker.submitMoveCommand(selectedUnitId, selectedTargetPos.x, selectedTargetPos.y);
        await engineWorker.submitAttackCommand(selectedUnitId, targetId);
      }
    } else if (actionType === 'Capture') {
      await engineWorker.submitMoveCommand(selectedUnitId, selectedTargetPos.x, selectedTargetPos.y);
      await engineWorker.submitCaptureCommand(selectedUnitId);
    } else if (actionType === 'Load') {
      await engineWorker.submitMoveCommand(selectedUnitId, selectedTargetPos.x, selectedTargetPos.y);
      const targetUnit = get().unitData.find(u => u.x === selectedTargetPos.x && u.y === selectedTargetPos.y && u.id !== selectedUnitId);
      if (targetUnit) {
        await engineWorker.submitLoadCommand(selectedUnitId, targetUnit.id);
      }
    } else if (actionType === 'Merge') {
      // 移動先に同じ勢力・同種のユニットを探して合流する
      await engineWorker.submitMoveCommand(selectedUnitId, selectedTargetPos.x, selectedTargetPos.y);
      const targetUnit = get().unitData.find(u => u.x === selectedTargetPos.x && u.y === selectedTargetPos.y && u.id !== selectedUnitId);
      if (targetUnit) {
        await engineWorker.submitMergeCommand(selectedUnitId, targetUnit.id);
      }
    } else if (actionType === 'Drop') {
      // 移動コマンドは送信済みの状態で、降車する積載ユニットの選択メニューを開く
      await engineWorker.submitMoveCommand(selectedUnitId, selectedTargetPos.x, selectedTargetPos.y);
      await get().openDropMenu();
      return; // まだ cancelInteraction / syncGameState は呼ばない
    }

    get().cancelInteraction();
    await get().syncGameState();
  },

  openProduceMenu: async (x: number, y: number) => {
    const { engineWorker } = get();
    if (!engineWorker) return;
    const units = await engineWorker.getProducibleUnits(x, y);
    if (units && units.length > 0) {
      set({
        interactionState: 'produce_menu',
        produceMenu: { x, y, units },
      });
    }
  },

  closeProduceMenu: () => {
    set({ interactionState: 'idle', produceMenu: null });
  },

  executeProduce: async (unitType: string, x: number, y: number) => {
    const { engineWorker } = get();
    if (!engineWorker) return;
    await engineWorker.submitProduceCommand(unitType, x, y);
    get().closeProduceMenu();
    await get().syncGameState();
  },

  // 積載ユニット一覧を取得して降車ユニット選択モードに遷移する
  openDropMenu: async () => {
    const { engineWorker, selectedUnitId } = get();
    if (!engineWorker || !selectedUnitId) return;
    const loadedUnits = await engineWorker.getLoadedUnits(selectedUnitId);
    set({ interactionState: 'drop_unit_selection', loadedUnits, actionMenu: null });
  },

  // 降車するユニットを選択し、降ろせるマスのハイライトに遷移する
  selectDropCargo: async (cargoId: string) => {
    const { engineWorker, selectedUnitId } = get();
    if (!engineWorker || !selectedUnitId) return;
    const droppableTiles = await engineWorker.getDroppableTiles(selectedUnitId, cargoId);
    set({
      interactionState: 'drop_target_selection',
      dropCargoId: cargoId,
      reachableCells: droppableTiles, // 降車可能マスをハイライト表示に流用する
    });
  },

  // 降車先マスを選択して降車コマンドを送信する
  executeDropTarget: async (x: number, y: number) => {
    const { engineWorker, selectedUnitId, dropCargoId } = get();
    if (!engineWorker || !selectedUnitId || !dropCargoId) return;
    await engineWorker.submitUnloadCommand(selectedUnitId, dropCargoId, x, y);
    get().cancelInteraction();
    await get().syncGameState();
  },

  tickAiTurn: async () => {
    const { engineWorker } = get();
    if (!engineWorker) return;

    set({ interactionState: 'ai_thinking', selectedUnitId: null, actionMenu: null, produceMenu: null });

    while (true) {
      const acted = await engineWorker.executeAiTurn();
      await get().syncGameState();
      if (!acted) break;
    }

    await get().endTurn();
  },

  endTurn: async () => {
    const { engineWorker } = get();
    if (!engineWorker) return;

    const gameOverObj = await engineWorker.checkGameOver();
    if (gameOverObj) return;

    await engineWorker.endTurn();
    await get().syncGameState();
    
    const { turnInfo, p1IsAi, p2IsAi } = get();
    if (turnInfo) {
      const isAiTurn = (turnInfo.phase === 'P1' && p1IsAi) || (turnInfo.phase === 'P2' && p2IsAi);
      if (isAiTurn) {
        get().tickAiTurn();
      } else {
        set({ interactionState: 'idle' });
      }
    }
  }
}));
