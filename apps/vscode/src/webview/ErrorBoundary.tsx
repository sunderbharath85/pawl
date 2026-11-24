import { Component, type ErrorInfo, type ReactNode } from "react";

interface Props {
  fallback: ReactNode;
  /** Changing this value retries rendering the children (e.g. new sprites or a reconnect). */
  resetKey?: unknown;
  children: ReactNode;
}

interface State {
  failed: boolean;
}

/** Keeps the rest of the pet view usable if one part (e.g. the pet stage) throws. */
export class ErrorBoundary extends Component<Props, State> {
  state: State = { failed: false };

  static getDerivedStateFromError(): State {
    return { failed: true };
  }

  componentDidCatch(error: unknown, info: ErrorInfo): void {
    console.error("[pawl] render failed", error, info.componentStack);
  }

  componentDidUpdate(prev: Props): void {
    if (this.state.failed && prev.resetKey !== this.props.resetKey) this.setState({ failed: false });
  }

  render(): ReactNode {
    return this.state.failed ? this.props.fallback : this.props.children;
  }
}
