import {
  Button,
  Divider,
  FileButton,
  Group,
  Modal,
  Paper,
  Stack,
  Text,
  Title,
} from "@mantine/core";
import { useCallback, useEffect, useState } from "react";
import { useGameStore } from "../../../store/gameStore";

interface SaveLoadModalProps {
  opened: boolean;
  onClose: () => void;
  mode: "save" | "load";
}

interface SlotInfo {
  slotIndex: number;
  hasData: boolean;
  mapName?: string;
  turn?: number;
  activePlayer?: string;
}

export const SaveLoadModal = ({ opened, onClose, mode }: SaveLoadModalProps) => {
  const getSlotStatus = useGameStore((state) => state.getSlotStatus);
  const saveGame = useGameStore((state) => state.saveGame);
  const loadGame = useGameStore((state) => state.loadGame);
  const downloadSaveData = useGameStore((state) => state.downloadSaveData);
  const uploadSaveData = useGameStore((state) => state.uploadSaveData);

  const [slots, setSlots] = useState<SlotInfo[]>([]);
  const [loadingSlot, setLoadingSlot] = useState<number | null>(null);
  const [loadingFile, setLoadingFile] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // モーダルが開かれたらスロット状態をリフレッシュする
  const refreshSlots = useCallback(async () => {
    const status = await getSlotStatus();
    setSlots(status);
  }, [getSlotStatus]);

  useEffect(() => {
    if (opened) {
      setError(null); // モーダルを開いたときはエラーをクリア
      refreshSlots();
    }
  }, [opened, refreshSlots]);

  const handleSlotAction = async (slotIndex: number) => {
    setLoadingSlot(slotIndex);
    setError(null);
    try {
      if (mode === "save") {
        await saveGame(slotIndex);
        await refreshSlots();
      } else {
        await loadGame(slotIndex);
        onClose(); // ロード成功時は閉じる
      }
    } catch (e) {
      console.error(e);
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setLoadingSlot(null);
    }
  };

  const handleFileUpload = async (file: File | null) => {
    if (!file) return;
    setLoadingFile(true);
    setError(null);
    try {
      await uploadSaveData(file);
      onClose(); // ロード成功時は閉じる
    } catch (e) {
      console.error(e);
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setLoadingFile(false);
    }
  };

  return (
    <Modal
      opened={opened}
      onClose={onClose}
      title={<Title order={3}>{mode === "save" ? "ゲームのセーブ" : "ゲームのロード"}</Title>}
      centered
      size="md"
    >
      <Stack gap="md">
        <Text size="sm" c="dimmed">
          {mode === "save"
            ? "保存先のスロットを選択してください。既存データは上書きされます。"
            : "ロード元のスロットを選択してください。"}
        </Text>

        {error && (
          <Paper
            withBorder
            p="xs"
            bg="red.0"
            style={{ borderColor: "var(--mantine-color-red-filled)" }}
          >
            <Text size="xs" c="red" fw={700}>
              {error}
            </Text>
          </Paper>
        )}

        <Stack gap="xs">
          {slots.map((slot) => (
            <Paper key={slot.slotIndex} withBorder p="sm" radius="md">
              <Group justify="space-between">
                <Stack gap={2}>
                  <Text size="sm" fw={700}>
                    スロット {slot.slotIndex}
                  </Text>
                  {slot.hasData ? (
                    <Text size="xs" c="dimmed">
                      {slot.mapName} (ターン: {slot.turn}, プレイヤー: {slot.activePlayer})
                    </Text>
                  ) : (
                    <Text size="xs" c="dimmed">
                      データなし
                    </Text>
                  )}
                </Stack>

                <Button
                  size="xs"
                  variant={mode === "save" ? "filled" : slot.hasData ? "filled" : "outline"}
                  color={mode === "save" ? "blue" : "green"}
                  disabled={mode === "load" && !slot.hasData}
                  loading={loadingSlot === slot.slotIndex}
                  onClick={() => handleSlotAction(slot.slotIndex)}
                >
                  {mode === "save" ? "保存する" : "読込する"}
                </Button>
              </Group>
            </Paper>
          ))}
        </Stack>

        <Divider my="xs" label="ファイルの入出力" labelPosition="center" />

        {mode === "save" ? (
          <Button variant="outline" fullWidth color="blue" onClick={downloadSaveData}>
            セーブデータファイル (.sav) をダウンロード
          </Button>
        ) : (
          <FileButton onChange={handleFileUpload} accept=".sav" disabled={loadingFile}>
            {(props) => (
              <Button {...props} variant="outline" fullWidth color="green" loading={loadingFile}>
                セーブデータファイル (.sav) からロード
              </Button>
            )}
          </FileButton>
        )}
      </Stack>
    </Modal>
  );
};
