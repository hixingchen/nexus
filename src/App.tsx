import { MainLayout } from './components/layout/MainLayout';
import { Toaster } from 'sonner';

function App() {
  return (
    <div className="h-screen flex flex-col bg-nexus-bg text-nexus-text">
      <MainLayout />
      <Toaster
        position="top-center"
        toastOptions={{
          style: {
            background: '#252536',
            color: '#cdd6f4',
            border: '1px solid #363647',
          },
        }}
      />
    </div>
  );
}

export default App;
