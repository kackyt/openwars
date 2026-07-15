import { style } from "@vanilla-extract/css";

/** アプリケーションの全体レイアウトを管理するコンテナスタイル */
export const appContainer = style({
  position: "relative",
  height: "100vh",
  width: "100vw",
  overflow: "hidden",
});
