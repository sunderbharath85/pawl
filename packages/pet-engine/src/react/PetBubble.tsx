import { useEffect, useRef, useState, type JSX, type KeyboardEvent, type MouseEvent } from "react";
import type { PetView } from "../views";
import { usePetStyles } from "./hooks";

export interface PetBubbleProps {
  view: PetView;
  /** Approve the pending permission (only shown for permission bubbles). May return a promise. */
  onApprove?: (() => void | Promise<unknown>) | undefined;
  onDeny?: (() => void | Promise<unknown>) | undefined;
  onClick?: (() => void) | undefined;
  className?: string | undefined;
  approveLabel?: string | undefined;
  denyLabel?: string | undefined;
}

function isPromise(v: unknown): v is Promise<unknown> {
  return typeof v === "object" && v !== null && typeof (v as { then?: unknown }).then === "function";
}

/**
 * Speech bubble for a pet; permission bubbles get Approve/Deny buttons unless `readOnly` (a visitor's
 * request, answered on its own machine). Null when the view has no bubble.
 */
export function PetBubble(props: PetBubbleProps): JSX.Element | null {
  const { view, onApprove, onDeny, onClick, className, approveLabel = "Approve", denyLabel = "Deny" } = props;
  usePetStyles();
  const bubble = view.bubble;
  const [busy, setBusy] = useState<"approve" | "deny" | null>(null);
  const mounted = useRef(true);
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);
  // A new request (or a resolved one) resets the busy state.
  useEffect(() => setBusy(null), [bubble?.requestId, bubble?.kind]);
  // Safety net: never leave the buttons disabled if the hub never resolves the request.
  useEffect(() => {
    if (!busy) return;
    const t = setTimeout(() => setBusy(null), 15_000);
    return () => clearTimeout(t);
  }, [busy]);

  if (!bubble) return null;
  const isPermission = bubble.kind === "permission";
  const actionable = isPermission && !bubble.readOnly;

  const run = (which: "approve" | "deny", fn?: () => void | Promise<unknown>) => (e: MouseEvent<HTMLButtonElement>) => {
    e.stopPropagation();
    if (!fn || busy) return;
    setBusy(which);
    let result: unknown;
    try {
      result = fn();
    } catch {
      setBusy(null);
      return;
    }
    if (isPromise(result)) {
      result.then(
        () => undefined,
        () => {
          if (mounted.current) setBusy(null);
        },
      );
    }
  };

  const onKey = (e: KeyboardEvent<HTMLDivElement>) => {
    if (!onClick || e.target !== e.currentTarget) return;
    if (e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      onClick();
    }
  };

  const cls = ["ap-bubble", `ap-bubble-${bubble.kind}`, bubble.readOnly ? "ap-bubble-readonly" : "", className ?? ""].filter(Boolean).join(" ");
  return (
    <div
      className={cls}
      role={actionable ? "alertdialog" : "status"}
      aria-live={actionable ? "assertive" : "polite"}
      aria-label={actionable ? `${view.name} needs permission` : isPermission ? `${view.name} needs approval on another machine` : undefined}
      data-clickable={onClick ? "true" : "false"}
      data-pet-hit="bubble"
      data-pet-id={view.petId}
      tabIndex={onClick ? 0 : undefined}
      onClick={onClick ? () => onClick() : undefined}
      onKeyDown={onClick ? onKey : undefined}
    >
      <div className="ap-bubble-text">{bubble.text}</div>
      {actionable && (onApprove || onDeny) ? (
        <div className="ap-bubble-actions">
          {onApprove ? (
            <button type="button" className="ap-btn ap-btn-approve" disabled={busy !== null} onClick={run("approve", onApprove)}>
              {busy === "approve" ? "…" : approveLabel}
            </button>
          ) : null}
          {onDeny ? (
            <button type="button" className="ap-btn ap-btn-deny" disabled={busy !== null} onClick={run("deny", onDeny)}>
              {busy === "deny" ? "…" : denyLabel}
            </button>
          ) : null}
        </div>
      ) : null}
    </div>
  );
}
