import { beforeEach, describe, expect, it } from "vitest";
import { useGameStore } from "./gameStore";

describe("gameStore", () => {
  beforeEach(() => {
    // 各テスト前にストアの状態を初期化する
    const store = useGameStore.getState();
    store.cancelInteraction();
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

  describe("selectUnit & openProduceMenu faction guard", () => {
    it("should not select non-active faction unit", async () => {
      const store = useGameStore.getState();

      // P1ターン (緑軍: green) の状態をセット
      useGameStore.setState({
        turnInfo: { turn: 1, phase: "P1", funds: 1000 },
        engineWorker: {
          getReachableCells: async () => [{ x: 1, y: 1 }],
        } as unknown as import("comlink").Remote<import("../worker/engineWorker").EngineWorker>,
        unitData: [
          {
            id: "enemy-unit",
            type: "infantry",
            faction: "blue", // 敵軍 (青軍)
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
            faction: "green", // 自軍 (緑軍)
            x: 1,
            y: 1,
            hp: 10,
            is_loaded: false,
            is_exhausted: false,
            fuel: { current: 99, max: 99 },
            weapons: [],
          },
        ],
      });

      // 敵ユニットを選択しようとする
      await store.selectUnit("enemy-unit");
      expect(useGameStore.getState().interactionState).toBe("idle");
      expect(useGameStore.getState().selectedUnitId).toBeNull();

      // 自軍ユニットを選択する
      await store.selectUnit("own-unit");
      expect(useGameStore.getState().interactionState).toBe("unit_selected");
      expect(useGameStore.getState().selectedUnitId).toBe("own-unit");
    });

    it("should not open produce menu for non-active faction property", async () => {
      const store = useGameStore.getState();

      // P1ターン (緑軍: green) の状態をセット
      useGameStore.setState({
        turnInfo: { turn: 1, phase: "P1", funds: 1000 },
        engineWorker: {
          getProducibleUnits: async () => [{ type: "infantry", name: "歩兵", cost: 1000 }],
        } as unknown as import("comlink").Remote<import("../worker/engineWorker").EngineWorker>,
        propertyData: [
          {
            x: 0,
            y: 0,
            type: "factory",
            owner: "blue", // 敵拠点
            capture_points: 20,
            max_capture_points: 20,
          },
          {
            x: 1,
            y: 1,
            type: "factory",
            owner: "green", // 自軍拠点
            capture_points: 20,
            max_capture_points: 20,
          },
        ],
      });

      // 敵拠点に対して生産メニューを開こうとする
      await store.openProduceMenu(0, 0);
      expect(useGameStore.getState().produceMenu).toBeNull();
      expect(useGameStore.getState().interactionState).toBe("idle");

      // 自軍拠点に対して生産メニューを開こうとする
      await store.openProduceMenu(1, 1);
      expect(useGameStore.getState().produceMenu).not.toBeNull();
      expect(useGameStore.getState().interactionState).toBe("produce_menu");
    });
  });
});
