import { MantineProvider, Modal, Title, Text, Button } from '@mantine/core';
import '@mantine/core/styles.css';
import { GameCanvas } from './components/game/GameCanvas';
import { TurnIndicator } from './components/ui/TurnIndicator';
import { UnitInfoPanel } from './components/ui/UnitInfoPanel';
import { ActionMenu } from './components/ui/ActionMenu';
import { ProduceMenu } from './components/ui/ProduceMenu';
import { DropMenu } from './components/ui/DropMenu';
import { MainMenu } from './components/ui/MainMenu';
import { useGameStore } from './store/gameStore';

function App() {
  const { 
    appState,
    isEngineReady, 
    turnInfo, 
    hoveredUnit, 
    hoveredTerrain, 
    actionMenu, 
    produceMenu,
    interactionState,
    loadedUnits,
    propertyData,
    closeActionMenu,
    closeProduceMenu,
    cancelInteraction,
    executeAction,
    executeProduce,
    selectDropCargo,
    endTurn,
    gameOver
  } = useGameStore();

  const handleActionSelect = async (action: string) => {
    await executeAction(action);
  };

  const handleProduceSelect = async (unitType: string) => {
    if (produceMenu) {
      await executeProduce(unitType, produceMenu.x, produceMenu.y);
    }
  };

  const getGameOverText = () => {
    if (!gameOver) return '';
    if ('draw' in gameOver && gameOver.draw) {
      return 'ゲームは引き分けで終了しました。';
    }
    if ('winner' in gameOver) {
      const winner = gameOver.winner;
      const loserFaction = winner === 1 ? 'blue' : 'green';
      const loserName = winner === 1 ? 'プレイヤー2 (青軍)' : 'プレイヤー1 (緑軍)';
      
      const loserCapital = propertyData.find(p => p.type === 'capital' && p.owner === loserFaction);
      if (!loserCapital) {
        return `${loserName}の首都が占領されました。`;
      } else {
        return `${loserName}の全部隊が全滅しました。`;
      }
    }
    return '';
  };

  if (appState === 'menu') {
    return (
      <MantineProvider defaultColorScheme="dark">
        <MainMenu />
      </MantineProvider>
    );
  }

  return (
    <MantineProvider defaultColorScheme="dark">
      <div style={{ position: 'relative', height: '100vh', width: '100vw', overflow: 'hidden' }}>
        <GameCanvas />
        
        {isEngineReady && turnInfo && (
          <TurnIndicator 
            turn={turnInfo.turn} 
            phase={turnInfo.phase} 
            funds={turnInfo.funds}
            onEndTurn={endTurn}
            isAiThinking={interactionState === 'ai_thinking'}
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
        {interactionState === 'drop_unit_selection' && (
          <DropMenu
            loadedUnits={loadedUnits}
            onSelect={selectDropCargo}
            onClose={cancelInteraction}
          />
        )}

        <Modal 
          opened={!!gameOver} 
          onClose={() => window.location.reload()} 
          title="ゲーム終了"
          centered
        >
          <Title order={2} ta="center" mb="md">
            {gameOver && 'winner' in gameOver ? `プレイヤー ${gameOver.winner} の勝利！` : '引き分け！'}
          </Title>
          <Text ta="center" mb="xl">
            {getGameOverText()}
          </Text>
          <Button fullWidth onClick={() => window.location.reload()}>
            メインメニューに戻る
          </Button>
        </Modal>
      </div>
    </MantineProvider>
  );
}

export default App;
