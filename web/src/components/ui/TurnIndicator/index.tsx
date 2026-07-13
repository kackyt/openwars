import { Paper, Text, Group, ColorSwatch, Button, Stack } from '@mantine/core';
import { turnIndicatorContainer } from './index.css';

export const TurnIndicator = ({ turn, phase, funds, onEndTurn, isAiThinking }: { turn: number; phase: string; funds: number; onEndTurn: () => void; isAiThinking: boolean }) => {
  const isP1 = phase === 'P1';
  const isP2 = phase === 'P2';
  
  const phaseText = isAiThinking ? 'AI思考中...' : isP1 ? '緑軍のターン' : isP2 ? '青軍のターン' : phase;
  const phaseColor = isP1 ? 'green' : isP2 ? 'blue' : 'gray';

  return (
    <Paper shadow="md" p="sm" radius="md" withBorder className={turnIndicatorContainer} style={{ borderLeft: `6px solid var(--mantine-color-${phaseColor}-filled)` }}>
      <Group justify="space-between">
        <Stack gap={0}>
          <Text size="lg" fw={800}>Turn {turn}</Text>
          <Text size="sm" fw={700} c="yellow">資金: {funds} G</Text>
        </Stack>
        
        <Stack gap="xs" align="flex-end">
          <Group gap="xs">
            <ColorSwatch color={`var(--mantine-color-${phaseColor}-filled)`} size={14} />
            <Text size="sm" fw={700} c={phaseColor}>
              {phaseText}
            </Text>
          </Group>
          <Button size="xs" color="red" variant="light" onClick={onEndTurn} disabled={isAiThinking}>
            End Turn
          </Button>
        </Stack>
      </Group>
    </Paper>
  );
};
