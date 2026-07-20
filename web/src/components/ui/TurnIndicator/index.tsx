import { Button, ColorSwatch, Group, Paper, Stack, Text } from "@mantine/core";
import { PHASE_P1, PHASE_P2 } from "../../../constants/mappings";
import { COLOR_SWATCH_SIZE } from "../../../constants/rendering";
import { turnIndicatorContainer } from "./index.css";

interface TurnIndicatorProps {
  turn: number;
  phase: string;
  funds: number;
  onEndTurn: () => void;
  onSaveClick: () => void;
  onLoadClick: () => void;
  isAiThinking: boolean;
}

/**
 * ターンインジケーターコンポーネント
 * 現在のターン数、フェーズ（プレイヤー）、資金量を表示し、ターン終了操作を行うためのUIです。
 * カーソルが画面右上エリアにホバーした場合は左上に動的退避し、マップ遮蔽を防ぎます。
 */
export const TurnIndicator = ({
  turn,
  phase,
  funds,
  onEndTurn,
  onSaveClick,
  onLoadClick,
  isAiThinking,
}: TurnIndicatorProps) => {
  const isP1 = phase === PHASE_P1;
  const isP2 = phase === PHASE_P2;

  const phaseText = isAiThinking
    ? "AI思考中..."
    : isP1
      ? "緑軍のターン"
      : isP2
        ? "青軍のターン"
        : phase;
  const phaseColor = isP1 ? "green" : isP2 ? "blue" : "gray";

  return (
    <Paper
      shadow="md"
      p="sm"
      radius="md"
      withBorder
      className={turnIndicatorContainer}
      style={{ borderLeft: `6px solid var(--mantine-color-${phaseColor}-filled)` }}
    >
      <Group justify="space-between">
        <Stack gap={0}>
          <Text size="lg" fw={800}>
            Turn {turn}
          </Text>
          <Text size="sm" fw={700} c="yellow">
            資金: {funds} G
          </Text>
        </Stack>

        <Stack gap="xs" align="flex-end">
          <Group gap="xs">
            <ColorSwatch
              color={`var(--mantine-color-${phaseColor}-filled)`}
              size={COLOR_SWATCH_SIZE}
            />
            <Text size="sm" fw={700} c={phaseColor}>
              {phaseText}
            </Text>
          </Group>
          <Group gap="xs">
            <Button
              size="xs"
              color="blue"
              variant="subtle"
              onClick={onSaveClick}
              disabled={isAiThinking}
            >
              セーブ
            </Button>
            <Button
              size="xs"
              color="green"
              variant="subtle"
              onClick={onLoadClick}
              disabled={isAiThinking}
            >
              ロード
            </Button>
            <Button
              size="xs"
              color="red"
              variant="light"
              onClick={onEndTurn}
              disabled={isAiThinking}
            >
              End Turn
            </Button>
          </Group>
        </Stack>
      </Group>
    </Paper>
  );
};
