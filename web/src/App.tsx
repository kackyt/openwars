import { Button, MantineProvider, Modal, Text, Title } from "@mantine/core";
import { useState } from "react";
import "@mantine/core/styles.css";
import { appContainer } from "./App.css";
import { ErrorBoundary } from "./components/common/ErrorBoundary";
import { GameCanvas } from "./components/game/GameCanvas";
import { ActionMenu } from "./components/ui/ActionMenu";
import { DropMenu } from "./components/ui/DropMenu";
import { MainMenu } from "./components/ui/MainMenu";
import { ProduceMenu } from "./components/ui/ProduceMenu";
import { SaveLoadModal } from "./components/ui/SaveLoadModal";
import { TurnIndicator } from "./components/ui/TurnIndicator";
import { UnitInfoPanel } from "./components/ui/UnitInfoPanel";
import { FACTION_BLUE, FACTION_GREEN, TERRAIN_CAPITAL } from "./constants/mappings";
import { useGameStore } from "./store/gameStore";

/**
 * アプリケーションのルートコンポーネント
 * メインメニューとゲーム本編、各ポップアップUI（メニュー等）の出し分けや
 * ゲームオーバーモーダルの表示制御を行います。
 */
function App() {
  const [saveModalOpened, setSaveModalOpened] = useState(false);
  const [loadModalOpened, setLoadModalOpened] = useState(false);

  // Zustand のセレクタを用いて必要な状態のみを個別に購読する (再描画の抑制)
  const appState = useGameStore((state) => state.appState);
  const isEngineReady = useGameStore((state) => state.isEngineReady);
  const turnInfo = useGameStore((state) => state.turnInfo);
  const hoveredUnit = useGameStore((state) => state.hoveredUnit);
  const hoveredTerrain = useGameStore((state) => state.hoveredTerrain);
  const actionMenu = useGameStore((state) => state.actionMenu);
  const produceMenu = useGameStore((state) => state.produceMenu);
  const interactionState = useGameStore((state) => state.interactionState);
  const loadedUnits = useGameStore((state) => state.loadedUnits);
  const propertyData = useGameStore((state) => state.propertyData);
  const closeActionMenu = useGameStore((state) => state.closeActionMenu);
  const closeProduceMenu = useGameStore((state) => state.closeProduceMenu);
  const cancelInteraction = useGameStore((state) => state.cancelInteraction);
  const executeAction = useGameStore((state) => state.executeAction);
  const executeProduce = useGameStore((state) => state.executeProduce);
  const selectDropCargo = useGameStore((state) => state.selectDropCargo);
  const endTurn = useGameStore((state) => state.endTurn);
  const gameOver = useGameStore((state) => state.gameOver);

  /** アクションメニュー選択時のハンドラー */
  const handleActionSelect = async (action: string) => {
    await executeAction(action);
  };

  /** 生産メニュー選択時のハンドラー */
  const handleProduceSelect = async (unitType: string) => {
    if (produceMenu) {
      await executeProduce(unitType, produceMenu.gridX, produceMenu.gridY);
    }
  };

  /** ゲームオーバー時の詳細な説明テキストを構築する */
  const getGameOverText = () => {
    if (!gameOver) return "";
    if ("draw" in gameOver && gameOver.draw) {
      return "ゲームは引き分けで終了しました。";
    }
    if ("winner" in gameOver) {
      const winner = gameOver.winner;
      const loserFaction = winner === 1 ? FACTION_BLUE : FACTION_GREEN;
      const loserName = winner === 1 ? "プレイヤー2 (青軍)" : "プレイヤー1 (緑軍)";

      const loserCapital = propertyData.find(
        (p) => p.type === TERRAIN_CAPITAL && p.owner === loserFaction,
      );
      if (!loserCapital) {
        return `${loserName}の首都が占領されました。`;
      }
      return `${loserName}の全部隊が全滅しました。`;
    }
    return "";
  };

  // メインメニュー画面のレンダリング
  if (appState === "menu") {
    return (
      <MantineProvider defaultColorScheme="dark">
        <ErrorBoundary>
          <MainMenu />
        </ErrorBoundary>
      </MantineProvider>
    );
  }

  // Wasm エンジンや Web Worker などの完全なリセットのため、reload() を利用する
  const handleResetGame = () => {
    window.location.reload();
  };

  return (
    <MantineProvider defaultColorScheme="dark">
      <ErrorBoundary>
        <div className={appContainer}>
          <GameCanvas />

          {isEngineReady && turnInfo && (
            <TurnIndicator
              turn={turnInfo.turn}
              phase={turnInfo.phase}
              funds={turnInfo.funds}
              onEndTurn={endTurn}
              onSaveClick={() => setSaveModalOpened(true)}
              onLoadClick={() => setLoadModalOpened(true)}
              isAiThinking={interactionState === "ai_thinking"}
            />
          )}

          <UnitInfoPanel unit={hoveredUnit} terrain={hoveredTerrain} />

          {actionMenu && (
            <ActionMenu
              x={actionMenu.x}
              y={actionMenu.y}
              actions={actionMenu.actions}
              onSelect={handleActionSelect}
              onClose={closeActionMenu}
            />
          )}

          {produceMenu && (
            <ProduceMenu
              x={produceMenu.x}
              y={produceMenu.y}
              units={produceMenu.units}
              currentFunds={turnInfo?.funds || 0}
              onSelect={handleProduceSelect}
              onClose={closeProduceMenu}
            />
          )}

          {/* 降車するユニットの選択メニュー */}
          {interactionState === "drop_unit_selection" && (
            <DropMenu
              loadedUnits={loadedUnits}
              onSelect={selectDropCargo}
              onClose={cancelInteraction}
            />
          )}

          <Modal opened={!!gameOver} onClose={handleResetGame} title="ゲーム終了" centered>
            <Title order={2} ta="center" mb="md">
              {gameOver && "winner" in gameOver
                ? `プレイヤー ${gameOver.winner} の勝利！`
                : "引き分け！"}
            </Title>
            <Text ta="center" mb="xl">
              {getGameOverText()}
            </Text>
            <Button fullWidth onClick={handleResetGame}>
              メインメニューに戻る
            </Button>
          </Modal>

          <SaveLoadModal
            opened={saveModalOpened}
            onClose={() => setSaveModalOpened(false)}
            mode="save"
          />
          <SaveLoadModal
            opened={loadModalOpened}
            onClose={() => setLoadModalOpened(false)}
            mode="load"
          />
        </div>
      </ErrorBoundary>
    </MantineProvider>
  );
}

export default App;
