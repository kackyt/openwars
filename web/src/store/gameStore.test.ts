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
});
