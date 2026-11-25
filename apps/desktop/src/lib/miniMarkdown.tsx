// A small, safe markdown subset for the pet window's bubble (agent and pet messages), without Streamdown or shiki in
// the pet entry: **bold**, *italic*, `inline code`, fenced code blocks (monospace, no highlighting), bullet and
// numbered lists, headings and quotes as plain emphasis, and line breaks. Links become their text (inert: no <a>,
// no address), images become their alt text, and HTML is never interpreted: everything is built as React elements,
// so "<b>" shows as the characters "<b>".
import { Fragment, type ReactNode } from "react";

export type MdInline =
  | { t: "text"; v: string }
  | { t: "code"; v: string }
  | { t: "strong"; c: MdInline[] }
  | { t: "em"; c: MdInline[] };

export type MdBlock =
  | { t: "p"; lines: MdInline[][] }
  | { t: "h"; c: MdInline[] }
  | { t: "quote"; lines: MdInline[][] }
  | { t: "code"; v: string }
  | { t: "ul"; items: MdInline[][] }
  | { t: "ol"; start: number; items: MdInline[][] };

// Inline: code first (nothing inside it is formatting), then images and links (their text only), bold, italic.
// Underscores only count at word edges, so snake_case_names stay as they are.
const INLINE =
  /(`+)([\s\S]*?[^`])\1(?!`)|!\[([^\]]*)\]\((?:[^()]|\([^()]*\))*\)|\[([^\]]+)\]\((?:[^()]|\([^()]*\))*\)|\*\*(?=\S)([\s\S]*?\S)\*\*|__(?=\S)([\s\S]*?\S)__(?!\w)|\*(?=[^\s*])([\s\S]*?[^\s*])\*|(?<![\w])_(?=[^\s_])([\s\S]*?[^\s_])_(?!\w)/g;

export function parseInline(text: string, depth = 0): MdInline[] {
  const out: MdInline[] = [];
  const push = (v: string) => {
    if (!v) return;
    const last = out[out.length - 1];
    if (last?.t === "text") last.v += v;
    else out.push({ t: "text", v });
  };
  if (depth > 4) {
    push(text);
    return out;
  }
  let last = 0;
  for (const m of text.matchAll(INLINE)) {
    push(text.slice(last, m.index));
    last = m.index + m[0].length;
    if (m[2] !== undefined) {
      const v = m[2];
      out.push({ t: "code", v: v.length > 1 && v.startsWith(" ") && v.endsWith(" ") ? v.slice(1, -1) : v });
    } else if (m[3] !== undefined) push(m[3]);
    else if (m[4] !== undefined) out.push(...parseInline(m[4], depth + 1));
    else if (m[5] !== undefined || m[6] !== undefined) out.push({ t: "strong", c: parseInline((m[5] ?? m[6])!, depth + 1) });
    else out.push({ t: "em", c: parseInline((m[7] ?? m[8])!, depth + 1) });
  }
  push(text.slice(last));
  return out;
}

const FENCE = /^\s{0,3}(```+|~~~+)/;
const BULLET = /^\s*[-*+]\s+(.*)$/;
const NUMBERED = /^\s*(\d{1,9})[.)]\s+(.*)$/;
const HEADING = /^\s{0,3}#{1,6}\s+(.*?)\s*#*\s*$/;
const QUOTE = /^\s{0,3}>\s?(.*)$/;
const RULE = /^\s{0,3}([-*_])(\s*\1){2,}\s*$/;

export function parseMarkdown(text: string): MdBlock[] {
  const lines = text.replace(/\r\n?/g, "\n").split("\n");
  const blocks: MdBlock[] = [];
  let para: MdInline[][] | null = null;
  let quote: MdInline[][] | null = null;
  const endText = () => {
    para = null;
    quote = null;
  };
  for (let i = 0; i < lines.length; i++) {
    const line = lines[i]!;
    const fence = FENCE.exec(line);
    if (fence) {
      // A code block runs to its closing fence (or the end: a clipped message keeps its code as code).
      const mark = fence[1]!;
      const body: string[] = [];
      for (i++; i < lines.length && !lines[i]!.trimStart().startsWith(mark); i++) body.push(lines[i]!);
      blocks.push({ t: "code", v: body.join("\n") });
      endText();
      continue;
    }
    if (!line.trim()) {
      endText();
      continue;
    }
    if (RULE.test(line)) {
      endText();
      continue;
    }
    const bullet = BULLET.exec(line);
    const numbered = bullet ? null : NUMBERED.exec(line);
    if (bullet || numbered) {
      endText();
      const prev = blocks[blocks.length - 1];
      const item = parseInline((bullet ? bullet[1] : numbered![2])!);
      if (bullet && prev?.t === "ul") prev.items.push(item);
      else if (numbered && prev?.t === "ol") prev.items.push(item);
      else blocks.push(bullet ? { t: "ul", items: [item] } : { t: "ol", start: Number(numbered![1]), items: [item] });
      continue;
    }
    const heading = HEADING.exec(line);
    if (heading) {
      endText();
      blocks.push({ t: "h", c: parseInline(heading[1]!) });
      continue;
    }
    const q = QUOTE.exec(line);
    if (q) {
      para = null;
      if (!quote) {
        quote = [];
        blocks.push({ t: "quote", lines: quote });
      }
      quote.push(parseInline(q[1]!));
      continue;
    }
    // A line right after a list item continues it (a wrapped item), else it is paragraph text.
    const prev = blocks[blocks.length - 1];
    if (!para && !quote && (prev?.t === "ul" || prev?.t === "ol") && /^\s+\S/.test(line)) {
      const items = prev.items;
      items[items.length - 1]!.push({ t: "text", v: " " }, ...parseInline(line.trim()));
      continue;
    }
    quote = null;
    if (!para) {
      para = [];
      blocks.push({ t: "p", lines: para });
    }
    para.push(parseInline(line.trim()));
  }
  return blocks;
}

function renderInline(nodes: MdInline[]): ReactNode[] {
  return nodes.map((n, i) => {
    switch (n.t) {
      case "text":
        return <Fragment key={i}>{n.v}</Fragment>;
      case "code":
        return (
          <code key={i} className="md-code">
            {n.v}
          </code>
        );
      case "strong":
        return <strong key={i}>{renderInline(n.c)}</strong>;
      case "em":
        return <em key={i}>{renderInline(n.c)}</em>;
    }
  });
}

function renderLines(lines: MdInline[][]): ReactNode[] {
  return lines.map((l, i) => (
    <Fragment key={i}>
      {i > 0 && <br />}
      {renderInline(l)}
    </Fragment>
  ));
}

/** Markdown text (an agent's or the pet's message) as the safe subset above. */
export function MiniMarkdown({ text }: { text: string }) {
  const blocks = parseMarkdown(text);
  return (
    <div className="md">
      {blocks.map((b, i) => {
        switch (b.t) {
          case "p":
            return <p key={i}>{renderLines(b.lines)}</p>;
          case "h":
            return (
              <p key={i} className="md-h">
                <strong>{renderInline(b.c)}</strong>
              </p>
            );
          case "quote":
            return (
              <p key={i} className="md-quote">
                {renderLines(b.lines)}
              </p>
            );
          case "code":
            return (
              <pre key={i} className="md-pre">
                <code>{b.v}</code>
              </pre>
            );
          case "ul":
            return (
              <ul key={i}>
                {b.items.map((it, j) => (
                  <li key={j}>{renderInline(it)}</li>
                ))}
              </ul>
            );
          case "ol":
            return (
              <ol key={i} start={b.start === 1 ? undefined : b.start}>
                {b.items.map((it, j) => (
                  <li key={j}>{renderInline(it)}</li>
                ))}
              </ol>
            );
        }
      })}
    </div>
  );
}
