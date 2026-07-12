import { MantineProvider } from '@mantine/core';
import '@mantine/core/styles.css';
import { GameCanvas } from './components/game/GameCanvas';
import { TurnIndicator } from './components/ui/TurnIndicator';
import { useEffect, useState } from 'react';
import * as Comlink from 'comlink';
import type { EngineWorker } from './worker/engineWorker';

function App() {
  const [engineReady, setEngineReady] = useState(false);
  const [turnInfo, setTurnInfo] = useState({ turn: 1, phase: 'P1' });

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

  return (
    <MantineProvider defaultColorScheme="dark">
      <div style={{ position: 'relative', height: '100vh', width: '100vw', overflow: 'hidden' }}>
        <GameCanvas />
        {engineReady && <TurnIndicator turn={turnInfo.turn} phase={turnInfo.phase} />}
      </div>
    </MantineProvider>
  );
}

export default App;
