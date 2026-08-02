import { Component, type ReactNode } from "react";

interface Props {
  children: ReactNode;
}

interface State {
  error: Error | null;
}

/** A render throw in a desktop webview has no reload button and no devtools —
 * without this boundary the user gets a silent white screen. */
export class ErrorBoundary extends Component<Props, State> {
  state: State = { error: null };

  static getDerivedStateFromError(error: Error): State {
    return { error };
  }

  render() {
    if (this.state.error) {
      return (
        <main className="panel">
          <div className="error-banner" role="alert">
            <span>MCPanel hit an unexpected error: {this.state.error.message}</span>
            <button type="button" onClick={() => this.setState({ error: null })}>
              try again
            </button>
          </div>
        </main>
      );
    }
    return this.props.children;
  }
}
