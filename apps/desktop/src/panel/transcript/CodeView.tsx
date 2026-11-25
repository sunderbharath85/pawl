// A tool's input or output (hub-client `CodeText`) in a shadcn/ui Code Block: highlighted when its language is known
// (shiki loads on first use, see hooks/use-highlighted-code), diffs as added / removed lines, and a footer that says
// how many lines were left out.
import { CodeBlock, CodeBlockActions, CodeBlockContent, CodeBlockCopyButton, CodeBlockFooter, CodeBlockLine } from "@/components/ui/code-block";
import { useHighlightedCode } from "@/hooks/use-highlighted-code";
import type { CodeText } from "@pawl/hub-client";

/** Languages the highlighter knows (lib/highlighter.tsx); anything else shows as plain text. */
const HIGHLIGHTED = new Set(["bash", "css", "html", "javascript", "json", "jsx", "markdown", "python", "sql", "tsx", "typescript", "yaml"]);

function moreLines(n: number): string {
  return `… ${n} more line${n === 1 ? "" : "s"} not shown`;
}

export function CodeView({ code, tone = "default", label }: { code: CodeText; tone?: "default" | "destructive"; label?: string }) {
  return (
    <CodeBlock size="sm" variant={tone} aria-label={label}>
      <CodeBlockActions>
        <CodeBlockCopyButton value={code.text} />
      </CodeBlockActions>
      {code.language === "diff" ? <DiffLines text={code.text} /> : <HighlightedLines text={code.text} language={code.language} />}
      {code.omittedLines > 0 && <CodeBlockFooter className="text-xs">{moreLines(code.omittedLines)}</CodeBlockFooter>}
    </CodeBlock>
  );
}

function HighlightedLines({ text, language }: { text: string; language: string }) {
  const lines = useHighlightedCode(text, HIGHLIGHTED.has(language) ? language : "text");
  return (
    <CodeBlockContent wrap className="max-h-72">
      {lines.map((line, i) => (
        <CodeBlockLine key={i}>{line}</CodeBlockLine>
      ))}
    </CodeBlockContent>
  );
}

/** "+" / "-" lines tinted, keeping their sign in the text (no separate gutter marker); file headers stay plain. */
function DiffLines({ text }: { text: string }) {
  return (
    <CodeBlockContent wrap className="max-h-72">
      {text.split("\n").map((line, i) => {
        if (line.startsWith("+++") || line.startsWith("---")) return <CodeBlockLine key={i} className="text-muted-foreground">{line}</CodeBlockLine>;
        if (line.startsWith("+")) return <CodeBlockLine key={i} variant="added" className="after:hidden">{line}</CodeBlockLine>;
        if (line.startsWith("-")) return <CodeBlockLine key={i} variant="removed" className="text-destructive after:hidden">{line}</CodeBlockLine>;
        return <CodeBlockLine key={i}>{line}</CodeBlockLine>;
      })}
    </CodeBlockContent>
  );
}
