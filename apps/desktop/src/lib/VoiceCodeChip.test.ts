import { describe, expect, test } from "bun:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { VoiceCodeChip } from "./VoiceCodeChip";

describe("the spoken code on approval cards", () => {
  test("says what to say, digit by digit; nothing without a code", () => {
    const html = renderToStaticMarkup(createElement(VoiceCodeChip, { code: "47" }));
    expect(html).toContain('aria-label="To approve by voice, say: approve 4 7"');
    expect(html).toContain('<span class="voice-code-say">say:</span> approve <strong class="voice-code-digits">4 7</strong>');
    expect(renderToStaticMarkup(createElement(VoiceCodeChip, { code: undefined }))).toBe("");
  });
});
