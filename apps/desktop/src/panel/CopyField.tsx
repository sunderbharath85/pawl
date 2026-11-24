// A read-only value with a copy button (pairing invites, CLI lines).
import { useEffect, useState } from "react";
import { copyText } from "../lib/pairing";
import * as I from "./icons";

/** Read-only value with a copy button. `shown` (e.g. with the relay key masked) is displayed; `text` is copied. */
export function CopyField({ label, text, shown, primary, multiline }: { label: string; text: string; shown?: string; primary?: boolean; multiline?: boolean }) {
  const masked = shown !== undefined && shown !== text;
  const [copied, setCopied] = useState<boolean | null>(null);
  useEffect(() => {
    if (copied === null) return;
    const t = setTimeout(() => setCopied(null), 1800);
    return () => clearTimeout(t);
  }, [copied]);
  return (
    <div className="copy-field">
      <code
        className={`copy-value${multiline ? " is-multiline" : ""}`}
        aria-label={label}
        tabIndex={0}
        onFocus={(e) => {
          if (!masked) window.getSelection()?.selectAllChildren(e.currentTarget);
        }}
      >
        {shown ?? text}
      </code>
      <button
        className={`pill${primary ? " pill-primary" : ""} copy-btn`}
        title={`Copy ${label.toLowerCase()}`}
        onClick={() => void copyText(text).then(setCopied)}
      >
        <I.Copy size={14} /> {copied === true ? "Copied" : copied === false ? (masked ? "Copy failed, try again" : "Select to copy") : "Copy"}
      </button>
    </div>
  );
}
