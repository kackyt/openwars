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
}

interface TurnInfo {
  turn: number;
  phase: string;
}

interface PropertyData {
  x: number;
  y: number;
  type: string;
  owner: string;
}

interface ActionMenuState {
  x: number;
  y: number;
  unitId: string;
  actions: string[];
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
  
  // UI State
  hoveredCellX: number;
  hoveredCellY: number;
  hoveredTerrain: { type: string, def?: number } | null;
  hoveredUnit: UnitData | null;
  actionMenu: ActionMenuState | null;

  // Actions
  initEngine: (mapName: string, topology: string, p1IsAi: boolean, p2IsAi: boolean) => Promise<void>;
  syncGameState: () => Promise<void>;
  setHoveredCell: (x: number, y: number) => void;
  openActionMenu: (x: number, y: number, unitId: string, actions: string[]) => void;
  closeActionMenu: () => void;
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
    } catch (e) {
      console.error("Failed to initialize engine:", e);
    }
  },

  syncGameState: async () => {
    const { engineWorker } = get();
    if (!engineWorker) return;

    try {
      const [mapData, unitData, turnInfo, propertyData, terrainDefs] = await Promise.all([
        engineWorker.getMap(),
        engineWorker.getUnits(),
        engineWorker.getTurnInfo(),
        engineWorker.getProperties(),
        engineWorker.getTerrainDefs(),
      ]);

      set({ mapData, unitData, turnInfo, propertyData, terrainDefs });
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
  }
}));
