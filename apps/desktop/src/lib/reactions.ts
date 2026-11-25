// The pet's replies start with one reaction emoji from a fixed set (the hub's brain picks it). The bubble and the
// panel's Chat show it as a small icon in the dog's art style (assets/icons, served at /icons/) instead of the emoji,
// and fall back to the emoji itself while an icon file is missing. Pure.

export interface Reaction {
  /** The emoji as the hub writes it. */
  emoji: string;
  /** Icon name: `/icons/<name>.png`. */
  name: string;
  /** What it means, for screen readers and tooltips. */
  label: string;
}

/** The fixed set (assets/icons/manifest.json maps the same names to these emoji). */
export const REACTIONS: readonly Reaction[] = [
  { emoji: "🐾", name: "paw", label: "Hello" },
  { emoji: "👀", name: "look", label: "Looking" },
  { emoji: "⚠️", name: "alert", label: "Needs attention" },
  { emoji: "✅", name: "done", label: "Done" },
  { emoji: "🤔", name: "thinking", label: "Thinking" },
  { emoji: "💤", name: "sleep", label: "Quiet" },
  { emoji: "🛠️", name: "tools", label: "Working" },
];

/** Icons are served here (vite.config.ts copies assets/icons). */
export const ICONS_BASE_URL = "/icons/";

const VS16 = "️";

/**
 * A reply's leading reaction emoji, and the text without it: "✅ Tests pass." -> {done, "Tests pass."}. The emoji
 * may come with or without its variation selector (U+FE0F). Anything else (no emoji, another emoji) is left as is.
 */
export function splitReaction(text: string): { reaction: Reaction | null; text: string } {
  const t = text.trimStart();
  for (const r of REACTIONS) {
    const bare = r.emoji.replace(VS16, "");
    for (const form of [r.emoji, bare]) {
      if (t.startsWith(form)) {
        let rest = t.slice(form.length);
        if (rest.startsWith(VS16)) rest = rest.slice(VS16.length);
        return { reaction: r, text: rest.trimStart() };
      }
    }
  }
  return { reaction: null, text };
}

/** The 48 px file: shown at about 22 px, it stays sharp on 2x screens. */
export function reactionIconUrl(r: Reaction): string {
  return `${ICONS_BASE_URL}${r.name}@48.png`;
}
