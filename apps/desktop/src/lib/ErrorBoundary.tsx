import { Component, type ErrorInfo, type ReactNode } from "react";

interface Props {
  /** What to show instead: a node, or one made from the error and a `reset` that renders the children again. */
  fallback: ReactNode | ((error: Error, reset: () => void) => ReactNode);
  /** Changing this resets the boundary (e.g. species/activity). */
  resetKey?: string | number;
  /** Names the part in the console line (e.g. "settings:a2a"). */
  label?: string;
  children: ReactNode;
}
interface State {
  error: Error | null;
  key?: string | number;
}

/** Whatever was thrown, as an Error (render code can throw anything). */
export function asError(thrown: unknown): Error {
  if (thrown instanceof Error) return thrown;
  return new Error(typeof thrown === "string" ? thrown : String(thrown));
}

/** Keeps a broken part (a pet renderer, a settings section, a panel view) from taking the whole window down. */
export class ErrorBoundary extends Component<Props, State> {
  state: State = { error: null, key: this.props.resetKey };

  static getDerivedStateFromError(thrown: unknown): Partial<State> {
    return { error: asError(thrown) };
  }

  static getDerivedStateFromProps(props: Props, state: State): Partial<State> | null {
    if (props.resetKey !== state.key) return { error: null, key: props.resetKey };
    return null;
  }

  componentDidCatch(error: Error, info: ErrorInfo): void {
    console.error(`[desktop] render error${this.props.label ? ` in ${this.props.label}` : ""}`, error, info.componentStack);
  }

  reset = (): void => this.setState({ error: null });

  render(): ReactNode {
    const { error } = this.state;
    if (!error) return this.props.children;
    const { fallback } = this.props;
    return typeof fallback === "function" ? fallback(error, this.reset) : fallback;
  }
}
