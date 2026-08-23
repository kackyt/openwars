import { describe, expect, it } from "vitest";
import { MAP_OPTIONS } from "./maps";

describe("MAP_OPTIONS", () => {
  it("exposes every embedded map through map_53", () => {
    expect(MAP_OPTIONS).toHaveLength(53);
    expect(MAP_OPTIONS[0]).toBe("map_1");
    expect(MAP_OPTIONS[MAP_OPTIONS.length - 1]).toBe("map_53");
  });
});
