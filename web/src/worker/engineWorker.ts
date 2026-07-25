import * as Comlink from "comlink";
import type { PropertyData, TurnInfo, UnitData } from "../store/gameStore";
import { default as initWasm, WasmEngine } from "../wasm/engine.js";
import wasmUrl from "../wasm/engine_bg.wasm?url";

/**
 * @class EngineWorker
 * @description Web Worker 内で WASM ゲームエンジンをインスタンス化・実行し、
 * メインスレッド（Zustandストア）との通信を中継するクラスです。
 * データの受け渡しは JSON 文字列にシリアライズして行われます。
 */
export class EngineWorker {
  private engine: WasmEngine | null = null;

  /**
   * Wasm ゲームエンジンを初期化します。
   * @param mapName マップ名
   * @param topology グリッド形式
   */
  async init(mapName: string, topology: string): Promise<void> {
    await initWasm(wasmUrl);
    this.engine = new WasmEngine(mapName, topology);
  }

  /**
   * 現在のマップセル配置を取得します。
   */
  async getMap(): Promise<string[][]> {
    if (!this.engine) throw new Error("Engine not initialized");
    // wasm-bindgen が生成するコード上、戻り値が unknown になり得るため string として型アサーションを行っています。
    const jsonStr = this.engine.get_map();
    return JSON.parse(jsonStr as string);
  }

  /**
   * 現在の全ユニットデータを取得します。
   */
  async getUnits(): Promise<UnitData[]> {
    if (!this.engine) throw new Error("Engine not initialized");
    const jsonStr = this.engine.get_units();
    return JSON.parse(jsonStr as string);
  }

  /**
   * 現在のターンとフェーズの情報を取得します。
   */
  async getTurnInfo(): Promise<TurnInfo | null> {
    if (!this.engine) throw new Error("Engine not initialized");
    const jsonStr = this.engine.get_turn_info();
    return JSON.parse(jsonStr as string);
  }

  /**
   * 現在のプロパティ（都市・首都など施設）のデータを取得します。
   */
  async getProperties(): Promise<PropertyData[]> {
    if (!this.engine) throw new Error("Engine not initialized");
    const jsonStr = this.engine.get_properties();
    return JSON.parse(jsonStr as string);
  }

  /**
   * 地形ディフェンスなどの地形定義情報を取得します。
   */
  async getTerrainDefs(): Promise<Record<string, number>> {
    if (!this.engine) throw new Error("Engine not initialized");
    const jsonStr = this.engine.get_terrain_defs();
    return JSON.parse(jsonStr as string);
  }

  /**
   * AIの思考を実行し、行動結果（行動したか、撃破されたユニットがあるか）を取得します。
   */
  async executeAiTurn(): Promise<{ acted: boolean; destroyed: string[] }> {
    if (!this.engine) throw new Error("Engine not initialized");
    const jsonStr = this.engine.execute_ai_turn();
    return JSON.parse(jsonStr as string);
  }

  /**
   * ユニットの移動可能範囲セル一覧を取得します。
   * @param unitId ユニットID
   */
  async getReachableCells(unitId: string): Promise<{ x: number; y: number }[]> {
    if (!this.engine) throw new Error("Engine not initialized");
    const jsonStr = this.engine.get_reachable_cells(unitId);
    return JSON.parse(jsonStr as string);
  }

  /**
   * 現在のプレイヤーが指定ユニットを選択できるかを取得します。
   */
  async isUnitSelectable(unitId: string): Promise<boolean> {
    if (!this.engine) throw new Error("Engine not initialized");
    return this.engine.is_unit_selectable(unitId);
  }

  /**
   * 指定ユニットが新たな移動先を選択できるかを取得します。
   */
  async canUnitMove(unitId: string): Promise<boolean> {
    if (!this.engine) throw new Error("Engine not initialized");
    return this.engine.can_unit_move(unitId);
  }

  /**
   * ユニットが目的地に移動した後に実行可能なアクション一覧を取得します。
   * @param unitId ユニットID
   * @param destX 目的地X
   * @param destY 目的地Y
   */
  async getAvailableActions(unitId: string, destX: number, destY: number): Promise<string[]> {
    if (!this.engine) throw new Error("Engine not initialized");
    const jsonStr = this.engine.get_available_actions(unitId, destX, destY);
    return JSON.parse(jsonStr as string);
  }

  /**
   * 指定位置のユニットが攻撃可能なターゲット一覧を取得します。
   * @param unitId ユニットID
   * @param destX 移動先X
   * @param destY 移動先Y
   */
  async getAttackableTargets(
    unitId: string,
    destX: number,
    destY: number,
  ): Promise<{ id: string; x: number; y: number }[]> {
    if (!this.engine) throw new Error("Engine not initialized");
    const jsonStr = this.engine.get_attackable_targets(unitId, destX, destY);
    return JSON.parse(jsonStr as string);
  }

  /**
   * 指定座標の拠点で生産可能なユニット種別一覧を取得します。
   * @param x 拠点座標X
   * @param y 拠点座標Y
   */
  async getProducibleUnits(
    x: number,
    y: number,
  ): Promise<{ type: string; name: string; cost: number }[]> {
    if (!this.engine) throw new Error("Engine not initialized");
    const jsonStr = this.engine.get_producible_units(x, y);
    return JSON.parse(jsonStr as string);
  }

  /**
   * ユニット移動コマンドを送信します。
   * @param unitId ユニットID
   * @param destX 目的地X
   * @param destY 目的地Y
   */
  async submitMoveCommand(unitId: string, destX: number, destY: number): Promise<void> {
    if (!this.engine) throw new Error("Engine not initialized");
    this.engine.submit_move_command(unitId, destX, destY);
  }

  /**
   * ユニット待機コマンドを送信します。
   * @param unitId ユニットID
   */
  async submitWaitCommand(unitId: string): Promise<void> {
    if (!this.engine) throw new Error("Engine not initialized");
    this.engine.submit_wait_command(unitId);
  }

  /**
   * ユニット攻撃コマンドを送信します。
   * @param unitId 攻撃側ユニットID
   * @param targetId 防御側ユニットID
   * @returns 撃破されたユニットのIDリスト
   */
  async submitAttackCommand(unitId: string, targetId: string): Promise<string[]> {
    if (!this.engine) throw new Error("Engine not initialized");
    const jsonStr = this.engine.submit_attack_command(unitId, targetId);
    return JSON.parse(jsonStr as string);
  }

  /**
   * 施設占領コマンドを送信します。
   * @param unitId 占領を実行するユニットID
   */
  async submitCaptureCommand(unitId: string): Promise<void> {
    if (!this.engine) throw new Error("Engine not initialized");
    this.engine.submit_capture_command(unitId);
  }

  /**
   * 施設修復コマンドを送信します。
   * @param unitId 修復を実行するユニットID
   */
  async submitRepairCommand(unitId: string): Promise<void> {
    if (!this.engine) throw new Error("Engine not initialized");
    this.engine.submit_repair_command(unitId);
  }

  /**
   * ユニット積載コマンドを送信します。
   * @param unitId 搭載するユニットID
   * @param targetId 搭載先（輸送船等）のユニットID
   */
  async submitLoadCommand(unitId: string, targetId: string): Promise<void> {
    if (!this.engine) throw new Error("Engine not initialized");
    this.engine.submit_load_command(unitId, targetId);
  }

  /**
   * ユニット生産コマンドを送信します。
   * @param unitType 生産するユニット種別
   * @param x 生産座標X
   * @param y 生産座標Y
   */
  async submitProduceCommand(unitType: string, x: number, y: number): Promise<void> {
    if (!this.engine) throw new Error("Engine not initialized");
    this.engine.submit_produce_command(unitType, x, y);
  }

  /**
   * 輸送ユニットに積載されているユニット一覧を取得します。
   * @param transportId 輸送ユニットID
   */
  async getLoadedUnits(transportId: string): Promise<{ id: string; type: string }[]> {
    if (!this.engine) throw new Error("Engine not initialized");
    const jsonStr = this.engine.get_loaded_units(transportId);
    return JSON.parse(jsonStr as string);
  }

  /**
   * 指定積載ユニットを降ろせるマスの一覧を取得します。
   * @param transportId 輸送ユニットID
   * @param cargoId 降車対象のユニットID
   */
  async getDroppableTiles(
    transportId: string,
    cargoId: string,
  ): Promise<{ x: number; y: number }[]> {
    if (!this.engine) throw new Error("Engine not initialized");
    const jsonStr = this.engine.get_droppable_tiles(transportId, cargoId);
    return JSON.parse(jsonStr as string);
  }

  /**
   * 降車コマンドを送信します。
   * @param transportId 輸送ユニットID
   * @param cargoId 降車ユニットID
   * @param targetX 降車先X座標
   * @param targetY 降車先Y座標
   */
  async submitUnloadCommand(
    transportId: string,
    cargoId: string,
    targetX: number,
    targetY: number,
  ): Promise<void> {
    if (!this.engine) throw new Error("Engine not initialized");
    this.engine.submit_unload_command(transportId, cargoId, targetX, targetY);
  }

  /**
   * ユニット合流コマンドを送信します。
   * @param unitId 合流元ユニットID
   * @param targetId 合流先ユニットID
   */
  async submitMergeCommand(unitId: string, targetId: string): Promise<void> {
    if (!this.engine) throw new Error("Engine not initialized");
    this.engine.submit_merge_command(unitId, targetId);
  }

  /**
   * プレイヤーのターンを終了させます。
   */
  async endTurn(): Promise<void> {
    if (!this.engine) throw new Error("Engine not initialized");
    this.engine.submit_end_turn_command();
  }

  /**
   * ゲームオーバー状態になっているかチェックします。
   * @returns 勝者プレイヤー番号、または引き分け情報
   */
  async checkGameOver(): Promise<{ winner: number } | { draw: boolean } | null> {
    if (!this.engine) throw new Error("Engine not initialized");
    const jsonStr = this.engine.check_game_over();
    return JSON.parse(jsonStr as string);
  }

  /**
   * ゲームデータをエクスポートします。
   */
  async exportSaveData(mapName: string): Promise<string> {
    if (!this.engine) throw new Error("Engine not initialized");
    return this.engine.export_save_data(mapName);
  }

  /**
   * ゲームデータをインポートします。
   */
  async importSaveData(saveStr: string): Promise<void> {
    if (!this.engine) throw new Error("Engine not initialized");
    this.engine.import_save_data(saveStr);
  }
}

Comlink.expose(EngineWorker);
