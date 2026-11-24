import * as vscode from "vscode";
import * as path from "node:path";
import type { HubEvent, ToolCallPayload, ToolKind, ToolLocation } from "@ai-pet/protocol";
import type { HubConnection } from "./connection";
import { headUriFor } from "./git";
import { errorMessage, toFileUri, workspaceFolderOf } from "./util";

type OpenMode = "off" | "open" | "diff";

interface TrackedCall {
  kind?: ToolKind;
  locations: ToolLocation[];
  opened: boolean;
  seenAt: number;
}

/** Forget tool calls we have not heard about for this long. */
const TRACK_TTL_MS = 30 * 60_000;
/** Do not reopen the same file more often than this (agents often write a file several times in a row). */
const REOPEN_COOLDOWN_MS = 1500;

/**
 * Watches live `tool_call` / `tool_call_update` events; when an `edit` call completes on files inside this
 * workspace, opens them (or a diff against git HEAD) per `ai-pet.openEditedFiles`.
 */
export class EditedFilesWatcher implements vscode.Disposable {
  private readonly calls = new Map<string, TrackedCall>();
  private readonly lastOpened = new Map<string, number>();
  private readonly disposables: vscode.Disposable[] = [];
  private queue: Promise<void> = Promise.resolve();

  constructor(
    private readonly hub: HubConnection,
    private readonly log: vscode.LogOutputChannel,
  ) {
    this.disposables.push(
      hub.onMessage((msg) => {
        if (msg.type === "event") this.onEvent(msg.event);
      }),
    );
  }

  private get mode(): OpenMode {
    const v = vscode.workspace.getConfiguration("ai-pet").get<string>("openEditedFiles");
    return v === "off" || v === "diff" ? v : "open";
  }

  private onEvent(e: HubEvent): void {
    if (e.type !== "tool_call" && e.type !== "tool_call_update") return;
    const p = e.payload as Partial<ToolCallPayload> | null;
    if (!p || typeof p.toolCallId !== "string") return;
    this.prune();
    const key = `${e.sessionId}:${p.toolCallId}`;
    let call = this.calls.get(key);
    if (!call) {
      call = { locations: [], opened: false, seenAt: Date.now() };
      this.calls.set(key, call);
    }
    call.seenAt = Date.now();
    if (p.kind) call.kind = p.kind;
    if (Array.isArray(p.locations) && p.locations.length > 0) call.locations = p.locations;
    if (call.opened || call.kind !== "edit" || p.status !== "completed") return;
    call.opened = true;
    if (this.mode === "off" || call.locations.length === 0) return;
    const cwd = this.hub.state.sessions[e.sessionId]?.cwd;
    const locations = call.locations;
    this.queue = this.queue.then(() => this.openAll(locations, cwd)).catch(() => undefined);
  }

  private async openAll(locations: ToolLocation[], cwd: string | undefined): Promise<void> {
    const mode = this.mode;
    if (mode === "off") return;
    const seen = new Set<string>();
    for (const loc of locations) {
      if (!loc?.path) continue;
      const uri = toFileUri(loc.path, cwd);
      if (uri.scheme !== "file" || !workspaceFolderOf(uri)) continue;
      const key = process.platform === "win32" ? uri.fsPath.toLowerCase() : uri.fsPath;
      if (seen.has(key)) continue;
      seen.add(key);
      const last = this.lastOpened.get(key) ?? 0;
      if (Date.now() - last < REOPEN_COOLDOWN_MS) continue;
      this.lastOpened.set(key, Date.now());
      try {
        if (mode === "diff") await this.openDiff(uri, loc.line);
        else await this.openFile(uri, loc.line);
      } catch (err) {
        this.log.debug(`could not open edited file ${uri.fsPath}: ${errorMessage(err)}`);
      }
    }
  }

  private async openFile(uri: vscode.Uri, line?: number): Promise<void> {
    await vscode.window.showTextDocument(uri, { preview: true, preserveFocus: true, selection: selectionFor(line) });
  }

  private async openDiff(uri: vscode.Uri, line?: number): Promise<void> {
    const head = await headUriFor(uri);
    if (!head) {
      await this.openFile(uri, line);
      return;
    }
    const title = `${path.basename(uri.fsPath)} (HEAD ↔ agent edit)`;
    await vscode.commands.executeCommand("vscode.diff", head, uri, title, {
      preview: true,
      preserveFocus: true,
      selection: selectionFor(line),
    } satisfies vscode.TextDocumentShowOptions);
  }

  private prune(): void {
    if (this.calls.size < 256) return;
    const cutoff = Date.now() - TRACK_TTL_MS;
    for (const [k, v] of this.calls) if (v.seenAt < cutoff) this.calls.delete(k);
    for (const [k, t] of this.lastOpened) if (t < cutoff) this.lastOpened.delete(k);
  }

  dispose(): void {
    for (const d of this.disposables) d.dispose();
    this.calls.clear();
  }
}

/** ACP tool locations use 1-based lines. */
function selectionFor(line: number | undefined): vscode.Range | undefined {
  if (typeof line !== "number" || !Number.isFinite(line) || line < 1) return undefined;
  const l = Math.floor(line) - 1;
  return new vscode.Range(l, 0, l, 0);
}
