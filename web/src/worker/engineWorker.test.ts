import { beforeEach, describe, expect, it, vi } from "vitest";

const wasmMocks = vi.hoisted(() => ({
  initWasm: vi.fn(async () => undefined),
  constructor: vi.fn(),
  setPlayerAiVersion: vi.fn(),
  getPlayerAiVersions: vi.fn(() => JSON.stringify({ 1: "V3", 2: "V3" })),
  getSuppliableTargets: vi.fn<(unitId: string, destX: number, destY: number) => string>(() =>
    JSON.stringify([]),
  ),
  submitSupplyCommand: vi.fn<(supplierId: string, targetId: string) => void>(),
}));

vi.mock("../wasm/engine.js", () => ({
  default: wasmMocks.initWasm,
  WasmEngine: class {
    constructor(mapName: string, topology: string) {
      wasmMocks.constructor(mapName, topology);
    }

    set_player_ai_version(playerId: number, version: string) {
      wasmMocks.setPlayerAiVersion(playerId, version);
    }

    get_player_ai_versions() {
      return wasmMocks.getPlayerAiVersions();
    }

    get_suppliable_targets(unitId: string, destX: number, destY: number) {
      return wasmMocks.getSuppliableTargets(unitId, destX, destY);
    }

    submit_supply_command(supplierId: string, targetId: string) {
      return wasmMocks.submitSupplyCommand(supplierId, targetId);
    }
  },
}));

vi.mock("../wasm/engine_bg.wasm?url", () => ({ default: "mock-engine.wasm" }));

import { EngineWorker } from "./engineWorker";

describe("EngineWorker player AI versions", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    wasmMocks.getPlayerAiVersions.mockReturnValue(JSON.stringify({ 1: "V3", 2: "V3" }));
  });

  it("applies both player versions immediately after construction", async () => {
    const worker = new EngineWorker();

    await worker.init("map_1", "square", { 1: "V1", 2: "V3" });

    expect(wasmMocks.constructor).toHaveBeenCalledWith("map_1", "square");
    expect(wasmMocks.setPlayerAiVersion).toHaveBeenNthCalledWith(1, 1, "V1");
    expect(wasmMocks.setPlayerAiVersion).toHaveBeenNthCalledWith(2, 2, "V3");
  });

  it("normalizes a loaded V2 and reapplies selectable versions", async () => {
    const worker = new EngineWorker();
    await worker.init("map_1", "square", { 1: "V3", 2: "V3" });
    wasmMocks.setPlayerAiVersion.mockClear();
    wasmMocks.getPlayerAiVersions.mockReturnValue(JSON.stringify({ 1: "V2", 2: "V1" }));

    const versions = await worker.reapplyNormalizedPlayerAiVersions();

    expect(versions).toEqual({ 1: "V3", 2: "V1" });
    expect(wasmMocks.setPlayerAiVersion).toHaveBeenNthCalledWith(1, 1, "V3");
    expect(wasmMocks.setPlayerAiVersion).toHaveBeenNthCalledWith(2, 2, "V1");
  });
});

describe("EngineWorker supply API", () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it("returns supply targets from the WASM engine", async () => {
    wasmMocks.getSuppliableTargets.mockReturnValue(
      JSON.stringify([{ id: "target-1", x: 3, y: 2 }]),
    );
    const worker = new EngineWorker();
    await worker.init("map_1", "square", { 1: "V3", 2: "V3" });

    await expect(worker.getSuppliableTargets("supplier-1", 2, 2)).resolves.toEqual([
      { id: "target-1", x: 3, y: 2 },
    ]);
    expect(wasmMocks.getSuppliableTargets).toHaveBeenCalledWith("supplier-1", 2, 2);
  });

  it("forwards a single supply command to the WASM engine", async () => {
    const worker = new EngineWorker();
    await worker.init("map_1", "square", { 1: "V3", 2: "V3" });

    await worker.submitSupplyCommand("supplier-1", "target-1");

    expect(wasmMocks.submitSupplyCommand).toHaveBeenCalledOnce();
    expect(wasmMocks.submitSupplyCommand).toHaveBeenCalledWith("supplier-1", "target-1");
  });
});
