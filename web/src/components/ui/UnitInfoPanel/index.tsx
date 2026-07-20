import { Badge, Group, Paper, Text } from "@mantine/core";
import { FACTION_MAP, TERRAIN_MAP, UNIT_MAP } from "../../../constants/mappings";
import {
  marginTopSmall,
  terrainSectionAlone,
  terrainSectionWithUnit,
  unitInfoContainer,
  unitSectionAlone,
  unitSectionWithTerrain,
} from "./index.css";

interface UnitInfoProps {
  unit: {
    id: string;
    type: string;
    faction: string;
    hp?: number;
    fuel?: { current: number; max: number };
    weapons?: {
      name: string;
      ammo: number;
      max_ammo: number;
      min_range: number;
      max_range: number;
    }[];
  } | null;
  terrain: {
    type: string;
    def?: number;
    property?: {
      owner: string;
      capture_points: number;
      max_capture_points: number;
    } | null;
  } | null;
}

/**
 * ユニット情報パネルコンポーネント
 * 現在ホバー中のセルに存在する地形情報、およびユニットのステータス情報（HP、燃料、残弾等）を
 * 画面左下に固定表示します。
 */
export const UnitInfoPanel = ({ unit, terrain }: UnitInfoProps) => {
  if (!unit && !terrain) return null;

  return (
    <Paper className={unitInfoContainer} shadow="md" p="md" radius="md" withBorder>
      {unit && (
        <div className={terrain ? unitSectionWithTerrain : unitSectionAlone}>
          <Group justify="space-between" mb="xs">
            <Text fw={700}>{UNIT_MAP[unit.type] || unit.type.replace("_", " ")}</Text>
            <Badge color={unit.faction === "blue" ? "blue" : "green"}>
              {FACTION_MAP[unit.faction] || unit.faction}
            </Badge>
          </Group>
          <Text size="sm">耐久力: {unit.hp ?? 10} / 10</Text>
          {unit.fuel && (
            <Text size="sm">
              燃料: {unit.fuel.current} / {unit.fuel.max}
            </Text>
          )}
          {unit.weapons && unit.weapons.length > 0 && (
            <div className={marginTopSmall}>
              <Text size="xs" fw={700} c="dimmed">
                武器:
              </Text>
              {unit.weapons.map((w) => (
                <Text key={w.name} size="xs">
                  {w.name}: {w.ammo} / {w.max_ammo}
                  {w.max_range > 1 ? ` (射程: ${w.min_range}-${w.max_range})` : ""}
                </Text>
              ))}
            </div>
          )}
        </div>
      )}

      {terrain && (
        <div className={unit ? terrainSectionWithUnit : terrainSectionAlone}>
          <Text fw={700} size="sm" c="dimmed">
            地形: {TERRAIN_MAP[terrain.type] || terrain.type}
          </Text>
          <Text size="xs" c="dimmed">
            防御効果: {terrain.def ?? 0}%
          </Text>
          {terrain.property && (
            <div className={marginTopSmall}>
              <Text size="xs" c="dimmed">
                所属: {FACTION_MAP[terrain.property.owner] || "中立"}
              </Text>
              <Text size="xs" c="dimmed">
                耐久力: {terrain.property.capture_points} / {terrain.property.max_capture_points}
              </Text>
            </div>
          )}
        </div>
      )}
    </Paper>
  );
};
