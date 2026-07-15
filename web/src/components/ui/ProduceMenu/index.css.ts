import { style } from "@vanilla-extract/css";

export const produceMenuContainer = style({
  position: "absolute",
  zIndex: 1000,
  minWidth: "200px",
  backgroundColor: "rgba(20, 20, 20, 0.95)",
  backdropFilter: "blur(8px)",
});

export const unitName = style({
  flex: 1,
  fontWeight: "bold",
  textAlign: "left",
});
