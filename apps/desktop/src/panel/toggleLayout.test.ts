// The blank-window bug (Settings > A2A / Voice in 0.1.1): `.toggle input` is absolutely positioned and hidden; with no
// positioned `.toggle` its containing block was the window, so clicking a toggle far down Settings focused a checkbox
// sitting at its unscrolled place and the browser scrolled the whole window to it, out of the app.
import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { join } from "node:path";

const css = readFileSync(join(import.meta.dir, "panel.css"), "utf8").replace(/\r\n/g, "\n");

/** The declarations of the first rule with exactly this selector. */
function rule(selector: string): string {
  const at = css.indexOf(`\n${selector} {`);
  if (at < 0) throw new Error(`no rule ${selector}`);
  const start = css.indexOf("{", at) + 1;
  return css.slice(start, css.indexOf("}", start)).replace(/\/\*[\s\S]*?\*\//g, "");
}

describe("panel layout: toggles", () => {
  test("the hidden checkbox is positioned against its own toggle (never the window)", () => {
    expect(rule(".toggle input")).toMatch(/position:\s*absolute/);
    expect(rule(".toggle")).toMatch(/position:\s*relative/);
  });
});
