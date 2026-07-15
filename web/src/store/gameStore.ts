/**
 * @file gameStore.ts
 * @description Zustandを用いたゲームのグローバル状態管理ストア。
 * エンジンの初期化、Wasm Worker との通信同期、ゲーム内のインタラクション状態マシン（状態遷移）を管理します。
 *
 * 【インタラクション状態マシン (interactionState) 遷移図】
 *
 *        ┌───────────────────────┐
 *        │         idle          │◄─────────────────────────────────────────────┐
 *        └─────┬───────────┬─────┘                                              │
 *              │           │                                                    │
 *      (Select Unit) (Select Factory)                                           │
 *              │           │                                                    │
 *              ▼           ▼                                                    │
 *    ┌───────────┐   ┌──────────────┐                                           │
 *    │   unit_   │   │ produce_menu │                                           │
 *    │ selected  │   └──────┬───────┘                                           │
 *    └─────┬─────┘          │                                                   │
 *          │           (Produce Unit)                                           │
 *    (Select Move)          │                                                   │
 *          │                ▼                                                   │
 *          ▼          [Sync State] ─────────────────────────────────────────────┤
 *    ┌───────────┐                                                              │
 *    │  action_  │                                                              │
 *    │   menu    │                                                              │
 *    └─────┬─────┘                                                              │
 *          ├───────────────┬──────────────────┐                                 │
 *       (Attack)        (Drop)             (Wait / Capture / Load / Merge)      │
 *          │               │                  │                                 │
 *          ▼               ▼                  ▼                                 │
 *    ┌───────────┐   ┌──────────────┐   [Sync State] ───────────────────────────┤
 *    │  target_  │   │  drop_unit_  │                                           │
 *    │ selection │   │  selection   │                                           │
 *    └─────┬─────┘   └──────┬───────┘                                           │
 *          │                │                                                   │
 *       (Select          (Select                                                │
 *       Target)          Cargo)                                                 │
 *          │                ▼                                                   │
 *          │         ┌──────────────┐                                           │
 *          │         │ drop_target_ │                                           │
 *          │         │  selection   │                                           │
 *          │         └──────┬───────┘                                           │
 *          │                │                                                   │
 *          │             (Select                                                │
 *          │           Drop Tile)                                               │
 *          ▼                ▼                                                   │
 *     [Sync State] ────[Sync State] ────────────────────────────────────────────┘
 *
 *    ※ AIターン時は、一時的に interactionState: "ai_thinking" となり、
 *       すべてのプレイヤー入力が無視されます。
 */

import * as Comlink from "comlink";
import { create } from "zustand";
import { PHASE_P1, PHASE_P2 } from "../constants/mappings";
import type { EngineWorker } from "../worker/engineWorker";

/** AIの思考ターンで無限ループに陥らないための最大ループ回数ガード */
const MAX_AI_ACTION_LOOPS = 100;

export interface UnitData {
  id: string;
  type: string;
  faction: string;
  x: number;
  y: number;
  hp: number;
  is_loaded: boolean;
  is_exhausted: boolean;
  fuel: { current: number; max: number };
  weapons: { name: string; ammo: number; max_ammo: number; min_range: number; max_range: number }[];
}

export interface TurnInfo {
  turn: number;
  phase: string;
  funds: number;
}

export interface PropertyData {
  x: number;
  y: number;
  type: string;
  owner: string;
  capture_points: number;
  max_capture_points: number;
}

export interface ActionMenuState {
  x: number;
  y: number;
  unitId: string;
  actions: string[];
}

export interface ProduceMenuState {
  x: number;
  y: number;
  units: { type: string; name: string; cost: number }[];
}

export interface GameState {
  appState: "menu" | "playing";
  topology: "square" | "hex";
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
  interactionState:
    | "idle"
    | "unit_selected"
    | "action_menu"
    | "target_selection"
    | "produce_menu"
    | "drop_unit_selection"
    | "drop_target_selection"
    | "ai_thinking";
  selectedUnitId: string | null;
  reachableCells: { x: number; y: number }[];
  attackableTargets: { id: string; x: number; y: number }[];
  selectedTargetPos: { x: number; y: number } | null;
  produceMenu: ProduceMenuState | null;
  // 降車フロー用: 積載ユニット一覧と選択されたユニットIDを保持する
  loadedUnits: { id: string; type: string }[];
  dropCargoId: string | null;

  hoveredCellX: number;
  hoveredCellY: number;
  hoveredTerrain: { type: string; def?: number; property?: PropertyData | null } | null;
  hoveredUnit: UnitData | null;
  actionMenu: ActionMenuState | null;

  // Actions
  initEngine: (
    mapName: string,
    topology: string,
    p1IsAi: boolean,
    p2IsAi: boolean,
  ) => Promise<void>;
  syncGameState: (additionalState?: Partial<GameState>) => Promise<void>;
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
  recentlyDestroyedUnitIds: string[];
  clearRecentlyDestroyedUnits: () => void;
}

export const useGameStore = create<GameState>((set, get) => ({
  appState: "menu",
  topology: "square",
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

  interactionState: "idle",
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
  recentlyDestroyedUnitIds: [],
  actionMenu: null,

  clearRecentlyDestroyedUnits: () => {
    set({ recentlyDestroyedUnitIds: [] });
  },

  initEngine: async (mapName, topology, p1IsAi, p2IsAi) => {
    try {
      const worker = new Worker(new URL("../worker/engineWorker.ts", import.meta.url), {
        type: "module",
      });
      const engineClass = Comlink.wrap<typeof EngineWorker>(worker);
      const engineWorker = await new engineClass();
      await engineWorker.init(mapName, topology);

      // topology パラメータは 'square' | 'hex' のいずれかであることが前提
      // ドメイン上の安全性のため、型アサーションを行っています。
      set({
        engineWorker,
        isEngineReady: true,
        appState: "playing",
        topology: topology as "square" | "hex",
        p1IsAi,
        p2IsAi,
      });
      await get().syncGameState();

      const { turnInfo, p1IsAi: isP1Ai, p2IsAi: isP2Ai } = get();
      if (turnInfo) {
        const isAiTurn =
          (turnInfo.phase === PHASE_P1 && isP1Ai) || (turnInfo.phase === PHASE_P2 && isP2Ai);
        if (isAiTurn) {
          await get().tickAiTurn();
        }
      }
    } catch (e) {
      console.error("Failed to initialize engine:", e);
    }
  },

  syncGameState: async (additionalState?: Partial<GameState>) => {
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

      set({
        mapData,
        unitData,
        turnInfo,
        propertyData,
        terrainDefs,
        gameOver,
        ...additionalState,
      });
    } catch (e) {
      console.error("Failed to sync game state:", e);
    }
  },

  setHoveredCell: (x: number, y: number) => {
    const { mapData, unitData, terrainDefs, propertyData } = get();
    const cellType = mapData[y]?.[x] || "unknown";
    const unit = unitData.find((u) => u.x === x && u.y === y) || null;
    const property = propertyData.find((p) => p.x === x && p.y === y) || null;

    set({
      hoveredCellX: x,
      hoveredCellY: y,
      hoveredTerrain: { type: cellType, def: terrainDefs[cellType] || 0, property },
      hoveredUnit: unit,
    });
  },

  openActionMenu: (x: number, y: number, unitId: string, actions: string[]) => {
    set({ actionMenu: { x, y, unitId, actions } });
  },

  closeActionMenu: () => {
    set({ actionMenu: null });
    get().cancelInteraction();
  },

  selectUnit: async (unitId: string) => {
    const { engineWorker, unitData } = get();
    if (!engineWorker) return;
    try {
      const unit = unitData.find((u) => u.id === unitId);
      if (unit?.is_exhausted) return; // 行動済みなら選択不可
      const cells = await engineWorker.getReachableCells(unitId);
      set({
        interactionState: "unit_selected",
        selectedUnitId: unitId,
        reachableCells: cells,
      });
    } catch (e) {
      console.error("Failed in selectUnit action:", e);
    }
  },

  selectMoveTarget: async (x: number, y: number) => {
    const { engineWorker, selectedUnitId } = get();
    if (!engineWorker || !selectedUnitId) return;
    try {
      const actions = await engineWorker.getAvailableActions(selectedUnitId, x, y);
      set({
        interactionState: "action_menu",
        selectedTargetPos: { x, y },
      });
      get().openActionMenu(x, y, selectedUnitId, actions);
    } catch (e) {
      console.error("Failed in selectMoveTarget action:", e);
    }
  },

  cancelInteraction: () => {
    set({
      interactionState: "idle",
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

    try {
      if (actionType === "Wait") {
        await engineWorker.submitMoveCommand(
          selectedUnitId,
          selectedTargetPos.x,
          selectedTargetPos.y,
        );
        await engineWorker.submitWaitCommand(selectedUnitId);
      } else if (actionType === "Attack") {
        if (!targetId) {
          const targets = await engineWorker.getAttackableTargets(
            selectedUnitId,
            selectedTargetPos.x,
            selectedTargetPos.y,
          );
          set({
            interactionState: "target_selection",
            attackableTargets: targets,
            actionMenu: null,
          });
          return;
        }
        await engineWorker.submitMoveCommand(
          selectedUnitId,
          selectedTargetPos.x,
          selectedTargetPos.y,
        );
        const destroyedIds = await engineWorker.submitAttackCommand(selectedUnitId, targetId);
        get().cancelInteraction();
        await get().syncGameState({ recentlyDestroyedUnitIds: destroyedIds });
        return;
      } else if (actionType === "Capture") {
        await engineWorker.submitMoveCommand(
          selectedUnitId,
          selectedTargetPos.x,
          selectedTargetPos.y,
        );
        await engineWorker.submitCaptureCommand(selectedUnitId);
      } else if (actionType === "Load") {
        await engineWorker.submitMoveCommand(
          selectedUnitId,
          selectedTargetPos.x,
          selectedTargetPos.y,
        );
        const targetUnit = get().unitData.find(
          (u) =>
            u.x === selectedTargetPos.x && u.y === selectedTargetPos.y && u.id !== selectedUnitId,
        );
        if (targetUnit) {
          await engineWorker.submitLoadCommand(selectedUnitId, targetUnit.id);
        }
      } else if (actionType === "Merge") {
        // 移動先に同じ勢力・同種のユニットを探して合流する
        await engineWorker.submitMoveCommand(
          selectedUnitId,
          selectedTargetPos.x,
          selectedTargetPos.y,
        );
        const targetUnit = get().unitData.find(
          (u) =>
            u.x === selectedTargetPos.x && u.y === selectedTargetPos.y && u.id !== selectedUnitId,
        );
        if (targetUnit) {
          await engineWorker.submitMergeCommand(selectedUnitId, targetUnit.id);
        }
      } else if (actionType === "Drop") {
        // 移動コマンドは送信済みの状態で、降車する積載ユニットの選択メニューを開く
        await engineWorker.submitMoveCommand(
          selectedUnitId,
          selectedTargetPos.x,
          selectedTargetPos.y,
        );
        await get().openDropMenu();
        return; // まだ cancelInteraction / syncGameState は呼ばない
      }

      get().cancelInteraction();
      await get().syncGameState();
    } catch (e) {
      console.error(`Failed to execute action "${actionType}":`, e);
      get().cancelInteraction();
    }
  },

  openProduceMenu: async (x: number, y: number) => {
    const { engineWorker } = get();
    if (!engineWorker) return;
    try {
      const units = await engineWorker.getProducibleUnits(x, y);
      if (units && units.length > 0) {
        set({
          interactionState: "produce_menu",
          produceMenu: { x, y, units },
        });
      }
    } catch (e) {
      console.error("Failed to open produce menu:", e);
    }
  },

  closeProduceMenu: () => {
    set({ interactionState: "idle", produceMenu: null });
  },

  executeProduce: async (unitType: string, x: number, y: number) => {
    const { engineWorker } = get();
    if (!engineWorker) return;
    try {
      await engineWorker.submitProduceCommand(unitType, x, y);
      get().closeProduceMenu();
      await get().syncGameState();
    } catch (e) {
      console.error(`Failed to produce unit "${unitType}":`, e);
      get().closeProduceMenu();
    }
  },

  // 積載ユニット一覧を取得して降車ユニット選択モードに遷移する
  openDropMenu: async () => {
    const { engineWorker, selectedUnitId } = get();
    if (!engineWorker || !selectedUnitId) return;
    try {
      const loadedUnits = await engineWorker.getLoadedUnits(selectedUnitId);
      set({ interactionState: "drop_unit_selection", loadedUnits, actionMenu: null });
    } catch (e) {
      console.error("Failed to open drop menu:", e);
      get().cancelInteraction();
    }
  },

  // 降車するユニットを選択し、降ろせるマスのハイライトに遷移する
  selectDropCargo: async (cargoId: string) => {
    const { engineWorker, selectedUnitId } = get();
    if (!engineWorker || !selectedUnitId) return;
    try {
      const droppableTiles = await engineWorker.getDroppableTiles(selectedUnitId, cargoId);
      set({
        interactionState: "drop_target_selection",
        dropCargoId: cargoId,
        reachableCells: droppableTiles, // 降車可能マスをハイライト表示に流用する
      });
    } catch (e) {
      console.error("Failed to select drop cargo:", e);
      get().cancelInteraction();
    }
  },

  // 降車先マスを選択して降車コマンドを送信する
  executeDropTarget: async (x: number, y: number) => {
    const { engineWorker, selectedUnitId, dropCargoId } = get();
    if (!engineWorker || !selectedUnitId || !dropCargoId) return;
    try {
      await engineWorker.submitUnloadCommand(selectedUnitId, dropCargoId, x, y);
      get().cancelInteraction();
      await get().syncGameState();
    } catch (e) {
      console.error("Failed to execute drop target:", e);
      get().cancelInteraction();
    }
  },

  tickAiTurn: async () => {
    const { engineWorker } = get();
    if (!engineWorker) return;

    set({
      interactionState: "ai_thinking",
      selectedUnitId: null,
      actionMenu: null,
      produceMenu: null,
    });

    try {
      let loopCount = 0;
      while (true) {
        // AIの無限ループ防止ガード
        if (loopCount++ > MAX_AI_ACTION_LOOPS) {
          console.warn("AI turn reached loop threshold guard, terminating AI execution.");
          break;
        }

        const aiResult = await engineWorker.executeAiTurn();
        await get().syncGameState(
          aiResult.destroyed && aiResult.destroyed.length > 0
            ? { recentlyDestroyedUnitIds: aiResult.destroyed }
            : undefined,
        );
        if (!aiResult.acted) break;
      }

      // AIターンはWasm内部でNextPhaseCommandが処理されて終了しているため、直接次のターンの状態を確認する
      const { turnInfo, p1IsAi, p2IsAi } = get();
      if (turnInfo) {
        const isNextAiTurn =
          (turnInfo.phase === PHASE_P1 && p1IsAi) || (turnInfo.phase === PHASE_P2 && p2IsAi);
        if (isNextAiTurn) {
          await get().tickAiTurn();
        } else {
          set({ interactionState: "idle" });
        }
      }
    } catch (e) {
      console.error("Failed during AI turn execution:", e);
      set({ interactionState: "idle" });
    }
  },

  endTurn: async () => {
    const { engineWorker } = get();
    if (!engineWorker) return;

    try {
      const gameOverObj = await engineWorker.checkGameOver();
      if (gameOverObj) return;

      await engineWorker.endTurn();
      await get().syncGameState();

      const { turnInfo, p1IsAi, p2IsAi } = get();
      if (turnInfo) {
        const isAiTurn =
          (turnInfo.phase === PHASE_P1 && p1IsAi) || (turnInfo.phase === PHASE_P2 && p2IsAi);
        if (isAiTurn) {
          await get().tickAiTurn();
        } else {
          set({ interactionState: "idle" });
        }
      }
    } catch (e) {
      console.error("Failed to end turn:", e);
      set({ interactionState: "idle" });
    }
  },
}));
