import { MantineProvider } from '@mantine/core';
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
    closeActionMenu,
    closeProduceMenu,
    cancelInteraction,
    executeAction,
    executeProduce,
    selectDropCargo,
    endTurn
  } = useGameStore();

  const handleActionSelect = async (action: string) => {
    await executeAction(action);
  };

  const handleProduceSelect = async (unitType: string) => {
    if (produceMenu) {
      await executeProduce(unitType, produceMenu.x, produceMenu.y);
    }
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
      </div>
    </MantineProvider>
  );
}

export default App;
