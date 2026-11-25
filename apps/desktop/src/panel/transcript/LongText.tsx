// Very long text (an agent's reply, its thinking, pasted text, a notice's body) shows its first LONG_TEXT_MAX
// characters and a "Show more" button, so one huge message cannot freeze the panel. Nothing is dropped.
import { useState } from "react";

export const LONG_TEXT_MAX = 40_000;

export function useShowMore(text: string, max = LONG_TEXT_MAX): { shown: string; more: number; showAll(): void } {
  const [all, setAll] = useState(false);
  const cut = !all && text.length > max;
  return { shown: cut ? text.slice(0, max) : text, more: cut ? text.length - max : 0, showAll: () => setAll(true) };
}

export function ShowMore({ more, onClick }: { more: number; onClick(): void }) {
  if (more <= 0) return null;
  return (
    <button type="button" className="mt-1 self-start text-xs font-medium text-primary underline underline-offset-2" onClick={onClick}>
      Show {more.toLocaleString()} more characters
    </button>
  );
}

/** Plain text with "Show more" (pre-wrapped). */
export function LongText({ text, className }: { text: string; className?: string }) {
  const { shown, more, showAll } = useShowMore(text);
  return (
    <span className={className}>
      {shown}
      {more > 0 && (
        <>
          {"… "}
          <ShowMore more={more} onClick={showAll} />
        </>
      )}
    </span>
  );
}
