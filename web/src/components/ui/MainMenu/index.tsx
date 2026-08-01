import { Button, Container, Group, Paper, Select, Stack, Title } from "@mantine/core";
import { useState } from "react";
import { MAIN_MENU_MARGIN_TOP } from "../../../constants/rendering";
import { useGameStore } from "../../../store/gameStore";
import {
  applyPlayerMenuValue,
  PLAYER_MENU_OPTIONS,
  type PlayerId,
  type PlayerMenuValue,
  toPlayerMenuValue,
} from "../../../types/player";
import { SaveLoadModal } from "../SaveLoadModal";

/**
 * メインメニューコンポーネント
 * ゲーム起動時の初期設定（マップ、グリッド形式、プレイヤー/AI設定）を行い、ゲームを開始する画面です。
 */
export const MainMenu = () => {
  // Zustand のセレクタを用いて initEngine のみを取得する
  const initEngine = useGameStore((state) => state.initEngine);
  const playerSettings = useGameStore((state) => state.playerSettings);
  const setPlayerSettings = useGameStore((state) => state.setPlayerSettings);

  const [mapName, setMapName] = useState("map_1");
  const [topology, setTopology] = useState("square");
  const [isLoading, setIsLoading] = useState(false);
  const [loadOpened, setLoadOpened] = useState(false);

  const updatePlayerSelection = (playerId: PlayerId, value: string | null) => {
    const menuValue = (value || "Human") as PlayerMenuValue;
    setPlayerSettings({
      ...playerSettings,
      [playerId]: applyPlayerMenuValue(playerSettings[playerId], menuValue),
    });
  };

  /** ゲームを開始する */
  const handleStart = async () => {
    setIsLoading(true);
    await initEngine(mapName, topology, playerSettings);
    setIsLoading(false);
  };

  return (
    <Container size="sm" mt={MAIN_MENU_MARGIN_TOP}>
      <Paper shadow="md" p="xl" radius="md" withBorder>
        <Stack gap="lg">
          <Title order={1} ta="center">
            OpenWars Web
          </Title>

          <Select
            label="Map"
            data={["map_1", "map_2", "map_3"]}
            value={mapName}
            onChange={(val) => setMapName(val || "map_1")}
          />

          <Select
            label="Grid Topology"
            data={[
              { value: "square", label: "Square (四角形)" },
              { value: "hex", label: "Hex (六角形)" },
            ]}
            value={topology}
            onChange={(val) => setTopology(val || "square")}
          />

          <Group grow>
            <Select
              label="Player 1 (Green)"
              data={[...PLAYER_MENU_OPTIONS]}
              value={toPlayerMenuValue(playerSettings[1])}
              onChange={(value) => updatePlayerSelection(1, value)}
              allowDeselect={false}
            />
            <Select
              label="Player 2 (Blue)"
              data={[...PLAYER_MENU_OPTIONS]}
              value={toPlayerMenuValue(playerSettings[2])}
              onChange={(value) => updatePlayerSelection(2, value)}
              allowDeselect={false}
            />
          </Group>

          <Button size="lg" mt="md" onClick={handleStart} loading={isLoading}>
            Start Game
          </Button>

          <Button size="lg" variant="outline" color="green" onClick={() => setLoadOpened(true)}>
            Load Game
          </Button>

          <SaveLoadModal opened={loadOpened} onClose={() => setLoadOpened(false)} mode="load" />
        </Stack>
      </Paper>
    </Container>
  );
};
