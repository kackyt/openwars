import { style } from "@vanilla-extract/css";
import { DROP_MENU_MIN_WIDTH, DROP_MENU_Z_INDEX } from "../../../constants/rendering";

/** 降車ユニット選択メニューのポップアップコンテナスタイル */
export const dropMenuContainer = style({
  position: "absolute",
  top: "50%",
  left: "50%",
  transform: "translate(-50%, -50%)",
  zIndex: DROP_MENU_Z_INDEX,
  minWidth: `${DROP_MENU_MIN_WIDTH}px`,
});
