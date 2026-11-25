// An agent's reply as markdown (Streamdown), with fenced code in a Code Block, like the AI Elements Message pattern.
// Links never navigate the panel window (it is the app): they show as text with the address on hover. Remote images
// are not loaded (the CSP blocks them anyway): their alt text shows instead.
import type { ComponentProps } from "react";
import { Streamdown, type ExtraProps } from "streamdown";
import { CodeView } from "./CodeView";
import { clipCode } from "@pawl/hub-client";

function MarkdownCode({ node: _node, className, children, ...props }: ComponentProps<"code"> & ExtraProps) {
  // Streamdown marks fenced code with data-block; everything else is inline code.
  if (!("data-block" in props)) {
    return (
      <code className="rounded-sm bg-muted px-1 py-0.5 font-mono text-[0.9em]" {...props}>
        {children}
      </code>
    );
  }
  const text = typeof children === "string" ? children : "";
  const language = /language-(\S+)/.exec(className ?? "")?.[1] ?? "text";
  return <CodeView code={clipCode(text, language, 400, 40_000)} />;
}

function SafeLink({ node: _node, href, children }: ComponentProps<"a"> & ExtraProps) {
  return (
    <span className="underline decoration-dotted underline-offset-2" title={href}>
      {children}
    </span>
  );
}

function ImageAlt({ alt }: ComponentProps<"img"> & ExtraProps) {
  return <span className="text-muted-foreground italic">{alt ? `[image: ${alt}]` : "[image]"}</span>;
}

const components = { code: MarkdownCode, a: SafeLink, img: ImageAlt };

export function Markdown({ children }: { children: string }) {
  return (
    <Streamdown mode="static" controls={false} components={components} className="ai-markdown min-w-0 space-y-3 leading-relaxed">
      {children}
    </Streamdown>
  );
}
