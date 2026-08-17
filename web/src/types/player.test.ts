import { describe, expect, it } from "vitest";
import {
  applyPlayerMenuValue,
  createDefaultPlayerSettings,
  isAiPhase,
  mergeLoadedAiVersions,
  normalizeLoadedPlayerAiVersions,
  PLAYER_MENU_OPTIONS,
  toPlayerMenuValue,
  toWorkerPlayerAiVersions,
} from "./player";

describe("player settings helpers", () => {
  it("exposes only Human, AI V1, AI V3, and AI V4 in the menu", () => {
    expect(PLAYER_MENU_OPTIONS).toEqual(["Human", "AI V1", "AI V3", "AI V4"]);
    expect(PLAYER_MENU_OPTIONS.some((option) => option.includes("V2"))).toBe(false);
  });

  it("creates the default P1 Human / P2 AI V4 settings", () => {
    expect(createDefaultPlayerSettings()).toEqual({
      1: { controlMode: "human", aiVersion: "V4" },
      2: { controlMode: "ai", aiVersion: "V4" },
    });
  });

  it("converts remembered versions to the Worker DTO independently of control mode", () => {
    expect(
      toWorkerPlayerAiVersions({
        1: { controlMode: "human", aiVersion: "V1" },
        2: { controlMode: "ai", aiVersion: "V4" },
      }),
    ).toEqual({ 1: "V1", 2: "V4" });
  });

  it("normalizes loaded V2 and missing versions to V3 while preserving V1 and V4", () => {
    expect(normalizeLoadedPlayerAiVersions({ 1: "V1", 2: "V2" })).toEqual({
      1: "V1",
      2: "V3",
    });
    expect(normalizeLoadedPlayerAiVersions({ 1: "V4", 2: "V2" })).toEqual({
      1: "V4",
      2: "V3",
    });
    expect(normalizeLoadedPlayerAiVersions({})).toEqual({ 1: "V3", 2: "V3" });
  });

  it("updates loaded versions without changing Human/AI control modes", () => {
    const current = {
      1: { controlMode: "human", aiVersion: "V3" },
      2: { controlMode: "ai", aiVersion: "V1" },
    } as const;

    expect(mergeLoadedAiVersions(current, { 1: "V4", 2: "V3" })).toEqual({
      1: { controlMode: "human", aiVersion: "V4" },
      2: { controlMode: "ai", aiVersion: "V3" },
    });
  });

  it("keeps the remembered version when switching to Human", () => {
    const human = applyPlayerMenuValue({ controlMode: "ai", aiVersion: "V1" }, "Human");

    expect(human).toEqual({ controlMode: "human", aiVersion: "V1" });
    expect(toPlayerMenuValue(human)).toBe("Human");
    expect(applyPlayerMenuValue(human, "AI V3")).toEqual({
      controlMode: "ai",
      aiVersion: "V3",
    });
    expect(applyPlayerMenuValue(human, "AI V4")).toEqual({
      controlMode: "ai",
      aiVersion: "V4",
    });
    expect(toPlayerMenuValue({ controlMode: "ai", aiVersion: "V4" })).toBe("AI V4");
  });

  it("detects AI phases from the typed settings record", () => {
    const settings = createDefaultPlayerSettings();

    expect(isAiPhase("P1", settings)).toBe(false);
    expect(isAiPhase("P2", settings)).toBe(true);
    expect(isAiPhase(undefined, settings)).toBe(false);
  });
});
