import { Component, type ErrorInfo, type ReactNode } from "react";

import { Button } from "@/components/ui/button";

type Props = { children: ReactNode };
type State = { error: Error | null };

/** Catches an error while drawing, so one broken screen never leaves a blank window. */
export class ErrorBoundary extends Component<Props, State> {
  override state: State = { error: null };

  static getDerivedStateFromError(error: Error): State {
    return { error };
  }

  override componentDidCatch(error: Error, info: ErrorInfo) {
    console.error("A screen could not be drawn.", error, info.componentStack);
  }

  override render() {
    const { error } = this.state;
    if (!error) return this.props.children;
    return (
      <div role="alert" className="grid h-full place-items-center bg-app-bg p-8">
        <div className="max-w-[68ch] space-y-3">
          <h1 className="text-[15px] font-semibold text-app-text">This screen could not be shown.</h1>
          <p className="text-app-text-2">
            Your projects are safe: they are stored by the core, not by this window.
          </p>
          <pre className="app-scroll overflow-auto rounded-control bg-app-subtle p-3 font-mono text-[12px] text-app-text-2">
            {error.message}
          </pre>
          <Button
            onClick={() => {
              this.setState({ error: null });
            }}
          >
            Try again
          </Button>
        </div>
      </div>
    );
  }
}
