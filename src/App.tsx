import { MainLayout } from './components/layout/MainLayout';
import { Toaster } from 'sonner';

function App() {
  return (
    <div className="h-screen flex flex-col bg-nexus-bg text-nexus-text">
      <MainLayout />
      <Toaster
        position="top-center"
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
