import { Paper, Text } from '@mantine/core';
import { turnIndicatorContainer } from './index.css';

export const TurnIndicator = ({ turn, phase }: { turn: number; phase: string }) => {
  return (
    <Paper shadow="sm" p="md" className={turnIndicatorContainer}>
      <Text size="xl" fw={700}>
        Turn {turn}
      </Text>
      <Text size="sm" c="dimmed">
        Phase: {phase}
      </Text>
    </Paper>
  );
};
