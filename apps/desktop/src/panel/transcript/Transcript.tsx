// A session as a conversation, built from AI Elements (shadcn/ui): the person's turns as bubbles, the agent's replies
// as markdown, its thinking as Reasoning, each tool call as a Tool line (Trace) with its input and output, runs of
// tool calls folded into one "Worked: …" step (Chain of Thought), plans as Plan, permission requests as lines, and
// short muted notices. Pending approvals sit at the end as Confirmation cards (our approve flow and rules).
// The data comes from hub-client `toTranscript` (pure, tested).
import { createContext, memo, useContext, useMemo, useRef, useState, type ReactNode } from "react";
import {
  BanIcon,
  BellIcon,
  BrainIcon,
  CircleAlertIcon,
  CircleCheckIcon,
  CircleDotIcon,
  CircleIcon,
  FileTextIcon,
  GlobeIcon,
  HourglassIcon,
  ImageIcon,
  InfoIcon,
  ListChecksIcon,
  LoaderCircleIcon,
  MessageSquareIcon,
  PaperclipIcon,
  PencilIcon,
  SearchIcon,
  ShieldCheckIcon,
  ShieldQuestionIcon,
  ShieldXIcon,
  TerminalIcon,
  WrenchIcon,
} from "lucide-react";
import type { HubEvent, PermissionRequest, Session, ToolKind } from "@pawl/protocol";
import {
  groupWork,
  reuseBlocks,
  reuseItems,
  toTranscript,
  type ToolItem,
  type TranscriptBlock,
  type TranscriptItem,
  type WorkBlock,
} from "@pawl/hub-client";
import { Badge } from "@/components/ui/badge";
import { Bubble, BubbleContent } from "@/components/ui/bubble";
import { Message, MessageContent, MessageFooter } from "@/components/ui/message";
import {
  MessageScroller,
  MessageScrollerButton,
  MessageScrollerContent,
  MessageScrollerItem,
  MessageScrollerProvider,
  MessageScrollerViewport,
} from "@/components/ui/message-scroller";
import {
  Trace,
  TraceCollapsible,
  TraceCollapsibleContent,
  TraceCollapsibleTrigger,
  TraceDescription,
  TraceIcon,
  TraceLine,
  TraceTitle,
} from "@/components/ui/trace";
import { clockTime } from "../../lib/format";
import { CodeView } from "./CodeView";
import { ConfirmationCard } from "./ConfirmationCard";
import { LongText, ShowMore, useShowMore } from "./LongText";
import { Markdown } from "./Markdown";

const KIND_ICON: Record<ToolKind, ReactNode> = {
  execute: <TerminalIcon />,
  read: <FileTextIcon />,
  edit: <PencilIcon />,
  delete: <PencilIcon />,
  move: <PencilIcon />,
  search: <SearchIcon />,
  fetch: <GlobeIcon />,
  think: <BrainIcon />,
  switch_mode: <WrenchIcon />,
  other: <WrenchIcon />,
};

export interface TranscriptProps {
  session: Session;
  events: readonly HubEvent[];
  /** This session's pending permission requests (answered here). */
  pending: readonly PermissionRequest[];
  /** The agent is working: a "Working…" line at the end. */
  working: boolean;
  /** Shown when there is nothing yet. */
  empty: ReactNode;
}

/** Tool calls and requests waiting for an answer here (their lines say so; the card at the end asks). */
const Waiting = createContext<{ tools: ReadonlySet<string>; requests: ReadonlySet<string> }>({ tools: new Set(), requests: new Set() });

/**
 * Which collapsible lines the person opened, by item id, kept for the whole view: a tool line keeps its state when it
 * moves into a "Worked: …" step (it is remounted there) and when its step grows.
 */
const OpenState = createContext<Map<string, boolean>>(new Map());

/** A collapsible's open state, remembered in OpenState under `id`. */
function useRememberedOpen(id: string, initial: boolean): [boolean, (open: boolean) => void] {
  const remembered = useContext(OpenState);
  const [open, setOpen] = useState(() => remembered.get(id) ?? initial);
  return [
    open,
    (next: boolean) => {
      remembered.set(id, next);
      setOpen(next);
    },
  ];
}

export function SessionTranscript({ session, events, pending, working, empty }: TranscriptProps) {
  // Items and blocks that did not change keep their object, so the memoized blocks skip re-rendering: a long session
  // re-renders only what a new event touched.
  const prevItems = useRef<TranscriptItem[]>([]);
  const prevBlocks = useRef<TranscriptBlock[]>([]);
  const openState = useRef(new Map<string, boolean>());
  const all = useMemo(() => {
    const next = reuseItems(prevItems.current, toTranscript(session, events));
    prevItems.current = next;
    return next;
  }, [session.source, session.cwd, events]);
  const waiting = useMemo(
    () => ({
      tools: new Set(pending.map((p) => `tool:${p.toolCall.toolCallId}`)),
      requests: new Set(pending.map((p) => p.requestId)),
    }),
    [pending],
  );
  // A request still waiting is asked by its card at the end, not by a line in the history too.
  const items = useMemo(
    () => all.filter((i) => !(i.kind === "permission" && i.outcome === "pending" && i.requestId && waiting.requests.has(i.requestId))),
    [all, waiting],
  );
  const blocks = useMemo(() => {
    const next = reuseBlocks(prevBlocks.current, groupWork(items));
    prevBlocks.current = next;
    return next;
  }, [items]);
  const last = items[items.length - 1];
  // A hook session that said it waits for the person in its terminal is not "working".
  const busy = working && !(last?.kind === "notice" && last.tone === "waiting");
  const nothing = blocks.length === 0 && pending.length === 0;

  return (
    <Waiting.Provider value={waiting}>
    <OpenState.Provider value={openState.current}>
    <div className="ai-scope flex min-h-0 flex-1 flex-col">
      <MessageScrollerProvider autoScroll>
        <MessageScroller>
          <MessageScrollerViewport>
            <MessageScrollerContent aria-busy={busy} className="mx-auto w-full max-w-3xl gap-5 px-6 py-5">
              {nothing && <div className="m-auto py-10 text-center text-sm text-muted-foreground">{empty}</div>}
              {blocks.map((b) => (
                <MessageScrollerItem key={b.id} messageId={b.id} scrollAnchor={b.kind === "user"}>
                  <Block block={b} />
                </MessageScrollerItem>
              ))}
              {pending.map((req) => (
                <MessageScrollerItem key={req.requestId} messageId={`pending:${req.requestId}`}>
                  <ConfirmationCard req={req} />
                </MessageScrollerItem>
              ))}
              {busy && pending.length === 0 && (
                <MessageScrollerItem messageId="working">
                  <Trace>
                    <TraceLine status="running">
                      <TraceIcon>
                        <LoaderCircleIcon className="animate-spin" />
                      </TraceIcon>
                      <TraceTitle>Working…</TraceTitle>
                    </TraceLine>
                  </Trace>
                </MessageScrollerItem>
              )}
            </MessageScrollerContent>
          </MessageScrollerViewport>
          <MessageScrollerButton />
        </MessageScroller>
      </MessageScrollerProvider>
    </div>
    </OpenState.Provider>
    </Waiting.Provider>
  );
}

/** One block of the conversation. Memoized: unchanged blocks (same object, see reuseBlocks) do not re-render. */
export const Block = memo(function Block({ block }: { block: TranscriptBlock }) {
  switch (block.kind) {
    case "user":
      return <UserTurn item={block} />;
    case "assistant":
      return <AssistantTurn item={block} />;
    case "reasoning":
      return <ReasoningStep item={block} />;
    case "tool":
      return (
        <Trace>
          <ToolRow tool={block} />
        </Trace>
      );
    case "work":
      return <WorkStep work={block} />;
    case "plan":
      return <PlanStep item={block} />;
    case "permission":
      return <PermissionLine item={block} />;
    case "notice":
      return <Notice item={block} />;
  }
});

function AssistantTurn({ item }: { item: Extract<TranscriptItem, { kind: "assistant" }> }) {
  const { shown, more, showAll } = useShowMore(item.text);
  return (
    <Message>
      <MessageContent className="gap-1.5">
        <Markdown>{shown}</Markdown>
        <ShowMore more={more} onClick={showAll} />
        <MessageFooter className="px-0 font-normal">
          <time>{clockTime(item.ts)}</time>
        </MessageFooter>
      </MessageContent>
    </Message>
  );
}

function ReasoningStep({ item }: { item: Extract<TranscriptItem, { kind: "reasoning" }> }) {
  const [open, setOpen] = useRememberedOpen(item.id, false);
  return (
    <Trace>
      <TraceCollapsible open={open} onOpenChange={setOpen}>
        <TraceCollapsibleTrigger>
          <TraceLine>
            <TraceIcon>
              <BrainIcon />
            </TraceIcon>
            <TraceTitle>Thought</TraceTitle>
          </TraceLine>
        </TraceCollapsibleTrigger>
        <TraceCollapsibleContent className="whitespace-pre-wrap">
          <LongText text={item.text} />
        </TraceCollapsibleContent>
      </TraceCollapsible>
    </Trace>
  );
}

function UserTurn({ item }: { item: Extract<TranscriptItem, { kind: "user" }> }) {
  const images = item.attachments.filter((a) => a.kind === "image");
  const pasted = item.attachments.filter((a) => a.kind === "pasted");
  return (
    <Message align="end">
      <MessageContent className="gap-1.5">
        <Bubble align="end" variant="muted">
          <BubbleContent className="whitespace-pre-wrap">
            {item.text || (pasted.length ? "Pasted text" : "Image")}
            {(images.length > 0 || pasted.length > 0) && (
              <span className="mt-2 flex flex-col items-end gap-1.5">
                {images.length > 0 && (
                  <span className="flex flex-wrap justify-end gap-1">
                    {images.map((a, i) => (
                      <Badge key={i} variant="outline" className="gap-1">
                        <ImageIcon className="size-3" /> {a.kind === "image" ? a.label : "Image"}
                      </Badge>
                    ))}
                  </span>
                )}
                {pasted.map((a, i) => (
                  <PastedText key={i} text={a.kind === "pasted" ? a.text : ""} />
                ))}
              </span>
            )}
          </BubbleContent>
        </Bubble>
        <MessageFooter className="font-normal">
          <time>{clockTime(item.ts)}</time>
        </MessageFooter>
      </MessageContent>
    </Message>
  );
}

function PastedText({ text }: { text: string }) {
  const lines = text.split("\n").length;
  return (
    <Trace className="items-end">
      <TraceCollapsible className="items-end">
        <TraceCollapsibleTrigger>
          <TraceLine>
            <TraceIcon>
              <PaperclipIcon />
            </TraceIcon>
            <TraceTitle>Pasted text</TraceTitle>
            <TraceDescription>
              {lines} line{lines === 1 ? "" : "s"}
            </TraceDescription>
          </TraceLine>
        </TraceCollapsibleTrigger>
        <TraceCollapsibleContent className="max-h-64 overflow-auto text-left font-mono text-xs whitespace-pre-wrap">
          <LongText text={text} />
        </TraceCollapsibleContent>
      </TraceCollapsible>
    </Trace>
  );
}

function statusIcon(tool: Pick<ToolItem, "status" | "toolKind">): ReactNode {
  switch (tool.status) {
    case "running":
      return <LoaderCircleIcon className="animate-spin" />;
    case "error":
      return <CircleAlertIcon />;
    case "denied":
      return <BanIcon />;
    case "done":
      return KIND_ICON[tool.toolKind];
  }
}

const STATUS_WORD: Partial<Record<ToolItem["status"], string>> = { error: "failed", denied: "refused", running: "running" };

export function ToolRow({ tool }: { tool: ToolItem }) {
  const asking = useContext(Waiting).tools.has(tool.id) && tool.status === "running";
  const [open, setOpen] = useRememberedOpen(tool.id, tool.status === "error");
  const line = (
    <TraceLine status={tool.status === "running" ? "running" : tool.status === "error" ? "error" : undefined}>
      <TraceIcon>{statusIcon(tool)}</TraceIcon>
      <TraceTitle>
        <span className="font-medium text-foreground">{tool.name}</span> {tool.title !== tool.name ? tool.title : null}
      </TraceTitle>
      {asking ? <TraceDescription>waiting for your answer</TraceDescription> : STATUS_WORD[tool.status] && <TraceDescription>{STATUS_WORD[tool.status]}</TraceDescription>}
    </TraceLine>
  );
  const hasDetail = !!(tool.input || tool.output || tool.error);
  if (!hasDetail) return line;
  return (
    <TraceCollapsible open={open} onOpenChange={setOpen}>
      <TraceCollapsibleTrigger>{line}</TraceCollapsibleTrigger>
      <TraceCollapsibleContent className="flex flex-col gap-2">
        {tool.input && <CodeView code={tool.input} label="Input" />}
        {tool.output && <CodeView code={tool.output} label="Output" />}
        {tool.error && <CodeView code={{ text: tool.error, language: "text", omittedLines: 0 }} tone="destructive" label="Error" />}
      </TraceCollapsibleContent>
    </TraceCollapsible>
  );
}

function WorkStep({ work }: { work: WorkBlock }) {
  const [open, setOpen] = useRememberedOpen(work.id, false);
  const failed = work.tools.some((t) => t.status === "error");
  const status = work.running ? "running" : failed ? "error" : undefined;
  return (
    <Trace>
      <TraceCollapsible open={open} onOpenChange={setOpen}>
        <TraceCollapsibleTrigger>
          <TraceLine status={status}>
            <TraceIcon>{work.running ? <LoaderCircleIcon className="animate-spin" /> : failed ? <CircleAlertIcon /> : <CircleCheckIcon />}</TraceIcon>
            <TraceTitle>{work.summary}</TraceTitle>
          </TraceLine>
        </TraceCollapsibleTrigger>
        <TraceCollapsibleContent>
          <Trace>
            {work.tools.map((t) => (
              <ToolRow key={t.id} tool={t} />
            ))}
          </Trace>
        </TraceCollapsibleContent>
      </TraceCollapsible>
    </Trace>
  );
}

function PlanStep({ item }: { item: Extract<TranscriptItem, { kind: "plan" }> }) {
  const done = item.entries.filter((e) => e.status === "completed").length;
  const running = done < item.entries.length && item.entries.some((e) => e.status === "in_progress");
  return (
    <Trace>
      <TraceCollapsible defaultOpen>
        <TraceCollapsibleTrigger>
          <TraceLine>
            <TraceIcon>
              <ListChecksIcon />
            </TraceIcon>
            <TraceTitle>Plan</TraceTitle>
            <TraceDescription>
              {done} of {item.entries.length} done
            </TraceDescription>
          </TraceLine>
        </TraceCollapsibleTrigger>
        <TraceCollapsibleContent>
          <Trace>
            {item.entries.map((e, i) => (
              <TraceLine key={i} status={e.status === "pending" ? "pending" : running && e.status === "in_progress" ? "running" : undefined}>
                <TraceIcon>{e.status === "completed" ? <CircleCheckIcon /> : e.status === "in_progress" ? <CircleDotIcon /> : <CircleIcon />}</TraceIcon>
                <TraceTitle className="whitespace-normal">{e.content}</TraceTitle>
              </TraceLine>
            ))}
          </Trace>
        </TraceCollapsibleContent>
      </TraceCollapsible>
    </Trace>
  );
}

const OUTCOME: Record<Extract<TranscriptItem, { kind: "permission" }>["outcome"], { word: string; icon: ReactNode }> = {
  pending: { word: "waiting for you", icon: <ShieldQuestionIcon /> },
  approved: { word: "approved", icon: <ShieldCheckIcon /> },
  denied: { word: "denied", icon: <ShieldXIcon /> },
  cancelled: { word: "withdrawn", icon: <ShieldXIcon /> },
};

function PermissionLine({ item }: { item: Extract<TranscriptItem, { kind: "permission" }> }) {
  const o = OUTCOME[item.outcome];
  return (
    <Trace>
      <TraceLine status={item.outcome === "denied" ? "error" : undefined}>
        <TraceIcon>{o.icon}</TraceIcon>
        <TraceTitle>Asked for approval: {item.title}</TraceTitle>
        <TraceDescription>
          {o.word}
          {item.by === "rule" ? " by a rule" : ""}
        </TraceDescription>
      </TraceLine>
    </Trace>
  );
}

const NOTICE_ICON: Record<Extract<TranscriptItem, { kind: "notice" }>["tone"], ReactNode> = {
  muted: <InfoIcon />,
  info: <BellIcon />,
  waiting: <HourglassIcon />,
  error: <CircleAlertIcon />,
};

function Notice({ item }: { item: Extract<TranscriptItem, { kind: "notice" }> }) {
  const icon = item.text.startsWith("Message from") ? <MessageSquareIcon /> : NOTICE_ICON[item.tone];
  const line = (
    <TraceLine status={item.tone === "error" ? "error" : undefined} className={item.tone === "muted" ? "text-xs opacity-80" : "text-xs"}>
      <TraceIcon>{icon}</TraceIcon>
      <TraceTitle className={item.tone === "waiting" ? "text-foreground" : undefined}>{item.text}</TraceTitle>
      <TraceDescription>{clockTime(item.ts)}</TraceDescription>
    </TraceLine>
  );
  if (!item.detail) return <Trace>{line}</Trace>;
  return (
    <Trace>
      <TraceCollapsible>
        <TraceCollapsibleTrigger>{line}</TraceCollapsibleTrigger>
        <TraceCollapsibleContent className="text-xs whitespace-pre-wrap">
          <LongText text={item.detail} />
        </TraceCollapsibleContent>
      </TraceCollapsible>
    </Trace>
  );
}
