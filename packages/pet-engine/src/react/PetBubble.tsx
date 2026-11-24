import { useEffect, useRef, useState, type JSX, type KeyboardEvent, type MouseEvent } from "react";
import { approvalConfirmText, type PetView } from "../views";
import { usePetStyles } from "./hooks";

export interface PetBubbleProps {
  view: PetView;
  /**
   * Approve the pending permission (only shown for permission bubbles). May return a promise. `confirmed` is
   * true once the user passed the bubble's confirm step (another machine's request, `bubble.confirm`).
   */
  onApprove?: ((opts: { confirmed: boolean }) => void | Promise<unknown>) | undefined;
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
 * Speech bubble for a pet; permission bubbles get Approve/Deny buttons unless `readOnly`. When the bubble
 * carries `confirm` (another machine's request), Approve first turns the bubble into a confirm step
 * ("Approve on Mac mini: Run npm test?") and only that step approves. Null when the view has no bubble.
 */
export function PetBubble(props: PetBubbleProps): JSX.Element | null {
  const { view, onApprove, onDeny, onClick, className, approveLabel = "Approve", denyLabel = "Deny" } = props;
  usePetStyles();
  const bubble = view.bubble;
  const [busy, setBusy] = useState<"approve" | "deny" | null>(null);
  const [confirming, setConfirming] = useState(false);
  const mounted = useRef(true);
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);
  // A new request (or a resolved one) resets the busy state and any confirm step.
  useEffect(() => {
    setBusy(null);
    setConfirming(false);
  }, [bubble?.requestId, bubble?.kind]);
  // Safety net: never leave the buttons disabled if the hub never resolves the request.
  useEffect(() => {
    if (!busy) return;
    const t = setTimeout(() => setBusy(null), 15_000);
    return () => clearTimeout(t);
  }, [busy]);

  if (!bubble) return null;
  const isPermission = bubble.kind === "permission";
  const actionable = isPermission && !bubble.readOnly;
  const confirmText = approvalConfirmText(bubble);

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
    if (confirming && e.key === "Escape") {
      e.preventDefault();
      setConfirming(false);
      return;
    }
    if (!onClick || e.target !== e.currentTarget) return;
    if (e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      onClick();
    }
  };

  const approve = onApprove;
  const cls = [
    "ap-bubble",
    `ap-bubble-${bubble.kind}`,
    bubble.readOnly ? "ap-bubble-readonly" : "",
    bubble.confirm ? "ap-bubble-remote" : "",
    confirming ? "ap-bubble-confirming" : "",
    className ?? "",
  ]
    .filter(Boolean)
    .join(" ");
  return (
    <div
      className={cls}
      role={actionable ? "alertdialog" : "status"}
      aria-live={actionable ? "assertive" : "polite"}
      aria-label={
        actionable
          ? bubble.confirm
            ? `${view.name} needs permission on ${bubble.confirm.machineName}`
            : `${view.name} needs permission`
          : isPermission
            ? `${view.name} needs approval on another machine`
            : undefined
      }
      data-clickable={onClick ? "true" : "false"}
      data-pet-hit="bubble"
      data-pet-id={view.petId}
      tabIndex={onClick || confirming ? 0 : undefined}
      onClick={onClick && !confirming ? () => onClick() : undefined}
      onKeyDown={onClick || confirming ? onKey : undefined}
    >
      {confirming && bubble.confirm ? (
        <div className="ap-bubble-text">
          Approve on <strong className="ap-bubble-machine">{bubble.confirm.machineName}</strong>: {bubble.confirm.action}?
        </div>
      ) : (
        <div className="ap-bubble-text">{bubble.text}</div>
      )}
      {actionable && confirming && approve ? (
        <div className="ap-bubble-actions">
          <button
            type="button"
            className="ap-btn ap-btn-approve"
            disabled={busy !== null}
            title={confirmText ?? undefined}
            autoFocus
            onClick={run("approve", () => approve({ confirmed: true }))}
          >
            {busy === "approve" ? "…" : `Approve on ${bubble.confirm?.machineName ?? "that machine"}`}
          </button>
          <button
            type="button"
            className="ap-btn ap-btn-deny"
            disabled={busy !== null}
            onClick={(e) => {
              e.stopPropagation();
              setConfirming(false);
            }}
          >
            Back
          </button>
        </div>
      ) : actionable && (onApprove || onDeny) ? (
        <div className="ap-bubble-actions">
          {approve ? (
            <button
              type="button"
              className="ap-btn ap-btn-approve"
              disabled={busy !== null || !!bubble.approveBlocked}
              title={bubble.approveBlocked}
              onClick={
                bubble.confirm
                  ? (e) => {
                      e.stopPropagation();
                      if (!busy) setConfirming(true);
                    }
                  : run("approve", () => approve({ confirmed: false }))
              }
            >
              {busy === "approve" ? "…" : bubble.confirm ? `${approveLabel}…` : approveLabel}
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
