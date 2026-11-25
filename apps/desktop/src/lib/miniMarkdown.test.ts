import { describe, expect, test } from "bun:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { MiniMarkdown, parseInline } from "./miniMarkdown";

const html = (text: string) => renderToStaticMarkup(createElement(MiniMarkdown, { text }));

describe("the pet bubble's markdown subset", () => {
  test("bold, italic, inline code; snake_case and lone stars stay text", () => {
    expect(html("Renamed **greet** to *welcome* in `src/greet.ts`.")).toBe(
      '<div class="md"><p>Renamed <strong>greet</strong> to <em>welcome</em> in <code class="md-code">src/greet.ts</code>.</p></div>',
    );
    expect(html("__bold__ and _it_ but snake_case_name and 2 * 3 * 4")).toBe(
      '<div class="md"><p><strong>bold</strong> and <em>it</em> but snake_case_name and 2 * 3 * 4</p></div>',
    );
    // Nothing inside code is formatting.
    expect(html("`**not bold**`")).toBe('<div class="md"><p><code class="md-code">**not bold**</code></p></div>');
    expect(parseInline("**bold with `code` inside**")).toEqual([{ t: "strong", c: [{ t: "text", v: "bold with " }, { t: "code", v: "code" }, { t: "text", v: " inside" }] }]);
  });

  test("bullet and numbered lists, line breaks and paragraphs", () => {
    expect(html("Changes:\n- `src/app.ts` imports it\n- tests pass\n\nDone.")).toBe(
      '<div class="md"><p>Changes:</p><ul><li><code class="md-code">src/app.ts</code> imports it</li><li>tests pass</li></ul><p>Done.</p></div>',
    );
    expect(html("3. third\n4. fourth")).toBe('<div class="md"><ol start="3"><li>third</li><li>fourth</li></ol></div>');
    expect(html("1. one\n2. two")).toBe("<div class=\"md\"><ol><li>one</li><li>two</li></ol></div>");
    expect(html("line one\nline two")).toBe('<div class="md"><p>line one<br/>line two</p></div>');
    expect(html("- a wrapped\n  item")).toBe('<div class="md"><ul><li>a wrapped item</li></ul></div>');
  });

  test("code blocks are monospace text; an unclosed one (a clipped message) stays code", () => {
    expect(html("Run:\n```sh\nnpm test -- --watch\n```\nok")).toBe(
      '<div class="md"><p>Run:</p><pre class="md-pre"><code>npm test -- --watch</code></pre><p>ok</p></div>',
    );
    expect(html("```\nlet a = 1;\nlet b")).toBe('<div class="md"><pre class="md-pre"><code>let a = 1;\nlet b</code></pre></div>');
  });

  test("safe: links are inert text, images are their alt text, HTML is shown as text", () => {
    const out = html("See [the PR](https://evil.example/x) ![shot](https://evil.example/a.png) <img src=x onerror=alert(1)> <b>hi</b>");
    expect(out).not.toContain("<a");
    expect(out).not.toContain("href");
    expect(out).not.toContain("<img");
    expect(out).not.toContain("<b>");
    expect(out).toContain("See the PR shot &lt;img src=x onerror=alert(1)&gt; &lt;b&gt;hi&lt;/b&gt;");
    expect(html("[x](javascript:alert(1))")).toBe('<div class="md"><p>x</p></div>');
  });

  test("headings and quotes are light emphasis; rules disappear", () => {
    expect(html("## Summary\n> quoted\n> more\n---\nend")).toBe(
      '<div class="md"><p class="md-h"><strong>Summary</strong></p><p class="md-quote">quoted<br/>more</p><p>end</p></div>',
    );
  });
});
