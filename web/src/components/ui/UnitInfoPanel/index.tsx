import { Paper, Text, Group, Badge } from '@mantine/core';
import { unitInfoContainer } from './index.css';

interface UnitInfoProps {
  unit: { id: string; type: string; faction: string; hp?: number } | null;
  terrain: { type: string; def?: number } | null;
}

export const UnitInfoPanel = ({ unit, terrain }: UnitInfoProps) => {
  if (!unit && !terrain) return null;

  return (
    <Paper shadow="md" p="md" radius="md" withBorder className={unitInfoContainer}>
      {unit && (
        <div style={{ marginBottom: terrain ? '12px' : '0' }}>
          <Group justify="space-between" mb="xs">
            <Text fw={700} style={{ textTransform: 'capitalize' }}>
              {unit.type.replace('_', ' ')}
            </Text>
            <Badge color={unit.faction === 'blue' ? 'blue' : 'green'}>{unit.faction}</Badge>
          </Group>
          <Text size="sm">HP: {unit.hp ?? 10} / 10</Text>
        </div>
      )}
      
      {terrain && (
        <div style={{ borderTop: unit ? '1px solid #333' : 'none', paddingTop: unit ? '8px' : '0' }}>
          <Text fw={700} size="sm" c="dimmed">Terrain: {terrain.type}</Text>
          <Text size="xs" c="dimmed">DEF: {terrain.def ?? 0}</Text>
        </div>
      )}
    </Paper>
  );
};
