// Which pet's bubble is open, and its draft (pure). A bubble belongs to one target (the default pet, or a session
// pet keyed by its session id): what the user typed for one target is never shown in, or sent to, another.

/** The default pet's bubble; a session pet's bubble is keyed by its session id. */
export const DEFAULT_TARGET = "default";

export interface OpenBubble {
  target: string;
  /** "click": the user opened it (focus its text box); "auto": an approval or a tray click; "hub": hub status. */
  by: "click" | "auto" | "hub";
}

/** A click on a pet: toggles its bubble (another pet's open bubble gives way to it). */
export function clickBubble(open: OpenBubble | null, target: string): OpenBubble | null {
  return open?.target === target ? null : { target, by: "click" };
}

/**
 * Something other than the user wants a bubble open (a new approval, a tray click). It opens only when no bubble is
 * open: it never replaces an open bubble (the user may be typing there); the pet's badge shows the approval instead.
 */
export function autoOpenBubble(open: OpenBubble | null, target: string): OpenBubble | null {
  return open ?? { target, by: "auto" };
}

/** What the text box holds, and for which target it was typed. */
export interface Draft {
  target: string;
  text: string;
}

/** The draft shown (and sent) in `target`'s bubble: empty when it was typed for another target. */
export function draftFor(draft: Draft, target: string): string {
  return draft.target === target ? draft.text : "";
}

/** The bot did not understand the message ("Not sure what you mean. Try …"): the bubble offers examples to tap. */
export function isNotSureReply(text: string): boolean {
  return /\bnot sure what you mean\b/i.test(text);
}

/**
 * Three example messages to tap after a "not sure" reply, fitted to what is going on: "approve" when something
 * waits, "what's everyone doing?", running the tests in the most recent session's repo, else the daily report.
 */
export function exampleChips(ctx: { pendingCount: number; recentRepo: string | null; agentName: string | null }): string[] {
  const chips: string[] = [];
  if (ctx.pendingCount > 0) chips.push("approve");
  chips.push("what's everyone doing?");
  if (ctx.recentRepo) chips.push(`${ctx.agentName ? `tell ${ctx.agentName.toLowerCase()} to ` : ""}run the tests in ${ctx.recentRepo}`);
  chips.push("daily report");
  return chips.slice(0, 3);
}

/**
 * The hub is already set up (so the "New here? … finish setting up" hint must not show): this machine has a name and
 * the hub has seen a session, or an agent's hooks are installed.
 */
export function setupLooksDone(input: { machineName: string | null | undefined; sessionCount: number; hooksInstalled: boolean }): boolean {
  return !!input.machineName?.trim() && (input.sessionCount > 0 || input.hooksInstalled);
}
