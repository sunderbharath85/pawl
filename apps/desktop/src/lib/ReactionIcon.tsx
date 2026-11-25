// A pet reply's reaction as a small icon (assets/icons), or the emoji itself while the icon cannot be loaded.
// Decorative: the reply's text carries the meaning, so screen readers skip it. Used by the bubble and the panel's Chat.
import { useState } from "react";
import { MiniMarkdown } from "./miniMarkdown";
import { reactionIconUrl, splitReaction, type Reaction } from "./reactions";

export function ReactionIcon({ reaction, size = 22 }: { reaction: Reaction; size?: number }) {
  const [failed, setFailed] = useState(false);
  if (failed) {
    return (
      <span className="reaction-emoji" aria-hidden="true">
        {reaction.emoji}
      </span>
    );
  }
  return (
    <img
      className="reaction-icon"
      src={reactionIconUrl(reaction)}
      width={size}
      height={size}
      alt=""
      aria-hidden="true"
      draggable={false}
      onError={() => setFailed(true)}
    />
  );
}

/**
 * A pet's reply: its leading reaction emoji as an icon beside the text (the text without the emoji). `markdown`
 * (the pet window) renders the text as the safe markdown subset in lib/miniMarkdown.tsx.
 */
export function PetReplyText({ text, markdown }: { text: string; markdown?: boolean }) {
  const { reaction, text: rest } = splitReaction(text);
  if (markdown) {
    return reaction ? (
      <div className="reaction-line">
        <ReactionIcon reaction={reaction} />
        <div className="reaction-text">
          <MiniMarkdown text={rest} />
        </div>
      </div>
    ) : (
      <MiniMarkdown text={text} />
    );
  }
  if (!reaction) return <>{text}</>;
  return (
    <span className="reaction-line">
      <ReactionIcon reaction={reaction} />
      <span className="reaction-text">{rest}</span>
    </span>
  );
}
