import { Paper, Text, Group, ColorSwatch } from '@mantine/core';
import { turnIndicatorContainer } from './index.css';

export const TurnIndicator = ({ turn, phase }: { turn: number; phase: string }) => {
  const isP1 = phase === 'P1';
  const isP2 = phase === 'P2';
  
  const phaseText = isP1 ? '緑軍のターン' : isP2 ? '青軍のターン' : phase;
  const phaseColor = isP1 ? 'green' : isP2 ? 'blue' : 'gray';

  return (
    <Paper shadow="md" p="sm" radius="md" withBorder className={turnIndicatorContainer} style={{ borderLeft: `6px solid var(--mantine-color-${phaseColor}-filled)` }}>
      <Group justify="space-between">
        <Text size="lg" fw={800}>
          Turn {turn}
        </Text>
        <Group gap="xs">
          <ColorSwatch color={`var(--mantine-color-${phaseColor}-filled)`} size={14} />
          <Text size="sm" fw={700} c={phaseColor}>
            {phaseText}
          </Text>
        </Group>
      </Group>
    </Paper>
  );
};
