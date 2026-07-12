import { Paper, Stack, Button, Text, ScrollArea } from '@mantine/core';
import { produceMenuContainer } from './index.css';

interface ProduceMenuProps {
  x: number;
  y: number;
  units: { type: string; name: string; cost: number }[];
  currentFunds: number;
  onSelect: (unitType: string) => void;
  onClose: () => void;
}

export const ProduceMenu = ({ x, y, units, currentFunds, onSelect, onClose }: ProduceMenuProps) => {
  if (units.length === 0) return null;

  return (
    <Paper 
      shadow="xl" 
      p="xs" 
      withBorder 
      className={produceMenuContainer}
      style={{ left: `${x}px`, top: `${y}px` }}
    >
      <ScrollArea h={300} offsetScrollbars>
        <Stack gap="xs">
          {units.map((unit) => {
            const canAfford = currentFunds >= unit.cost;
            return (
              <Button 
                key={unit.type} 
                variant="light" 
                size="sm" 
                fullWidth 
                disabled={!canAfford}
                onClick={() => onSelect(unit.type)}
                styles={{ inner: { justifyContent: 'space-between', width: '100%' } }}
              >
                <span className="flex-1 font-bold">{unit.name}</span>
                <Text size="sm" c={canAfford ? 'yellow' : 'red'}>{unit.cost}</Text>
              </Button>
            );
          })}
        </Stack>
      </ScrollArea>
      <Button variant="subtle" color="gray" size="sm" fullWidth mt="xs" onClick={onClose}>
        キャンセル
      </Button>
    </Paper>
  );
};
