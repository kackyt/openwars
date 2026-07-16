import { style } from "@vanilla-extract/css";

/** ユニット・地形情報パネルのポップアップコンテナスタイル */
export const unitInfoContainer = style({
  position: "absolute",
  bottom: "20px",
  left: "20px",
  width: "250px",
  zIndex: 100,
});

/** 地形が存在する場合のユニット情報のスタイル */
export const unitSectionWithTerrain = style({
  marginBottom: "12px",
});

/** 地形が存在しない場合のユニット情報のスタイル */
export const unitSectionAlone = style({
  marginBottom: 0,
});

/** 武器情報などのマージントップ */
export const marginTopSmall = style({
  marginTop: "4px",
});

/** ユニット情報の下にある場合の地形情報の区切り線スタイル */
export const terrainSectionWithUnit = style({
  borderTop: "1px solid #333",
  paddingTop: "8px",
});

/** ユニット情報がない場合の地形情報のスタイル */
export const terrainSectionAlone = style({
  borderTop: "none",
  paddingTop: 0,
});
