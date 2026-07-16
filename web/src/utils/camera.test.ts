import { describe, expect, it } from "vitest";
import { clampCameraPosition, globalToGrid } from "./camera";

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

  describe("clampCameraPosition", () => {
    it("should clamp camera coordinates within maps boundaries", () => {
      // マップサイズ 480x480、ウィンドウサイズ 640x480
      // ウィンドウの方が大きい場合、minX = 640 - 480 = 160. だが Math.min(0, 160) = 0.
      // したがって、xは [0, 0] にクランプされる。
      const result = clampCameraPosition(100, 100, 480, 480, 640, 480);
      expect(result).toEqual({ x: 0, y: 0 });
    });

    it("should allow scroll up to negative map boundaries when window is smaller", () => {
      // マップサイズ 960x960, ウィンドウサイズ 640x480
      // minX = 640 - 960 = -320
      // minY = 480 - 960 = -480
      // 正常スクロール範囲は x: [-320, 0], y: [-480, 0]
      const resultNormal = clampCameraPosition(-100, -200, 960, 960, 640, 480);
      expect(resultNormal).toEqual({ x: -100, y: -200 });

      // 左上の境界外にクランプされる
      const resultTooFarLeftUp = clampCameraPosition(100, 50, 960, 960, 640, 480);
      expect(resultTooFarLeftUp).toEqual({ x: 0, y: 0 });

      // 右下の境界外にクランプされる
      const resultTooFarRightDown = clampCameraPosition(-500, -600, 960, 960, 640, 480);
      expect(resultTooFarRightDown).toEqual({ x: -320, y: -480 });
    });
  });
});
