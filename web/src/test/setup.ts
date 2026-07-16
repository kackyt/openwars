// Vitest セットアップファイル
import "@testing-library/jest-dom";
import { vi } from "vitest";

// Web Worker の最小限のモック
class WorkerMock {
  url: string;
  onmessage: ((ev: MessageEvent) => void) | null = null;
  constructor(stringUrl: string) {
    this.url = stringUrl;
  }
  postMessage() {}
  terminate() {}
}

globalThis.Worker = WorkerMock as unknown as typeof Worker;

// PixiJSやその他のライブラリで使われるURLオブジェクトなどのモック
globalThis.URL.createObjectURL = vi.fn(() => "mock-url");

// React/PixiJS の警告を抑えるためのモック定義など
