import { describe, expect, test } from "bun:test";
import { autoOpenBubble, clickBubble, DEFAULT_TARGET, draftFor, exampleChips, isNotSureReply, setupLooksDone, type OpenBubble } from "./bubble";

const A = "m1:session-a";
const B = "m1:session-b";

describe("which bubble is open", () => {
  test("a click toggles that pet's bubble; another pet's bubble gives way", () => {
    const a = clickBubble(null, A);
    expect(a).toEqual({ target: A, by: "click" });
    expect(clickBubble(a, A)).toBeNull();
    expect(clickBubble(a, B)).toEqual({ target: B, by: "click" });
    expect(clickBubble(a, DEFAULT_TARGET)).toEqual({ target: DEFAULT_TARGET, by: "click" });
  });

  test("an approval (or a tray click) never replaces an open bubble", () => {
    const typingInA: OpenBubble = { target: A, by: "click" };
    // A new approval for session B while the user types in A's bubble: A's bubble stays (B's pet shows a badge).
    expect(autoOpenBubble(typingInA, B)).toBe(typingInA);
    expect(autoOpenBubble(typingInA, DEFAULT_TARGET)).toBe(typingInA);
    const defaultOpen: OpenBubble = { target: DEFAULT_TARGET, by: "auto" };
    expect(autoOpenBubble(defaultOpen, B)).toBe(defaultOpen);
    // Nothing open: it opens.
    expect(autoOpenBubble(null, B)).toEqual({ target: B, by: "auto" });
  });
});

describe("drafts never move to another target", () => {
  test("text typed for session A is not shown (or sent) in B's bubble", () => {
    const draft = { target: A, text: "rm -rf build and rerun" };
    expect(draftFor(draft, A)).toBe("rm -rf build and rerun");
    expect(draftFor(draft, B)).toBe("");
    expect(draftFor(draft, DEFAULT_TARGET)).toBe("");
  });
});

describe("example chips after a reply the bot did not understand", () => {
  test("only the bot's 'not sure' reply", () => {
    expect(isNotSureReply("*tilts head* Not sure what you mean. Try “what's Claude doing?”")).toBe(true);
    expect(isNotSureReply("All quiet. Nobody needs you.")).toBe(false);
  });

  test("three tappable examples fitted to the moment", () => {
    expect(exampleChips({ pendingCount: 0, recentRepo: null, agentName: null })).toEqual(["what's everyone doing?", "daily report"]);
    expect(exampleChips({ pendingCount: 0, recentRepo: "api", agentName: "Codex" })).toEqual([
      "what's everyone doing?",
      "tell codex to run the tests in api",
      "daily report",
    ]);
    expect(exampleChips({ pendingCount: 2, recentRepo: "api", agentName: null })).toEqual(["approve", "what's everyone doing?", "run the tests in api"]);
  });
});

describe("a hub that is already set up does not get the setup hint", () => {
  test("a named machine plus a session or an installed hook", () => {
    expect(setupLooksDone({ machineName: "Studio PC", sessionCount: 3, hooksInstalled: false })).toBe(true);
    expect(setupLooksDone({ machineName: "Studio PC", sessionCount: 0, hooksInstalled: true })).toBe(true);
    expect(setupLooksDone({ machineName: "Studio PC", sessionCount: 0, hooksInstalled: false })).toBe(false);
    expect(setupLooksDone({ machineName: " ", sessionCount: 5, hooksInstalled: true })).toBe(false);
    expect(setupLooksDone({ machineName: undefined, sessionCount: 5, hooksInstalled: true })).toBe(false);
  });
});
