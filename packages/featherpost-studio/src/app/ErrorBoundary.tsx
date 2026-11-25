import { m } from '@featherpost/i18n/messages';
import { Component, type ErrorInfo, type ReactNode } from 'react';

export interface ErrorBoundaryProps {
  children: ReactNode;
  resetKey?: unknown;
  onReset?: () => void;
}

interface ErrorBoundaryState {
  failed: boolean;
}

/**
 * 渲染期兜底边界：任一视图或根组件因异常数据抛错时显示可恢复的提示卡片（按当前界面语言成文），
 * 避免整个 WebView 卸载成空白白屏。
 */
export class ErrorBoundary extends Component<ErrorBoundaryProps, ErrorBoundaryState> {
  override state: ErrorBoundaryState = { failed: false };

  static getDerivedStateFromError(): ErrorBoundaryState {
    return { failed: true };
  }

  override componentDidCatch(error: Error, info: ErrorInfo): void {
    console.error('[maild-ui] unexpected render error:', error, info.componentStack);
  }

  override componentDidUpdate(prevProps: ErrorBoundaryProps): void {
    if (this.state.failed && prevProps.resetKey !== this.props.resetKey) {
      this.setState({ failed: false });
    }
  }

  reset = (): void => {
    this.setState({ failed: false });
    this.props.onReset?.();
  };

  override render(): ReactNode {
    if (!this.state.failed) return this.props.children;
    return (
      <div className="page error-boundary" role="alert">
        <div className="empty">
          <h2>{m.common_error_boundary_title()}</h2>
          <p>{m.common_error_boundary_body()}</p>
          <button type="button" className="primary-button" onClick={this.reset}>
            {m.common_error_boundary_reload()}
          </button>
        </div>
      </div>
    );
  }
}
