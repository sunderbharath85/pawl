// What a part of the panel shows when it crashed (an ErrorBoundary's fallback): a small card with the error and a
// Reload, in place of that part only. The rest of the window keeps working; it is never left blank.
import type { ReactNode } from "react";
import { ErrorBoundary } from "../lib/ErrorBoundary";

export function CrashCard({ where, error, onReload }: { where: string; error: Error; onReload(): void }) {
  return (
    <div className="card crash-card" role="alert">
      <strong>Something went wrong in {where}.</strong>
      <span className="crash-message">{error.message || error.name || "Unknown error"}</span>
      <div className="form-actions">
        <button type="button" className="pill pill-primary" onClick={onReload}>
          Reload
        </button>
      </div>
    </div>
  );
}

/** A panel view in its own boundary: a crash shows the card where the view was; the sidebar keeps working. */
export function ViewGuard({ children }: { children: ReactNode }) {
  return (
    <ErrorBoundary
      label="view"
      fallback={(error, reset) => (
        <section className="view">
          <CrashCard where="this view" error={error} onReload={reset} />
        </section>
      )}
    >
      {children}
    </ErrorBoundary>
  );
}

/** The whole panel, last resort (the sidebar or the banner crashed): Reload loads the window again. */
export function WindowGuard({ children }: { children: ReactNode }) {
  return (
    <ErrorBoundary
      label="panel"
      fallback={(error) => (
        <div className="crash-window">
          <CrashCard where="this window" error={error} onReload={() => window.location.reload()} />
        </div>
      )}
    >
      {children}
    </ErrorBoundary>
  );
}
