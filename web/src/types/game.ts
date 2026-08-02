export interface UnitData {
  id: string;
  type: string;
  faction: string;
  x: number;
  y: number;
  hp: number;
  is_loaded: boolean;
  is_exhausted: boolean;
  has_moved?: boolean;
  fuel: { current: number; max: number };
  weapons: { name: string; ammo: number; max_ammo: number; min_range: number; max_range: number }[];
}

export interface TurnInfo {
  turn: number;
  phase: string;
  funds: number;
}

export interface PropertyData {
  x: number;
  y: number;
  type: string;
  owner: string;
  capture_points: number;
  max_capture_points: number;
}
