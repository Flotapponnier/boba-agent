import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import { setTransport } from './api/client';
import { App } from './App';
import './styles/base.css';
import './styles/board.css';
import './styles/panels.css';
import './styles/fx.css';

async function boot() {
  if (import.meta.env.VITE_MOCK === '1') {
    const { mockServer } = await import('./mock/server');
    setTransport(mockServer);
    // Handy for demos and e2e scripts: speed up runs, reset data…
    (window as unknown as { bobaMock: typeof mockServer }).bobaMock = mockServer;
  }
  createRoot(document.getElementById('root') as HTMLElement).render(
    <StrictMode>
      <App />
    </StrictMode>,
  );
}

void boot();
