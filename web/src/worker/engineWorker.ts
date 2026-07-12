import * as Comlink from "comlink";
import { WasmEngine, default as initWasm } from "../wasm/engine.js";
import wasmUrl from "../wasm/engine_bg.wasm?url";

export class EngineWorker {
  private engine: WasmEngine | null = null;

  async init(mapName: string, topology: string) {
    await initWasm(wasmUrl);
    this.engine = new WasmEngine(mapName, topology);
  }

  async getMap() {
    if (!this.engine) throw new Error("Engine not initialized");
    const jsonStr = this.engine.get_map();
    return JSON.parse(jsonStr as string);
  }

  async getUnits() {
    if (!this.engine) throw new Error("Engine not initialized");
    const jsonStr = this.engine.get_units();
    return JSON.parse(jsonStr as string);
  }

  async getTurnInfo() {
    if (!this.engine) throw new Error("Engine not initialized");
    const jsonStr = this.engine.get_turn_info();
    return JSON.parse(jsonStr as string);
  }

  async getProperties() {
    if (!this.engine) throw new Error("Engine not initialized");
    const jsonStr = this.engine.get_properties();
    return JSON.parse(jsonStr as string);
  }

  async getTerrainDefs() {
    if (!this.engine) throw new Error("Engine not initialized");
    const jsonStr = this.engine.get_terrain_defs();
    return JSON.parse(jsonStr as string);
  }

  async executeAiTurn() {
    if (!this.engine) throw new Error("Engine not initialized");
    const res = await this.engine.execute_ai_turn();
    return JSON.parse(res as string);
  }

  async calculateMovePath(unitId: string, destX: number, destY: number) {
    if (!this.engine) throw new Error("Engine not initialized");
    const res = await this.engine.calculate_move_path(unitId, destX, destY);
    return JSON.parse(res as string);
  }
}

Comlink.expose(EngineWorker);
