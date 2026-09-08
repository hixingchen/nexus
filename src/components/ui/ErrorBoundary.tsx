import { Component, type ReactNode } from 'react';

interface ErrorBoundaryProps {
  /** 子树渲染抛错时的兜底 UI（默认通用提示） */
  fallback?: ReactNode;
  children: ReactNode;
}

interface ErrorBoundaryState {
  hasError: boolean;
}

/**
 * 渲染错误边界：子树在渲染/生命周期中抛错时展示兜底 UI 而非整个应用白屏。
 * 关键路径必须包裹：CodeMirror 等复杂编辑器，任何扩展/装饰异常都不该击穿整个应用。
 * 消费方可用 key 变化（切文件/切面板）自动重置边界
 */
export class ErrorBoundary extends Component<ErrorBoundaryProps, ErrorBoundaryState> {
  state: ErrorBoundaryState = { hasError: false };

  static getDerivedStateFromError(): ErrorBoundaryState {
    return { hasError: true };
  }

  componentDidCatch(error: unknown, errorInfo: unknown) {
    // 详细原因进控制台（fallback UI 只给用户可读提示）
    console.error('[ErrorBoundary] 渲染失败:', error, errorInfo);
  }

  render() {
    if (this.state.hasError) {
      return this.props.fallback ?? (
        <div className="flex items-center justify-center h-full p-4 text-center text-[12px] text-nexus-error">
          该文件渲染失败，请重新打开或查看其他内容（原因见控制台）
        </div>
      );
    }
    return this.props.children;
  }
}
