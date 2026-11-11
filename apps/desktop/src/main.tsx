import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';

import { App } from './App';
import { createEngineHost } from './engineHost';
import './styles.css';

const container = document.getElementById('root');
if (!container) {
  throw new Error('缺少 #root 容器');
}
// 在桌面外壳里接上引擎；在浏览器里是 null，界面用模拟文档
const host = createEngineHost();
// 外壳带 WebGL 画布和 dockview 拖放，StrictMode 下的双挂载会让画布控制器创建两次；
// 开发模式仍保留 StrictMode，便于发现副作用问题，控制器在卸载时完整释放。
createRoot(container).render(<StrictMode>{host ? <App host={host} /> : <App />}</StrictMode>);
