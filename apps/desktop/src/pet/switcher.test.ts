import { describe, expect, test } from "bun:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import type { SessionChip } from "../lib/petScreen";
import { SessionSwitcher } from "./Bubble";

function chip(id: string, over: Partial<SessionChip> = {}): SessionChip {
  return { sessionId: id, machineId: "m", name: "Rex", repo: id, label: `Rex · ${id}`, needsYou: false, pending: [], running: true, remote: false, lastEventAt: 0, ...over };
}

describe("the bubble's session switcher", () => {
  test("Pawl-wide first (selected by default), then the sessions; needs-you marked; the rest under more", () => {
    const html = renderToStaticMarkup(
      createElement(SessionSwitcher, {
        switcher: { chips: [chip("api", { needsYou: true, pending: [{} as never, {} as never] }), chip("web")], more: [chip("docs"), chip("cli")] },
        focus: null,
        onFocus: () => {},
      }),
    );
    expect(html.indexOf("Pawl-wide")).toBeLessThan(html.indexOf("api"));
    expect(html).toMatch(/aria-selected="true"[^>]*>Pawl-wide/);
    expect(html).toContain('aria-label="needs you"');
    expect(html).toContain('class="pb-switch-count">2<');
    expect(html).toContain("+2 more");
    expect(html).toContain(">Rex · docs</option>");
  });

  test("the focused session is the selected tab; no sessions: no switcher", () => {
    const web = chip("web");
    const html = renderToStaticMarkup(createElement(SessionSwitcher, { switcher: { chips: [chip("api"), web], more: [] }, focus: web, onFocus: () => {} }));
    expect(html).toMatch(/aria-selected="false"[^>]*>Pawl-wide/);
    expect(html).toMatch(/aria-selected="true" class="pb-switch-chip is-active"[^>]*title="Rex · web/);
    expect(renderToStaticMarkup(createElement(SessionSwitcher, { switcher: { chips: [], more: [] }, focus: null, onFocus: () => {} }))).toBe("");
  });
});
