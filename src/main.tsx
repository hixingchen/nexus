import React from 'react';
import ReactDOM from 'react-dom/client';
import App from './App';
import './index.css';

// 全局禁止右键菜单
document.addEventListener('contextmenu', (e) => e.preventDefault());

const rootEl = document.getElementById('root');
if (!rootEl) throw new Error('找不到 #root 挂载点（index.html 被修改？）');

ReactDOM.createRoot(rootEl).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
