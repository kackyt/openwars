import type { Remote } from "comlink";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { createDefaultPlayerSettings } from "../types/player";
import type { EngineWorker } from "../worker/engineWorker";
import { useGameStore } from "./gameStore";

const asEngineWorker = (worker: Partial<EngineWorker>): Remote<EngineWorker> =>
  worker as unknown as Remote<EngineWorker>;

describe("gameStore", () => {
  beforeEach(() => {
    // 各テスト前にストアの状態を初期化する
    useGameStore.setState({
      engineWorker: null,
      unitData: [],
      propertyData: [],
      turnInfo: null,
      playerSettings: createDefaultPlayerSettings(),
    });
    useGameStore.getState().cancelInteraction();
  });

  describe("cancelInteraction", () => {
    it("should reset interaction state to idle and clear selections", () => {
      const store = useGameStore.getState();

      // 適当な初期状態をセットする
      useGameStore.setState({
        interactionState: "unit_selected",
        selectedUnitId: "unit-123",
        reachableCells: [{ x: 1, y: 1 }],
        actionMenu: { x: 0, y: 0, unitId: "unit-123", actions: ["Wait"] },
      });

      // アクション実行
      store.cancelInteraction();

      // リセット結果の検証
      const state = useGameStore.getState();
      expect(state.interactionState).toBe("idle");
      expect(state.selectedUnitId).toBeNull();
      expect(state.reachableCells).toEqual([]);
      expect(state.actionMenu).toBeNull();
      expect(state.dropCargoId).toBeNull();
    });
  });

  describe("setHoveredCell", () => {
    it("should update hovered coordinate and resolve terrain and unit if matching", () => {
      const store = useGameStore.getState();

      // ダミーのマップ・ユニット・地形定義をセット
      useGameStore.setState({
        mapData: [["plains", "forest"]],
        terrainDefs: { plains: 1, forest: 2 },
        unitData: [
          {
            id: "unit-1",
            type: "infantry",
            faction: "blue",
            x: 1,
            y: 0,
            hp: 10,
            is_loaded: false,
            is_exhausted: false,
            fuel: { current: 99, max: 99 },
            weapons: [],
          },
        ],
        propertyData: [],
      });

      // 森のセル(1, 0)にホバーする。そこには unit-1 も存在する。
      store.setHoveredCell(1, 0);

      const state = useGameStore.getState();
      expect(state.hoveredCellX).toBe(1);
      expect(state.hoveredCellY).toBe(0);
      expect(state.hoveredTerrain).toEqual({
        type: "forest",
        def: 2,
        property: null,
      });
      expect(state.hoveredUnit).not.toBeNull();
      expect(state.hoveredUnit?.id).toBe("unit-1");
    });
  });

  describe("openActionMenu", () => {
    it("should set action menu state details correctly", () => {
      const store = useGameStore.getState();

      store.openActionMenu(5, 5, "unit-abc", ["Wait", "Attack"]);

      const state = useGameStore.getState();
      expect(state.actionMenu).toEqual({
        x: 5,
        y: 5,
        unitId: "unit-abc",
        actions: ["Wait", "Attack"],
      });
    });
  });

  describe("engine-backed interaction guards", () => {
    const units = [
      {
        id: "enemy-unit",
        type: "infantry",
        faction: "blue",
        x: 2,
        y: 2,
        hp: 10,
        is_loaded: false,
        is_exhausted: false,
        fuel: { current: 99, max: 99 },
        weapons: [],
      },
      {
        id: "own-unit",
        type: "infantry",
        faction: "green",
        x: 1,
        y: 1,
        hp: 10,
        is_loaded: false,
        is_exhausted: false,
        fuel: { current: 99, max: 99 },
        weapons: [],
      },
    ];

    it("uses the engine result to decide whether a unit is selectable", async () => {
      const isUnitSelectable = vi.fn(async (unitId: string) => unitId === "own-unit");
      useGameStore.setState({
        turnInfo: { turn: 1, phase: "P1", funds: 1000 },
        engineWorker: asEngineWorker({
          isUnitSelectable,
          canUnitMove: async () => true,
          getReachableCells: async () => [{ x: 1, y: 1 }],
        }),
        unitData: units,
      });

      await useGameStore.getState().selectUnit("enemy-unit");
      expect(useGameStore.getState().interactionState).toBe("idle");
      expect(useGameStore.getState().selectedUnitId).toBeNull();

      await useGameStore.getState().selectUnit("own-unit");
      expect(useGameStore.getState().interactionState).toBe("unit_selected");
      expect(useGameStore.getState().selectedUnitId).toBe("own-unit");
      expect(isUnitSelectable).toHaveBeenCalledWith("own-unit");
    });

    it("opens the current-position action menu when the engine disallows another move", async () => {
      useGameStore.setState({
        turnInfo: { turn: 1, phase: "P1", funds: 1000 },
        engineWorker: asEngineWorker({
          isUnitSelectable: async () => true,
          canUnitMove: async () => false,
          getAvailableActions: async () => ["Attack", "Wait"],
        }),
        unitData: [units[1]],
        reachableCells: [{ x: 9, y: 9 }],
      });

      await useGameStore.getState().selectUnit("own-unit");

      const state = useGameStore.getState();
      expect(state.interactionState).toBe("action_menu");
      expect(state.selectedTargetPos).toEqual({ x: 1, y: 1 });
      expect(state.reachableCells).toEqual([]);
      expect(state.actionMenu?.actions).toEqual(["Attack", "Wait"]);
    });

    it("blocks unit selection and production during an AI phase", async () => {
      const isUnitSelectable = vi.fn(async () => true);
      const getProducibleUnits = vi.fn(async () => [
        { type: "infantry", name: "歩兵", cost: 1000 },
      ]);
      useGameStore.setState({
        turnInfo: { turn: 1, phase: "P1", funds: 1000 },
        playerSettings: {
          1: { controlMode: "ai", aiVersion: "V3" },
          2: { controlMode: "human", aiVersion: "V3" },
        },
        engineWorker: asEngineWorker({ isUnitSelectable, getProducibleUnits }),
        unitData: [units[1]],
      });

      await useGameStore.getState().selectUnit("own-unit");
      await useGameStore.getState().openProduceMenu(1, 1);

      expect(isUnitSelectable).not.toHaveBeenCalled();
      expect(getProducibleUnits).not.toHaveBeenCalled();
      expect(useGameStore.getState().interactionState).toBe("idle");
    });

    it("uses the engine producible list instead of property ownership in the UI", async () => {
      const getProducibleUnits = vi.fn(async (x: number) =>
        x === 1 ? [{ type: "infantry", name: "歩兵", cost: 1000 }] : [],
      );
      useGameStore.setState({
        turnInfo: { turn: 1, phase: "P1", funds: 1000 },
        engineWorker: asEngineWorker({ getProducibleUnits }),
      });

      await useGameStore.getState().openProduceMenu(0, 0);
      expect(useGameStore.getState().produceMenu).toBeNull();

      await useGameStore.getState().openProduceMenu(1, 1);
      expect(useGameStore.getState().interactionState).toBe("produce_menu");
      expect(useGameStore.getState().produceMenu?.gridX).toBe(1);
    });
  });

  describe("executeDropTarget", () => {
    it("returns to cargo selection when loaded units remain", async () => {
      const submitUnloadCommand = vi.fn(async () => undefined);
      const remainingLoaded = [{ id: "cargo-2", type: "infantry" }];
      const syncGameState = vi.fn(async () => undefined);
      useGameStore.setState({
        engineWorker: asEngineWorker({
          submitUnloadCommand,
          getLoadedUnits: async () => remainingLoaded,
        }),
        selectedUnitId: "transport-1",
        dropCargoId: "cargo-1",
        interactionState: "drop_target_selection",
        reachableCells: [{ x: 2, y: 2 }],
        syncGameState,
      });

      await useGameStore.getState().executeDropTarget(2, 2);

      expect(submitUnloadCommand).toHaveBeenCalledWith("transport-1", "cargo-1", 2, 2);
      expect(syncGameState).toHaveBeenCalledOnce();
      const state = useGameStore.getState();
      expect(state.interactionState).toBe("drop_unit_selection");
      expect(state.loadedUnits).toEqual(remainingLoaded);
      expect(state.dropCargoId).toBeNull();
      expect(state.reachableCells).toEqual([]);
    });
  });

  describe("save import settings", () => {
    it("restores normalized AI versions, preserves modes, and resumes an active AI", async () => {
      const importSaveData = vi.fn(async () => undefined);
      const reapplyNormalizedPlayerAiVersions = vi.fn(async () => ({
        1: "V1" as const,
        2: "V3" as const,
      }));
      const tickAiTurn = vi.fn(async () => undefined);
      const syncGameState = vi.fn(async () => {
        useGameStore.setState({ turnInfo: { turn: 3, phase: "P2", funds: 2000 } });
      });
      const savePayload = btoa(JSON.stringify({ map_topology: "Hex" }));
      localStorage.setItem("openwars_save_slot_1", `OPWS1.${savePayload}.signature`);

      useGameStore.setState({
        engineWorker: asEngineWorker({
          importSaveData,
          reapplyNormalizedPlayerAiVersions,
        }),
        playerSettings: {
          1: { controlMode: "human", aiVersion: "V3" },
          2: { controlMode: "ai", aiVersion: "V1" },
        },
        syncGameState,
        tickAiTurn,
      });

      await useGameStore.getState().loadGame(1);

      expect(importSaveData).toHaveBeenCalledWith(`OPWS1.${savePayload}.signature`);
      expect(reapplyNormalizedPlayerAiVersions).toHaveBeenCalledOnce();
      expect(useGameStore.getState().playerSettings).toEqual({
        1: { controlMode: "human", aiVersion: "V1" },
        2: { controlMode: "ai", aiVersion: "V3" },
      });
      expect(useGameStore.getState().topology).toBe("hex");
      expect(tickAiTurn).toHaveBeenCalledOnce();
    });
  });

  describe("Supply target selection", () => {
    it("queries legal supply targets without committing movement", async () => {
      const getSuppliableTargets = vi.fn(async () => [{ id: "target-1", x: 3, y: 2 }]);
      const submitMoveCommand = vi.fn(async () => undefined);
      useGameStore.setState({
        engineWorker: asEngineWorker({ getSuppliableTargets, submitMoveCommand }),
        selectedUnitId: "supplier-1",
        selectedTargetPos: { x: 2, y: 2 },
        interactionState: "action_menu",
        actionMenu: { x: 2, y: 2, unitId: "supplier-1", actions: ["Supply"] },
      });

      await useGameStore.getState().executeAction("Supply");

      expect(getSuppliableTargets).toHaveBeenCalledWith("supplier-1", 2, 2);
      expect(submitMoveCommand).not.toHaveBeenCalled();
      const state = useGameStore.getState();
      expect(state.interactionState).toBe("target_selection");
      expect(state.targetableUnits).toEqual([{ id: "target-1", x: 3, y: 2 }]);
      expect(state.pendingTargetAction).toBe("Supply");
      expect(state.actionMenu).toBeNull();
    });

    it("commits movement then supplies exactly the chosen target", async () => {
      const submitMoveCommand = vi.fn(async () => undefined);
      const submitSupplyCommand = vi.fn(async () => undefined);
      const syncGameState = vi.fn(async () => undefined);
      useGameStore.setState({
        engineWorker: asEngineWorker({ submitMoveCommand, submitSupplyCommand }),
        selectedUnitId: "supplier-1",
        selectedTargetPos: { x: 2, y: 2 },
        targetableUnits: [
          { id: "target-1", x: 3, y: 2 },
          { id: "target-2", x: 2, y: 3 },
        ],
        pendingTargetAction: "Supply",
        interactionState: "target_selection",
        syncGameState,
      });

      await useGameStore.getState().executeAction("Supply", "target-2");

      expect(submitMoveCommand).toHaveBeenCalledOnce();
      expect(submitMoveCommand).toHaveBeenCalledWith("supplier-1", 2, 2);
      expect(submitSupplyCommand).toHaveBeenCalledOnce();
      expect(submitSupplyCommand).toHaveBeenCalledWith("supplier-1", "target-2");
      expect(syncGameState).toHaveBeenCalledOnce();
      const state = useGameStore.getState();
      expect(state.interactionState).toBe("idle");
      expect(state.targetableUnits).toEqual([]);
      expect(state.pendingTargetAction).toBeNull();
    });
  });
});
