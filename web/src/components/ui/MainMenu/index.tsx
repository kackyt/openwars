import { Button, Container, Group, Paper, Select, Stack, Title } from "@mantine/core";
import { useState } from "react";
import { MAIN_MENU_MARGIN_TOP } from "../../../constants/rendering";
import { useGameStore } from "../../../store/gameStore";

/**
 * メインメニューコンポーネント
 * ゲーム起動時の初期設定（マップ、グリッド形式、プレイヤー/AI設定）を行い、ゲームを開始する画面です。
 */
export const MainMenu = () => {
  // Zustand のセレクタを用いて initEngine のみを取得する
  const initEngine = useGameStore((state) => state.initEngine);

  const [mapName, setMapName] = useState("map_1");
  const [topology, setTopology] = useState("square");
  const [p1Type, setP1Type] = useState("player");
  const [p2Type, setP2Type] = useState("ai");
  const [isLoading, setIsLoading] = useState(false);

  /** ゲームを開始する */
  const handleStart = async () => {
    setIsLoading(true);
    await initEngine(mapName, topology, p1Type === "ai", p2Type === "ai");
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
              data={[
                { value: "player", label: "Human" },
                { value: "ai", label: "AI" },
              ]}
              value={p1Type}
              onChange={(val) => setP1Type(val || "player")}
            />
            <Select
              label="Player 2 (Blue)"
              data={[
                { value: "player", label: "Human" },
                { value: "ai", label: "AI" },
              ]}
              value={p2Type}
              onChange={(val) => setP2Type(val || "ai")}
            />
          </Group>

          <Button size="lg" mt="md" onClick={handleStart} loading={isLoading}>
            Start Game
          </Button>
        </Stack>
      </Paper>
    </Container>
  );
};
