// ゲーム内の種別マッピングや文字列表現の共通定義モジュール
// 日本語コメントで役割を記述しています。

/** 勢力の表示名マッピング */
export const FACTION_MAP: Record<string, string> = {
  blue: "青軍",
  green: "緑軍",
  neutral: "中立",
};

/** 地形の表示名マッピング */
export const TERRAIN_MAP: Record<string, string> = {
  plains: "平地",
  road: "道路",
  river: "川",
  bridge: "橋",
  mountain: "山",
  forest: "森",
  sea: "海",
  shoal: "浅瀬",
  city: "都市",
  factory: "工場",
  airport: "空港",
  port: "港",
  capital: "首都",
  unknown: "不明",
};

/** ユニットの表示名マッピング */
export const UNIT_MAP: Record<string, string> = {
  infantry: "軽歩兵",
  mech: "重歩兵",
  recon: "装甲車",
  tank: "軽戦車",
  mdtank: "中戦車",
  tankz: "重戦車",
  artillery: "砲台",
  lightspgun: "軽自走砲",
  heavyspgun: "重自走砲",
  rockets: "ロケットランチャー",
  antiair: "対空戦車",
  missiles: "対空ミサイル",
  fighter: "軽戦闘機",
  heavyfighter: "重戦闘機",
  bomber: "爆撃機",
  bcopters: "戦闘ヘリ",
  transporthelicopter: "輸送ヘリ",
  battleship: "戦艦",
  carrier: "空母",
  lander: "輸送船",
  supplytruck: "補給輸送車",
};

/** ユニット種別からアセット画像ファイル名へのマッピング */
export const UNIT_IMAGE_MAP: Record<string, string> = {
  infantry: "infantry",
  mech: "mech_infantry",
  recon: "armored_vehicle",
  tank: "light_tank",
  mdtank: "medium_tank",
  tankz: "heavy_tank",
  artillery: "artillery",
  lightspgun: "light_artillery",
  heavyspgun: "heavy_artillery",
  rockets: "rocket",
  antiair: "anti_air_tank",
  missiles: "anti_air_missile",
  fighter: "fighter",
  heavyfighter: "heavy_fighter",
  bomber: "bomber",
  bcopters: "battle_copter",
  transporthelicopter: "transport_copter",
  battleship: "battleship",
  carrier: "carrier",
  lander: "lander",
  supplytruck: "supply_truck",
};

/** 地形種別からアセット画像ファイル名へのマッピング */
export const TERRAIN_IMAGE_MAP: Record<string, string> = {
  plains: "plain",
  forest: "woods",
  mountain: "mountain",
  river: "river",
  road: "road",
  bridge: "bridge",
  sea: "sea",
  shoal: "shoal",
};

/** 生産可能な施設のリスト (マジックストリング配列の定数化) */
export const PRODUCIBLE_TERRAINS = ["factory", "airport", "port", "capital", "city"];

// フェーズ / プレイヤーカラー等の定数
export const PHASE_P1 = "P1";
export const PHASE_P2 = "P2";

export const FACTION_BLUE = "blue";
export const FACTION_GREEN = "green";
export const FACTION_NEUTRAL = "neutral";

export const TERRAIN_CAPITAL = "capital";
