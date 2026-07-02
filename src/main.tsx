import React from 'react';
import ReactDOM from 'react-dom/client';
import App from './App';
import './index.css';

// 全局禁止右键菜单
document.addEventListener('contextmenu', (e) => e.preventDefault());

ReactDOM.createRoot(document.getElementById('root')!).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
