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
import type { PropertyData, TurnInfo, UnitData } from "../types/game";
import {
  createDefaultPlayerSettings,
  isAiPhase,
  mergeLoadedAiVersions,
  type PlayerSettings,
  toWorkerPlayerAiVersions,
} from "../types/player";
import type { EngineWorker } from "../worker/engineWorker";

export type { PropertyData, TurnInfo, UnitData } from "../types/game";

/** AIの思考ターンで無限ループに陥らないための最大ループ回数ガード */
const MAX_AI_ACTION_LOOPS = 100;

export interface ActionMenuState {
  x: number;
  y: number;
  unitId: string;
  actions: string[];
}

export interface ProduceMenuState {
  /** ポップアップ表示の画面ピクセルX座標 */
  x: number;
  /** ポップアップ表示の画面ピクセルY座標 */
  y: number;
  /** 生産対象のグリッドX座標 */
  gridX: number;
  /** 生産対象のグリッドY座標 */
  gridY: number;
  units: { type: string; name: string; cost: number }[];
}

export interface GameState {
  appState: "menu" | "playing";
  topology: "square" | "hex";
  playerSettings: PlayerSettings;
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
  // 攻撃・補給で共用する選択可能な対象ユニット一覧
  targetableUnits: { id: string; x: number; y: number }[];
  // 対象選択中に実行するアクションを保持する
  pendingTargetAction: "Attack" | "Supply" | null;
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
  initEngine: (mapName: string, topology: string, playerSettings: PlayerSettings) => Promise<void>;
  setPlayerSettings: (playerSettings: PlayerSettings) => void;
  syncGameState: (additionalState?: Partial<GameState>) => Promise<void>;
  tickAiTurn: () => Promise<void>;
  setHoveredCell: (x: number, y: number) => void;
  openActionMenu: (x: number, y: number, unitId: string, actions: string[]) => void;
  closeActionMenu: () => void;

  // Interaction Actions
  selectUnit: (unitId: string) => Promise<void>;
  selectMoveTarget: (x: number, y: number, screenX?: number, screenY?: number) => Promise<void>;
  cancelInteraction: () => void;
  executeAction: (actionType: string, targetId?: string) => Promise<void>;
  openProduceMenu: (x: number, y: number, screenX?: number, screenY?: number) => Promise<void>;
  closeProduceMenu: () => void;
  executeProduce: (unitType: string, x: number, y: number) => Promise<void>;
  // 降車フロー用アクション
  openDropMenu: () => Promise<void>;
  selectDropCargo: (cargoId: string) => Promise<void>;
  executeDropTarget: (x: number, y: number) => Promise<void>;
  endTurn: () => Promise<void>;
  recentlyDestroyedUnitIds: string[];
  clearRecentlyDestroyedUnits: () => void;

  // セーブ・ロード機能
  initAndLoadSaveString: (saveStr: string) => Promise<void>;
  saveGame: (slotIndex: number) => Promise<void>;
  loadGame: (slotIndex: number) => Promise<void>;
  downloadSaveData: () => Promise<void>;
  uploadSaveData: (file: File) => Promise<void>;
  getSlotStatus: () => Promise<
    {
      slotIndex: number;
      hasData: boolean;
      mapName?: string;
      turn?: number;
      activePlayer?: string;
    }[]
  >;
}

export const useGameStore = create<GameState>((set, get) => ({
  appState: "menu",
  topology: "square",
  playerSettings: createDefaultPlayerSettings(),
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
  targetableUnits: [],
  pendingTargetAction: null,
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

  setPlayerSettings: (playerSettings) => {
    set({ playerSettings });
  },

  initEngine: async (mapName, topology, playerSettings) => {
    try {
      const worker = new Worker(new URL("../worker/engineWorker.ts", import.meta.url), {
        type: "module",
      });
      const engineClass = Comlink.wrap<typeof EngineWorker>(worker);
      const engineWorker = await new engineClass();
      await engineWorker.init(mapName, topology, toWorkerPlayerAiVersions(playerSettings));

      // topology パラメータは 'square' | 'hex' のいずれかであることが前提
      // ドメイン上の安全性のため、型アサーションを行っています。
      set({
        engineWorker,
        isEngineReady: true,
        appState: "playing",
        topology: topology as "square" | "hex",
        playerSettings,
      });
      await get().syncGameState();

      const { turnInfo, playerSettings: currentPlayerSettings } = get();
      if (turnInfo && isAiPhase(turnInfo.phase, currentPlayerSettings)) {
        await get().tickAiTurn();
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
    const { engineWorker, unitData, turnInfo, playerSettings } = get();
    if (!engineWorker) return;
    try {
      // AIプレイヤーの手番中はプレイヤー入力を受け付けない
      if (isAiPhase(turnInfo?.phase, playerSettings)) return;

      // 勢力・行動済み・搭載中などのゲームルールは engine の判定を利用する
      if (!(await engineWorker.isUnitSelectable(unitId))) return;

      const unit = unitData.find((candidate) => candidate.id === unitId);
      if (!unit) return;

      if (!(await engineWorker.canUnitMove(unitId))) {
        // 再移動不可の場合は、engine が返す現在地でのアクションだけを表示する
        const actions = await engineWorker.getAvailableActions(unitId, unit.x, unit.y);
        if (actions && actions.length > 0) {
          set({
            interactionState: "action_menu",
            selectedUnitId: unitId,
            selectedTargetPos: { x: unit.x, y: unit.y },
            reachableCells: [],
          });
          get().openActionMenu(unit.x, unit.y, unitId, actions);
        } else {
          get().cancelInteraction();
        }
        return;
      }

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

  selectMoveTarget: async (x: number, y: number, screenX?: number, screenY?: number) => {
    const { engineWorker, selectedUnitId } = get();
    if (!engineWorker || !selectedUnitId) return;
    try {
      const actions = await engineWorker.getAvailableActions(selectedUnitId, x, y);
      set({
        interactionState: "action_menu",
        selectedTargetPos: { x, y },
      });
      const menuX = screenX !== undefined ? screenX : x;
      const menuY = screenY !== undefined ? screenY : y;
      get().openActionMenu(menuX, menuY, selectedUnitId, actions);
    } catch (e) {
      console.error("Failed in selectMoveTarget action:", e);
    }
  },

  cancelInteraction: () => {
    set({
      interactionState: "idle",
      selectedUnitId: null,
      reachableCells: [],
      targetableUnits: [],
      pendingTargetAction: null,
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
            targetableUnits: targets,
            pendingTargetAction: "Attack",
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
      } else if (actionType === "Supply") {
        if (!targetId) {
          const targets = await engineWorker.getSuppliableTargets(
            selectedUnitId,
            selectedTargetPos.x,
            selectedTargetPos.y,
          );
          set({
            interactionState: "target_selection",
            targetableUnits: targets,
            pendingTargetAction: "Supply",
            actionMenu: null,
          });
          return;
        }
        await engineWorker.submitMoveCommand(
          selectedUnitId,
          selectedTargetPos.x,
          selectedTargetPos.y,
        );
        await engineWorker.submitSupplyCommand(selectedUnitId, targetId);
      } else if (actionType === "Capture") {
        await engineWorker.submitMoveCommand(
          selectedUnitId,
          selectedTargetPos.x,
          selectedTargetPos.y,
        );
        await engineWorker.submitCaptureCommand(selectedUnitId);
      } else if (actionType === "Repair") {
        await engineWorker.submitMoveCommand(
          selectedUnitId,
          selectedTargetPos.x,
          selectedTargetPos.y,
        );
        await engineWorker.submitRepairCommand(selectedUnitId);
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

  openProduceMenu: async (x: number, y: number, screenX?: number, screenY?: number) => {
    const { engineWorker, turnInfo, playerSettings } = get();
    if (!engineWorker) return;
    try {
      // AIプレイヤーの手番中はプレイヤー入力を受け付けない
      if (isAiPhase(turnInfo?.phase, playerSettings)) return;

      // 所有者・地形・占有・生産範囲などは engine が返す生産可能一覧に集約する
      const units = await engineWorker.getProducibleUnits(x, y);
      if (units && units.length > 0) {
        const menuX = screenX !== undefined ? screenX : x;
        const menuY = screenY !== undefined ? screenY : y;
        set({
          interactionState: "produce_menu",
          produceMenu: { x: menuX, y: menuY, gridX: x, gridY: y, units },
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
      const remainingLoaded = await engineWorker.getLoadedUnits(selectedUnitId);
      await get().syncGameState();
      if (remainingLoaded && remainingLoaded.length > 0) {
        set({
          interactionState: "drop_unit_selection",
          loadedUnits: remainingLoaded,
          actionMenu: null,
          reachableCells: [],
          dropCargoId: null,
        });
      } else {
        get().cancelInteraction();
      }
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
      const { turnInfo, playerSettings } = get();
      if (turnInfo) {
        const isNextAiTurn = isAiPhase(turnInfo.phase, playerSettings);
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

      const { turnInfo, playerSettings } = get();
      if (turnInfo) {
        const isAiTurn = isAiPhase(turnInfo.phase, playerSettings);
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

  initAndLoadSaveString: async (saveStr: string) => {
    try {
      const worker = new Worker(new URL("../worker/engineWorker.ts", import.meta.url), {
        type: "module",
      });
      const engineClass = Comlink.wrap<typeof EngineWorker>(worker);
      const engineWorker = await new engineClass();
      const currentPlayerSettings = get().playerSettings;

      // Wasmエンジン側で初期化を行ってからインポートする。
      await engineWorker.init("map_1", "square", toWorkerPlayerAiVersions(currentPlayerSettings));
      await engineWorker.importSaveData(saveStr);
      const normalizedVersions = await engineWorker.reapplyNormalizedPlayerAiVersions();
      const playerSettings = mergeLoadedAiVersions(currentPlayerSettings, normalizedVersions);

      set({
        engineWorker,
        isEngineReady: true,
        appState: "playing",
        topology: parseSaveTopology(saveStr),
        // 操作モードはタイトル画面の既定値を維持し、AIバージョンだけをセーブから復元する。
        playerSettings,
      });

      await get().syncGameState();
      const { turnInfo } = get();
      if (turnInfo && isAiPhase(turnInfo.phase, playerSettings)) {
        await get().tickAiTurn();
      }
    } catch (e) {
      console.error("initAndLoadSaveString failed:", e);
      throw e;
    }
  },

  saveGame: async (slotIndex: number) => {
    const { engineWorker } = get();
    if (!engineWorker) return;
    try {
      const saveStr = await engineWorker.exportSaveData("OpenWarsMap");
      localStorage.setItem(`openwars_save_slot_${slotIndex}`, saveStr);
    } catch (e) {
      console.error("Save game failed:", e);
      throw e;
    }
  },

  loadGame: async (slotIndex: number) => {
    const { engineWorker } = get();
    try {
      const saveStr = localStorage.getItem(`openwars_save_slot_${slotIndex}`);
      if (!saveStr) throw new Error("指定されたスロットにセーブデータがありません。");
      if (engineWorker) {
        const currentPlayerSettings = get().playerSettings;
        await engineWorker.importSaveData(saveStr);
        const normalizedVersions = await engineWorker.reapplyNormalizedPlayerAiVersions();
        const playerSettings = mergeLoadedAiVersions(currentPlayerSettings, normalizedVersions);
        get().cancelInteraction();
        set({ playerSettings, topology: parseSaveTopology(saveStr) });
        await get().syncGameState();
        const { turnInfo } = get();
        if (turnInfo && isAiPhase(turnInfo.phase, playerSettings)) {
          await get().tickAiTurn();
        }
      } else {
        await get().initAndLoadSaveString(saveStr);
      }
    } catch (e) {
      console.error("Load game failed:", e);
      throw e;
    }
  },

  downloadSaveData: async () => {
    const { engineWorker } = get();
    if (!engineWorker) return;
    try {
      const saveStr = await engineWorker.exportSaveData("OpenWarsMap");
      const blob = new Blob([saveStr], { type: "text/plain" });
      const url = URL.createObjectURL(blob);
      const a = document.createElement("a");
      a.href = url;
      a.download = "openwars_save.sav";
      document.body.appendChild(a);
      a.click();
      document.body.removeChild(a);
      URL.revokeObjectURL(url);
    } catch (e) {
      console.error("Download save data failed:", e);
    }
  },

  uploadSaveData: async (file: File) => {
    const { engineWorker } = get();
    try {
      const saveStr = await file.text();
      if (engineWorker) {
        const currentPlayerSettings = get().playerSettings;
        await engineWorker.importSaveData(saveStr);
        const normalizedVersions = await engineWorker.reapplyNormalizedPlayerAiVersions();
        const playerSettings = mergeLoadedAiVersions(currentPlayerSettings, normalizedVersions);
        get().cancelInteraction();
        set({ playerSettings, topology: parseSaveTopology(saveStr) });
        await get().syncGameState();
        const { turnInfo } = get();
        if (turnInfo && isAiPhase(turnInfo.phase, playerSettings)) {
          await get().tickAiTurn();
        }
      } else {
        await get().initAndLoadSaveString(saveStr);
      }
    } catch (e) {
      console.error("Upload save data failed:", e);
      throw e;
    }
  },

  getSlotStatus: async () => {
    const statusList = [];
    for (let i = 1; i <= 5; i++) {
      const saveStr = localStorage.getItem(`openwars_save_slot_${i}`);
      if (saveStr) {
        const header = parseSaveHeader(saveStr);
        if (header) {
          statusList.push({
            slotIndex: i,
            hasData: true,
            mapName: header.mapName,
            turn: header.turn,
            activePlayer: header.activePlayer,
          });
          continue;
        }
      }
      statusList.push({ slotIndex: i, hasData: false });
    }
    return statusList;
  },
}));

interface SaveHeader {
  mapName: string;
  turn: number;
  activePlayer: string;
}

interface SavePayload {
  map_name?: string;
  map_topology?: string;
  match_state?: {
    current_turn_number?: number;
    active_player_index?: number;
  };
  players?: { name?: string }[];
}

function parseSavePayload(saveStr: string): SavePayload | null {
  try {
    const parts = saveStr.split(".");
    if (parts.length !== 3 || parts[0] !== "OPWS1") return null;
    const binary = atob(parts[1]);
    const bytes = Uint8Array.from(binary, (character) => character.charCodeAt(0));
    return JSON.parse(new TextDecoder().decode(bytes)) as SavePayload;
  } catch {
    return null;
  }
}

function parseSaveTopology(saveStr: string): "square" | "hex" {
  return parseSavePayload(saveStr)?.map_topology === "Hex" ? "hex" : "square";
}

function parseSaveHeader(saveStr: string): SaveHeader | null {
  const payload = parseSavePayload(saveStr);
  if (!payload) return null;

  const mapName = payload.map_name || "不明";
  const turn = payload.match_state?.current_turn_number || 0;
  const activeIdx = payload.match_state?.active_player_index || 0;
  const activePlayer = payload.players?.[activeIdx]?.name || "不明";

  return { mapName, turn, activePlayer };
}
