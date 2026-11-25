import { describe, expect, test } from "bun:test";
import { reactionIconUrl, REACTIONS, splitReaction } from "./reactions";

describe("a reply's leading reaction emoji becomes an icon", () => {
  test("each emoji of the set, with or without its variation selector", () => {
    expect(splitReaction("✅ Tests pass.")).toEqual({ reaction: REACTIONS[3]!, text: "Tests pass." });
    expect(splitReaction("⚠️ Codex needs you.").reaction?.name).toBe("alert");
    expect(splitReaction("⚠ Codex needs you.")).toEqual({ reaction: REACTIONS[2]!, text: "Codex needs you." });
    expect(splitReaction("🛠️ Working on it").reaction?.name).toBe("tools");
    expect(splitReaction("\u{1F6E0} Working on it").text).toBe("Working on it");
    expect(splitReaction("  🐾 Hi!")).toEqual({ reaction: REACTIONS[0]!, text: "Hi!" });
    for (const r of REACTIONS) expect(splitReaction(`${r.emoji} x`).reaction).toBe(r);
  });

  test("other text is left alone", () => {
    expect(splitReaction("Tests pass ✅")).toEqual({ reaction: null, text: "Tests pass ✅" });
    expect(splitReaction("🎉 party")).toEqual({ reaction: null, text: "🎉 party" });
    expect(splitReaction("")).toEqual({ reaction: null, text: "" });
  });

  test("icons are served from /icons/<name>@48.png", () => {
    expect(reactionIconUrl(REACTIONS[4]!)).toBe("/icons/thinking@48.png");
    expect(REACTIONS.map((r) => r.name)).toEqual(["paw", "look", "alert", "done", "thinking", "sleep", "tools"]);
  });
});

describe("rendering a reply", () => {
  test("the icon replaces the emoji, beside the text; other replies render as they are", async () => {
    const { createElement } = await import("react");
    const { renderToStaticMarkup } = await import("react-dom/server");
    const { PetReplyText } = await import("./ReactionIcon");
    const html = renderToStaticMarkup(createElement(PetReplyText, { text: "✅ All 3 tests pass." }));
    expect(html).toContain('src="/icons/done@48.png"');
    expect(html).toContain('alt=""');
    expect(html).toContain("All 3 tests pass.");
    expect(html).not.toContain("✅");
    expect(renderToStaticMarkup(createElement(PetReplyText, { text: "plain reply" }))).toBe("plain reply");
  });
});
