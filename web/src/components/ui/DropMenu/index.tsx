import { Button, Paper, Stack, Text } from "@mantine/core";

const UNIT_NAME_MAP: Record<string, string> = {
  infantry: "軽歩兵",
  mech: "重歩兵",
  recon: "装甲車",
  tank: "軽戦車",
  mdtank: "中戦車",
  tankz: "重戦車",
  artillery: "砲台",
  lightspgun: "軽自走砲",
  heavyspgun: "重自走砲",
  rockets: "ロケットランチャー",
  antiair: "対空戦車",
  missiles: "対空ミサイル",
  fighter: "軽戦闘機",
  heavyfighter: "重戦闘機",
  bomber: "爆撃機",
  bcopters: "戦闘ヘリ",
  transporthelicopter: "輸送ヘリ",
  battleship: "戦艦",
  carrier: "空母",
  lander: "輸送船",
  supplytruck: "補給輸送車",
};

interface DropMenuProps {
  loadedUnits: { id: string; type: string }[];
  onSelect: (cargoId: string) => void;
  onClose: () => void;
}

export const DropMenu = ({ loadedUnits, onSelect, onClose }: DropMenuProps) => {
  if (loadedUnits.length === 0) return null;

  return (
    <Paper
      shadow="xl"
      p="sm"
      withBorder
      style={{
        position: "absolute",
        top: "50%",
        left: "50%",
        transform: "translate(-50%, -50%)",
        zIndex: 100,
        minWidth: 200,
      }}
    >
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
            {UNIT_NAME_MAP[unit.type] || unit.type}
          </Button>
        ))}
        <Button variant="subtle" color="gray" size="sm" fullWidth onClick={onClose}>
          キャンセル
        </Button>
      </Stack>
    </Paper>
  );
};
