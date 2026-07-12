import * as Comlink from "comlink";
import init, {
  calculate_move_path,
  execute_ai_turn,
  get_game_state,
  get_turn_info,
} from "../wasm/engine.js";

export class EngineWorker {
  async initWasm() {
    await init();
  }

  getGameState() {
    return get_game_state();
  }

  getTurnInfo() {
    return get_turn_info();
  }

  async executeAiTurn() {
    return await execute_ai_turn();
  }

  async calculateMovePath(unitId: string, destX: number, destY: number) {
    return await calculate_move_path(unitId, destX, destY);
  }
}

Comlink.expose(EngineWorker);
