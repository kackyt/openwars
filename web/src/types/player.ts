import { PHASE_P1, PHASE_P2 } from "../constants/mappings";

export type PlayerId = 1 | 2;
export type WebAiVersion = "V1" | "V3" | "V4" | "V100" | "V200";
export type LoadedAiVersion = WebAiVersion | "V2";
export type PlayerControlMode = "human" | "ai";
export type PlayerMenuValue = "Human" | "AI V1" | "AI V3" | "AI V4" | "AI V100" | "AI V200";

export interface PlayerSetting {
  controlMode: PlayerControlMode;
  /** Human操作中も、次にAIへ切り替える場合のバージョンを保持する。 */
  aiVersion: WebAiVersion;
}

export type PlayerSettings = Record<PlayerId, PlayerSetting>;
export type WorkerPlayerAiVersionsDto = Record<PlayerId, WebAiVersion>;
export type LoadedPlayerAiVersionsDto = Record<string, LoadedAiVersion | undefined>;

export const PLAYER_IDS: readonly PlayerId[] = [1, 2];
export const PLAYER_MENU_OPTIONS: readonly PlayerMenuValue[] = [
  "Human",
  "AI V1",
  "AI V3",
  "AI V4",
  "AI V100",
  "AI V200",
];

export const createDefaultPlayerSettings = (): PlayerSettings => ({
  1: { controlMode: "human", aiVersion: "V4" },
  2: { controlMode: "ai", aiVersion: "V4" },
});

export const toWorkerPlayerAiVersions = (
  playerSettings: PlayerSettings,
): WorkerPlayerAiVersionsDto => ({
  1: playerSettings[1].aiVersion,
  2: playerSettings[2].aiVersion,
});

export const normalizeLoadedAiVersion = (version: LoadedAiVersion | undefined): WebAiVersion => {
  if (version === "V1") return "V1";
  if (version === "V4") return "V4";
  if (version === "V100") return "V100";
  if (version === "V200") return "V200";
  return "V3";
};

export const normalizeLoadedPlayerAiVersions = (
  versions: LoadedPlayerAiVersionsDto,
): WorkerPlayerAiVersionsDto => ({
  1: normalizeLoadedAiVersion(versions["1"]),
  2: normalizeLoadedAiVersion(versions["2"]),
});

export const mergeLoadedAiVersions = (
  playerSettings: PlayerSettings,
  versions: WorkerPlayerAiVersionsDto,
): PlayerSettings => ({
  1: { ...playerSettings[1], aiVersion: versions[1] },
  2: { ...playerSettings[2], aiVersion: versions[2] },
});

export const isAiPhase = (phase: string | undefined, playerSettings: PlayerSettings): boolean =>
  (phase === PHASE_P1 && playerSettings[1].controlMode === "ai") ||
  (phase === PHASE_P2 && playerSettings[2].controlMode === "ai");

export const toPlayerMenuValue = (setting: PlayerSetting): PlayerMenuValue => {
  if (setting.controlMode === "human") return "Human";
  if (setting.aiVersion === "V1") return "AI V1";
  if (setting.aiVersion === "V4") return "AI V4";
  if (setting.aiVersion === "V100") return "AI V100";
  if (setting.aiVersion === "V200") return "AI V200";
  return "AI V3";
};

export const applyPlayerMenuValue = (
  current: PlayerSetting,
  value: PlayerMenuValue,
): PlayerSetting => {
  if (value === "Human") {
    // Humanへ切り替えても、最後に選んだAIバージョンは忘れない。
    return { ...current, controlMode: "human" };
  }
  let aiVersion: WebAiVersion = "V3";
  if (value === "AI V1") aiVersion = "V1";
  else if (value === "AI V4") aiVersion = "V4";
  else if (value === "AI V100") aiVersion = "V100";
  else if (value === "AI V200") aiVersion = "V200";
  return {
    controlMode: "ai",
    aiVersion,
  };
};
