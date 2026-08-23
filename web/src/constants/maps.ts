/** エンジンへ組み込む全マップ。マスターデータの map_1〜map_53 と対応する。 */
export const MAP_OPTIONS = Array.from({ length: 53 }, (_, index) => `map_${index + 1}`);
