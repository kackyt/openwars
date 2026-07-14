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

  async executeAiTurn(): Promise<{ acted: boolean, destroyed: string[] }> {
    if (!this.engine) throw new Error("Engine not initialized");
    const jsonStr = this.engine.execute_ai_turn();
    return JSON.parse(jsonStr as string);
  }

  async getReachableCells(unitId: string) {
    if (!this.engine) throw new Error("Engine not initialized");
    const jsonStr = this.engine.get_reachable_cells(unitId);
    return JSON.parse(jsonStr as string);
  }

  async getAvailableActions(unitId: string, destX: number, destY: number) {
    if (!this.engine) throw new Error("Engine not initialized");
    const jsonStr = this.engine.get_available_actions(unitId, destX, destY);
    return JSON.parse(jsonStr as string);
  }

  async getAttackableTargets(unitId: string, destX: number, destY: number) {
    if (!this.engine) throw new Error("Engine not initialized");
    const jsonStr = this.engine.get_attackable_targets(unitId, destX, destY);
    return JSON.parse(jsonStr as string);
  }

  async getProducibleUnits(x: number, y: number) {
    if (!this.engine) throw new Error("Engine not initialized");
    const jsonStr = this.engine.get_producible_units(x, y);
    return JSON.parse(jsonStr as string);
  }

  async submitMoveCommand(unitId: string, destX: number, destY: number) {
    if (!this.engine) throw new Error("Engine not initialized");
    this.engine.submit_move_command(unitId, destX, destY);
  }

  async submitWaitCommand(unitId: string) {
    if (!this.engine) throw new Error("Engine not initialized");
    this.engine.submit_wait_command(unitId);
  }

  async submitAttackCommand(unitId: string, targetId: string): Promise<string[]> {
    if (!this.engine) throw new Error("Engine not initialized");
    const jsonStr = this.engine.submit_attack_command(unitId, targetId);
    return JSON.parse(jsonStr as string);
  }

  async submitCaptureCommand(unitId: string) {
    if (!this.engine) throw new Error("Engine not initialized");
    this.engine.submit_capture_command(unitId);
  }

  async submitLoadCommand(unitId: string, targetId: string) {
    if (!this.engine) throw new Error("Engine not initialized");
    this.engine.submit_load_command(unitId, targetId);
  }

  async submitProduceCommand(unitType: string, x: number, y: number) {
    if (!this.engine) throw new Error("Engine not initialized");
    this.engine.submit_produce_command(unitType, x, y);
  }

  // 輸送ユニットに積載されているユニット一覧を取得する
  async getLoadedUnits(transportId: string): Promise<{ id: string, type: string }[]> {
    if (!this.engine) throw new Error("Engine not initialized");
    const jsonStr = this.engine.get_loaded_units(transportId);
    return JSON.parse(jsonStr as string);
  }

  // 指定ユニットの降車可能マス一覧を取得する
  async getDroppableTiles(transportId: string, cargoId: string): Promise<{ x: number, y: number }[]> {
    if (!this.engine) throw new Error("Engine not initialized");
    const jsonStr = this.engine.get_droppable_tiles(transportId, cargoId);
    return JSON.parse(jsonStr as string);
  }

  // 降車コマンドを送信する
  async submitUnloadCommand(transportId: string, cargoId: string, targetX: number, targetY: number) {
    if (!this.engine) throw new Error("Engine not initialized");
    this.engine.submit_unload_command(transportId, cargoId, targetX, targetY);
  }

  // 合流コマンドを送信する
  async submitMergeCommand(unitId: string, targetId: string) {
    if (!this.engine) throw new Error("Engine not initialized");
    this.engine.submit_merge_command(unitId, targetId);
  }

  // ターン終了コマンドを送信
  async endTurn() {
    if (!this.engine) throw new Error("Engine not initialized");
    this.engine.submit_end_turn_command();
  }

  // ゲームオーバー状態のチェック
  async checkGameOver(): Promise<{ winner: number } | { draw: boolean } | null> {
    if (!this.engine) throw new Error("Engine not initialized");
    const jsonStr = this.engine.check_game_over();
    return JSON.parse(jsonStr as string);
  }

}

Comlink.expose(EngineWorker);
