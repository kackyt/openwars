import { Paper, Stack, Button } from '@mantine/core';
import { actionMenuContainer } from './index.css';

interface ActionMenuProps {
  x: number;
  y: number;
  actions: string[];
  onSelect: (action: string) => void;
  onClose: () => void;
}

export const ActionMenu = ({ x, y, actions, onSelect, onClose }: ActionMenuProps) => {
  if (actions.length === 0) return null;

  return (
    <Paper 
      shadow="xl" 
      p="xs" 
      withBorder 
      className={actionMenuContainer}
      style={{ left: `${x}px`, top: `${y}px` }}
    >
      <Stack gap="xs">
        {actions.map((action) => (
          <Button key={action} variant="light" size="sm" fullWidth onClick={() => onSelect(action)}>
            {action.toUpperCase()}
          </Button>
        ))}
        <Button variant="subtle" color="gray" size="sm" fullWidth onClick={onClose}>
          Cancel
        </Button>
      </Stack>
    </Paper>
  );
};
