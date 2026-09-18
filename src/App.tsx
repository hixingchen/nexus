import { MainLayout } from './components/layout/MainLayout';
import { Toaster } from 'sonner';
import { useAiPanelReserve } from './hooks/useAiPanelReserve';

function App() {
  // 通知让位 AI 面板（原生子 WebView 盖在所有 DOM 之上，见 useAiPanelReserve）
  const panelReserve = useAiPanelReserve();

  return (
    <div className="h-screen flex flex-col bg-nexus-bg text-nexus-text">
      <MainLayout />
      <Toaster
        position="top-center"
        // sonner 容器默认按**窗口**居中（left:50% + translate(-50%)，transform 不能动，
        // 那是它自己的居中/悬浮动画在用）：面板打开时通知右端会落进面板矩形，被原生层
        // 盖住——恰好是关闭按钮的位置。左移半个面板宽度 = 在「内容区」居中；
        // 面板宽度变化时订阅 store 自动跟随
        style={{ left: `calc(50% - ${panelReserve / 2}px)` }}
        toastOptions={{
          // 通知外观由 Toast.tsx 的 toast.custom 完全自控（背景/圆角/边框/阴影/内边距）。
          // unstyled 去掉 sonner 容器自带的装饰：否则容器的不透明背景会在自定义内容
          // 的圆角之外露出（四个角显示为黑块），且自带内边距会与内容内边距叠加
          unstyled: true,
        }}
      />
    </div>
  );
}

export default App;
