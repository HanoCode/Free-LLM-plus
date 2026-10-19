import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import App from './App';
import { initDesktop } from './lib/desktop';
import './styles/tokens.css';
import './styles/app.css';

initDesktop();

createRoot(document.getElementById('root')!).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
