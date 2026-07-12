import { MantineProvider } from '@mantine/core';
import '@mantine/core/styles.css';
import { GameCanvas } from './components/game/GameCanvas';
import { TurnIndicator } from './components/ui/TurnIndicator';
import { UnitInfoPanel } from './components/ui/UnitInfoPanel';
import { ActionMenu } from './components/ui/ActionMenu';
import { useEffect, useState } from 'react';
import * as Comlink from 'comlink';
import type { EngineWorker } from './worker/engineWorker';

function App() {
  const [engineReady, setEngineReady] = useState(false);
  const [turnInfo, setTurnInfo] = useState({ turn: 1, phase: 'P1' });
  const [selectedUnit, setSelectedUnit] = useState<{id: string, type: string, faction: string, hp?: number} | null>(null);
  const [selectedTerrain, setSelectedTerrain] = useState<{type: string, def?: number} | null>(null);
  const [actionMenu, setActionMenu] = useState<{x: number, y: number, actions: string[]} | null>(null);

  useEffect(() => {
    async function initWorker() {
      const worker = new Worker(new URL('./worker/engineWorker.ts', import.meta.url), {
        type: 'module',
      });
      const engine = Comlink.wrap<typeof EngineWorker>(worker);
      
      const engineInstance = await new engine();
      await engineInstance.initWasm();
      
      setEngineReady(true);
      
      const infoStr = await engineInstance.getTurnInfo();
      const info = JSON.parse(infoStr as string);
      setTurnInfo(info);
    }
    
    initWorker();
  }, []);

  const handleCellClick = (_x: number, _y: number, cellType: number, unitInfo: any | null, clientX: number, clientY: number) => {
    const terrains = ['plain', 'woods', 'mountain'];
    const defs = [1, 2, 3];
    setSelectedTerrain({ type: terrains[cellType] || 'unknown', def: defs[cellType] || 0 });

    if (unitInfo) {
      setSelectedUnit(unitInfo);
      setActionMenu({
        x: clientX,
        y: clientY,
        actions: ['Wait', 'Attack', 'Capture']
      });
    } else {
      setSelectedUnit(null);
      setActionMenu(null);
    }
  };

  const handleActionSelect = (action: string) => {
    console.log(`Action selected: ${action}`);
    setActionMenu(null);
  };

  return (
    <MantineProvider defaultColorScheme="dark">
      <div style={{ position: 'relative', height: '100vh', width: '100vw', overflow: 'hidden' }}>
        <GameCanvas onCellClick={handleCellClick} />
        
        {engineReady && <TurnIndicator turn={turnInfo.turn} phase={turnInfo.phase} />}
        
        <UnitInfoPanel unit={selectedUnit} terrain={selectedTerrain} />
        
        {actionMenu && (
          <ActionMenu 
            x={actionMenu.x} 
            y={actionMenu.y} 
            actions={actionMenu.actions} 
            onSelect={handleActionSelect} 
            onClose={() => setActionMenu(null)} 
          />
        )}
      </div>
    </MantineProvider>
  );
}

export default App;
