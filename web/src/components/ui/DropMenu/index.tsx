import { Button, Paper, Stack, Text } from "@mantine/core";
import { UNIT_MAP } from "../../../constants/mappings";
import { dropMenuContainer } from "./index.css";

interface DropMenuProps {
  loadedUnits: { id: string; type: string }[];
  onSelect: (cargoId: string) => void;
  onClose: () => void;
}

/**
 * 降車ユニット選択メニューコンポーネント
 * 輸送ユニットに積載されているユニットから、降車させるユニットを選択するモーダル風UIです。
 */
export const DropMenu = ({ loadedUnits, onSelect, onClose }: DropMenuProps) => {
  if (loadedUnits.length === 0) return null;

  return (
    <Paper shadow="xl" p="sm" withBorder className={dropMenuContainer}>
      <Stack gap="xs">
        <Text size="sm" fw={700} ta="center">
          降車するユニットを選択
        </Text>
        {loadedUnits.map((unit) => (
          <Button
            key={unit.id}
            variant="light"
            size="sm"
            fullWidth
            onClick={() => onSelect(unit.id)}
          >
            {UNIT_MAP[unit.type] || unit.type}
          </Button>
        ))}
        <Button variant="subtle" color="gray" size="sm" fullWidth onClick={onClose}>
          キャンセル
        </Button>
      </Stack>
    </Paper>
  );
};
