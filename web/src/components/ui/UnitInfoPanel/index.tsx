import { Paper, Text, Group, Badge } from '@mantine/core';
import { unitInfoContainer } from './index.css';

interface UnitInfoProps {
  unit: { id: string; type: string; faction: string; hp?: number } | null;
  terrain: { type: string; def?: number } | null;
}

const FACTION_MAP: Record<string, string> = { blue: '青軍', green: '緑軍' };
const TERRAIN_MAP: Record<string, string> = { 
  plains: '平地', 
  road: '道路',
  river: '川',
  bridge: '橋',
  mountain: '山',
  forest: '森',
  sea: '海',
  shoal: '浅瀬',
  city: '都市',
  factory: '工場',
  airport: '空港',
  port: '港',
  capital: '首都',
  unknown: '不明' 
};
const UNIT_MAP: Record<string, string> = { 
  infantry: '軽歩兵',
  mech: '重歩兵',
  recon: '装甲車',
  tank: '軽戦車',
  mdtank: '中戦車',
  tankz: '重戦車',
  artillery: '砲台',
  lightspgun: '軽自走砲',
  heavyspgun: '重自走砲',
  rockets: 'ロケット砲',
  antiair: '対空戦車',
  missiles: '対空ミサイル',
  fighter: '戦闘機',
  heavyfighter: '重戦闘機',
  bomber: '爆撃機',
  bcopters: '戦闘ヘリ',
  transporthelicopter: '輸送ヘリ',
  battleship: '戦艦',
  carrier: '空母',
  lander: '輸送船',
  supplytruck: '補給車',
};

export const UnitInfoPanel = ({ unit, terrain }: UnitInfoProps) => {
  if (!unit && !terrain) return null;

  return (
    <Paper shadow="md" p="md" radius="md" withBorder className={unitInfoContainer}>
      {unit && (
        <div style={{ marginBottom: terrain ? '12px' : '0' }}>
          <Group justify="space-between" mb="xs">
            <Text fw={700}>
              {UNIT_MAP[unit.type] || unit.type.replace('_', ' ')}
            </Text>
            <Badge color={unit.faction === 'blue' ? 'blue' : 'green'}>{FACTION_MAP[unit.faction] || unit.faction}</Badge>
          </Group>
          <Text size="sm">耐久力: {unit.hp ?? 10} / 10</Text>
        </div>
      )}
      
      {terrain && (
        <div style={{ borderTop: unit ? '1px solid #333' : 'none', paddingTop: unit ? '8px' : '0' }}>
          <Text fw={700} size="sm" c="dimmed">地形: {TERRAIN_MAP[terrain.type] || terrain.type}</Text>
          <Text size="xs" c="dimmed">防御効果: {terrain.def ?? 0}%</Text>
        </div>
      )}
    </Paper>
  );
};

