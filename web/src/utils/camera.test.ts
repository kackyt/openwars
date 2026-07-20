import { describe, expect, it } from "vitest";
import { clampCameraPosition, globalToGrid, gridToGlobal } from "./camera";

describe("camera utils", () => {
  describe("globalToGrid", () => {
    it("should calculate correct grid coordinates for square topology", () => {
      // 48px タイル、カメラ位置(0,0)、四角形
      const result = globalToGrid(100, 60, 0, 0, "square", 48);
      // 100 / 48 = 2.08 -> 2
      // 60 / 48 = 1.25 -> 1
      expect(result).toEqual({ gridX: 2, gridY: 1 });
    });

    it("should offset gridX for odd rows in hex topology", () => {
      // hexの場合、奇数行（gridY=1）は tileSize/2（24px）右にずれる
      // Y = 60 (gridY = 1), X = 100
      // localX - 24 = 76 -> 76 / 48 = 1.58 -> 1
      const result = globalToGrid(100, 60, 0, 0, "hex", 48);
      expect(result).toEqual({ gridX: 1, gridY: 1 });
    });

    it("should take camera offset into account", () => {
      // カメラが右下に 10px ずつずれている場合
      // X = 100 -> localX = 100 - 10 = 90 -> 90 / 48 = 1.875 -> 1
      // Y = 60 -> localY = 60 - 10 = 50 -> 50 / 48 = 1.04 -> 1
      const result = globalToGrid(100, 60, 10, 10, "square", 48);
      expect(result).toEqual({ gridX: 1, gridY: 1 });
    });
  });

  describe("gridToGlobal", () => {
    it("should calculate correct screen coordinates for square topology", () => {
      const result = gridToGlobal(2, 1, -10, -20, "square", 48);
      // X: 2 * 48 + 0 - 10 = 86
      // Y: 1 * 48 - 20 = 28
      expect(result).toEqual({ globalX: 86, globalY: 28 });
    });

    it("should offset globalX for odd rows in hex topology", () => {
      const result = gridToGlobal(2, 1, -10, -20, "hex", 48);
      // X: 2 * 48 + 24 - 10 = 110
      // Y: 1 * 48 - 20 = 28
      expect(result).toEqual({ globalX: 110, globalY: 28 });
    });
  });

  describe("clampCameraPosition", () => {
    it("should clamp camera coordinates within padded boundaries", () => {
      // マップサイズ 480x480、ウィンドウサイズ 640x480
      // PADDING_LEFT=60, PADDING_TOP=120 にクランプされる
      const result = clampCameraPosition(200, 200, 480, 480, 640, 480);
      expect(result).toEqual({ x: 60, y: 120 });
    });

    it("should allow scrolling with padding to expose map edges around UI panels", () => {
      // マップサイズ 960x960, ウィンドウサイズ 640x480
      // minX = 640 - 960 - 120 = -440, maxX = 60
      // minY = 480 - 960 - 140 = -620, maxY = 120
      const resultNormal = clampCameraPosition(-100, -200, 960, 960, 640, 480);
      expect(resultNormal).toEqual({ x: -100, y: -200 });

      // 左上の限界値
      const resultTooFarLeftUp = clampCameraPosition(200, 300, 960, 960, 640, 480);
      expect(resultTooFarLeftUp).toEqual({ x: 60, y: 120 });

      // 右下の限界値
      const resultTooFarRightDown = clampCameraPosition(-700, -800, 960, 960, 640, 480);
      expect(resultTooFarRightDown).toEqual({ x: -440, y: -620 });
    });
  });
});
