import { MantineProvider } from '@mantine/core';
import '@mantine/core/styles.css';
import { GameCanvas } from './components/game/GameCanvas';
import { TurnIndicator } from './components/ui/TurnIndicator';
import { UnitInfoPanel } from './components/ui/UnitInfoPanel';
import { ActionMenu } from './components/ui/ActionMenu';
import { useEffect } from 'react';
import { useGameStore } from './store/gameStore';

function App() {
  const { 
    isEngineReady, 
    turnInfo, 
    hoveredUnit, 
    hoveredTerrain, 
    actionMenu, 
    initEngine,
    closeActionMenu
  } = useGameStore();

  useEffect(() => {
    initEngine();
  }, [initEngine]);

  const handleActionSelect = (action: string) => {
    console.log(`Action selected: ${action}`);
    closeActionMenu();
  };

  return (
    <MantineProvider defaultColorScheme="dark">
      <div style={{ position: 'relative', height: '100vh', width: '100vw', overflow: 'hidden' }}>
        <GameCanvas />
        
        {isEngineReady && turnInfo && <TurnIndicator turn={turnInfo.turn} phase={turnInfo.phase} />}
        
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
      </div>
    </MantineProvider>
  );
}

export default App;
