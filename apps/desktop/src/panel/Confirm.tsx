// In-app confirm dialog (never window.confirm): risky settings changes say exactly what they allow, and the
// user picks between a safe "Cancel" (focused first) and the named go-ahead button.
import { createContext, useCallback, useContext, useEffect, useMemo, useRef, useState, type KeyboardEvent, type ReactNode } from "react";
import type { RiskyChange } from "@pawl/hub-client";

export interface ConfirmOptions {
  title: string;
  body?: ReactNode;
  /** Several changes in one go (each its own heading + text). */
  items?: Array<{ title: string; body: ReactNode }>;
  confirmLabel: string;
  cancelLabel?: string;
  tone?: "danger" | "primary";
}

export interface ConfirmApi {
  /** Resolves true only when the user clicked the go-ahead button. */
  ask(opts: ConfirmOptions): Promise<boolean>;
  /** One dialog for every risky change of a save (true = go ahead with all of them). */
  risky(changes: readonly RiskyChange[]): Promise<boolean>;
}

const ConfirmContext = createContext<ConfirmApi | null>(null);

export function useConfirm(): ConfirmApi {
  const api = useContext(ConfirmContext);
  if (!api) throw new Error("useConfirm outside <ConfirmProvider>");
  return api;
}

/** `code` spans for text in backticks (commands in the confirm text). */
export function withCode(text: string): ReactNode {
  const parts = text.split(/`([^`]+)`/g);
  if (parts.length === 1) return text;
  return parts.map((p, i) => (i % 2 === 1 ? <code key={i} className="confirm-code">{p}</code> : p));
}

interface Pending extends ConfirmOptions {
  id: number;
  resolve(ok: boolean): void;
}

export function ConfirmProvider({ children }: { children: ReactNode }) {
  const [queue, setQueue] = useState<Pending[]>([]);
  const seq = useRef(0);

  const ask = useCallback(
    (opts: ConfirmOptions) => new Promise<boolean>((resolve) => setQueue((q) => [...q, { ...opts, id: ++seq.current, resolve }])),
    [],
  );
  const risky = useCallback(
    (changes: readonly RiskyChange[]) => {
      if (changes.length === 0) return Promise.resolve(true);
      if (changes.length === 1) {
        const c = changes[0]!;
        return ask({ title: c.title, body: withCode(c.body), confirmLabel: c.confirm, tone: "primary" });
      }
      return ask({
        title: "Save these changes?",
        items: changes.map((c) => ({ title: c.title, body: withCode(c.body) })),
        confirmLabel: "Allow and save",
        tone: "primary",
      });
    },
    [ask],
  );
  const api = useMemo(() => ({ ask, risky }), [ask, risky]);
  const current = queue[0];

  const close = (ok: boolean) => {
    if (!current) return;
    current.resolve(ok);
    setQueue((q) => q.slice(1));
  };

  return (
    <ConfirmContext.Provider value={api}>
      {children}
      {current && <ConfirmDialog key={current.id} opts={current} onClose={close} />}
    </ConfirmContext.Provider>
  );
}

function ConfirmDialog({ opts, onClose }: { opts: ConfirmOptions; onClose(ok: boolean): void }) {
  const ref = useRef<HTMLDivElement>(null);
  const cancelRef = useRef<HTMLButtonElement>(null);

  useEffect(() => {
    const before = document.activeElement as HTMLElement | null;
    cancelRef.current?.focus();
    return () => before?.focus?.();
  }, []);

  const onKeyDown = (e: KeyboardEvent) => {
    if (e.key === "Escape") {
      e.preventDefault();
      onClose(false);
      return;
    }
    if (e.key !== "Tab" || !ref.current) return;
    // Keep focus inside the dialog.
    const focusable = [...ref.current.querySelectorAll<HTMLElement>("button, [href], input, [tabindex]:not([tabindex='-1'])")].filter(
      (el) => !el.hasAttribute("disabled"),
    );
    if (focusable.length === 0) return;
    const first = focusable[0]!;
    const last = focusable[focusable.length - 1]!;
    if (e.shiftKey && document.activeElement === first) {
      e.preventDefault();
      last.focus();
    } else if (!e.shiftKey && document.activeElement === last) {
      e.preventDefault();
      first.focus();
    }
  };

  return (
    <div className="confirm-backdrop" onMouseDown={(e) => e.target === e.currentTarget && onClose(false)}>
      <div
        ref={ref}
        className="confirm-dialog"
        role="alertdialog"
        aria-modal="true"
        aria-labelledby="confirm-title"
        aria-describedby="confirm-body"
        onKeyDown={onKeyDown}
      >
        <h2 id="confirm-title">{opts.title}</h2>
        <div id="confirm-body" className="confirm-body">
          {opts.body && <p>{opts.body}</p>}
          {opts.items?.map((item, i) => (
            <div key={i} className="confirm-item">
              <strong>{item.title}</strong>
              <p>{item.body}</p>
            </div>
          ))}
        </div>
        <div className="confirm-actions">
          <button ref={cancelRef} className="pill" onClick={() => onClose(false)}>
            {opts.cancelLabel ?? "Cancel"}
          </button>
          <button className={`pill ${opts.tone === "danger" ? "pill-bad" : "pill-primary"}`} onClick={() => onClose(true)}>
            {opts.confirmLabel}
          </button>
        </div>
      </div>
    </div>
  );
}
