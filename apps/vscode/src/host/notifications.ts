import * as vscode from "vscode";
import { describePermission, SPECIES_META } from "@krypto/pet-engine";
import {
  DEFAULT_SPECIES,
  type HubEvent,
  type HubNotification,
  type PermissionRequest,
  type ServerMsg,
  type StopPayload,
  type ErrorPayload,
} from "@krypto/protocol";
import type { HubConnection } from "./connection";
import { answerRequest } from "./remote";
import { activeMachineId, isLocal, machineDisplayName, petFor } from "@krypto/hub-client";
import { agentName, errorMessage, isInWorkspace } from "./util";

type NotifyLevel = "all" | "permissions" | "off";

/** A hub `notification` about a session we just notified for (e.g. turn complete) is a duplicate. */
const DEDUPE_MS = 5000;
/** Events older than this are history (a mirror sync after a reconnect), not news. */
const FRESH_MS = 2 * 60_000;

export interface NotificationActions {
  showPet(sessionId?: string): Promise<void>;
  promptSession(sessionId?: string): Promise<void>;
}

export class Notifier implements vscode.Disposable {
  private readonly disposables: vscode.Disposable[] = [];
  private readonly recent = new Map<string, number>();
  private readonly shownRequests = new Set<string>();

  constructor(
    private readonly hub: HubConnection,
    private readonly actions: NotificationActions,
    private readonly log: vscode.LogOutputChannel,
  ) {
    this.disposables.push(hub.onMessage((msg) => this.onMessage(msg)));
  }

  private get level(): NotifyLevel {
    const v = vscode.workspace.getConfiguration("krypto").get<string>("notifications");
    return v === "off" || v === "permissions" ? v : "all";
  }

  private onMessage(msg: ServerMsg): void {
    try {
      switch (msg.type) {
        case "permission_request":
          this.onPermission(msg.request);
          break;
        case "permission_resolved":
          this.shownRequests.delete(msg.requestId);
          break;
        case "event":
          this.onEvent(msg.event);
          break;
        case "notification":
          this.onHubNotification(msg.notification);
          break;
        default:
          break;
      }
    } catch (err) {
      this.log.error(`notification failed: ${errorMessage(err)}`);
    }
  }

  /** Should this window speak up about the session? (Every VS Code window runs its own copy of the extension.) */
  private relevant(sessionId: string | undefined): boolean {
    const only = vscode.workspace.getConfiguration("krypto").get<boolean>("notifyOnlyWorkspaceSessions") === true;
    const session = sessionId ? this.hub.state.sessions[sessionId] : undefined;
    const inWorkspace = session ? isInWorkspace(session.cwd) : false;
    if (only) return inWorkspace;
    return inWorkspace || vscode.window.state.focused;
  }

  /**
   * Every machine's events notify, but only on the machine the user is active on, so they are told
   * once, where they are (PLAN.md migration step 3).
   */
  private here(): boolean {
    const state = this.hub.state;
    const own = state.machine?.id;
    return !own || activeMachineId(state, Date.now()) === own;
  }

  /** " on Mac mini" for another machine's item. */
  private where(machineId: string): string {
    const state = this.hub.state;
    return isLocal(state, machineId) ? "" : ` on ${machineDisplayName(state.machines[machineId])}`;
  }

  private who(machineId: string, agentId: string): string {
    const state = this.hub.state;
    const pet = petFor(Object.values(state.pets), machineId, agentId);
    const emoji = SPECIES_META[pet?.species ?? DEFAULT_SPECIES].emoji;
    const agent = agentName(state.agents, agentId, machineId);
    return `${pet ? `${emoji} ${pet.name} (${agent})` : `${emoji} ${agent}`}${this.where(machineId)}`;
  }

  private onPermission(req: PermissionRequest): void {
    if (this.level === "off" || !this.here()) return;
    const session = this.hub.state.sessions[req.sessionId];
    if (session && session.source !== "owned") return; // approvals for vendor-extension sessions stay in VS Code
    if (!this.relevant(req.sessionId)) return;
    if (this.shownRequests.has(req.requestId)) return;
    this.shownRequests.add(req.requestId);
    this.markRecent(req.sessionId, "perm");

    const text = `${this.who(req.machineId, req.agentId)} needs your OK: ${describePermission(req)}`;
    // Another machine's request is forwarded to it; approving asks in a modal first (answerRequest).
    const actions = ["Approve", "Deny", "Show"];
    void vscode.window.showWarningMessage(text, ...actions).then(async (pick) => {
      this.shownRequests.delete(req.requestId);
      if (!pick) return;
      if (pick === "Show") return this.actions.showPet(req.sessionId);
      await this.resolve(req.requestId, pick === "Approve");
    });
  }

  private async resolve(requestId: string, approve: boolean): Promise<void> {
    if (!this.hub.connected) {
      void vscode.window.showWarningMessage("Krypto: the hub is offline; the request could not be answered.");
      return;
    }
    if (!this.hub.state.pending[requestId]) {
      void vscode.window.showInformationMessage("Krypto: that request was already answered.");
      return;
    }
    try {
      await answerRequest(this.hub, requestId, { kind: approve ? "approve" : "deny" });
    } catch (err) {
      void vscode.window.showErrorMessage(`Krypto: could not ${approve ? "approve" : "deny"}: ${errorMessage(err)}`);
    }
  }

  private onEvent(e: HubEvent): void {
    if (this.level !== "all") return;
    if (e.type !== "stop" && e.type !== "error") return;
    if (Date.now() - e.ts > FRESH_MS || !this.here()) return;
    const session = this.hub.state.sessions[e.sessionId];
    if (!session || session.source !== "owned") return; // hook sessions are covered by hub notifications
    if (!this.relevant(e.sessionId)) return;
    const who = this.who(session.machineId, session.agentId);
    const title = session.title?.trim();
    // Replying to another machine's session is forwarded to it (migration step 4).
    const replyActions = isLocal(this.hub.state, session.machineId) || this.hub.state.machines[session.machineId]?.online ? ["Reply", "Show"] : ["Show"];

    if (e.type === "error") {
      const message = (e.payload as Partial<ErrorPayload> | null)?.message ?? "unknown error";
      if (this.isRecent(e.sessionId, "turn")) return;
      this.markRecent(e.sessionId, "turn");
      void vscode.window.showWarningMessage(`${who} hit an error: ${message}`, "Show").then((pick) => {
        if (pick === "Show") void this.actions.showPet(e.sessionId);
      });
      return;
    }

    const reason = (e.payload as Partial<StopPayload> | null)?.stopReason;
    if (reason === "cancelled") return;
    if (this.isRecent(e.sessionId, "turn")) return;
    this.markRecent(e.sessionId, "turn");
    if (reason === "end_turn" || reason === undefined) {
      const text = `${who} finished${title ? `: ${title}` : ""}.`;
      void vscode.window.showInformationMessage(text, ...replyActions).then((pick) => {
        if (pick === "Reply") void this.actions.promptSession(e.sessionId);
        else if (pick === "Show") void this.actions.showPet(e.sessionId);
      });
      return;
    }
    const why =
      reason === "refusal" ? "refused to continue" : reason === "max_tokens" ? "ran out of tokens" : "hit its turn limit";
    void vscode.window.showWarningMessage(`${who} stopped: ${why}.`, ...replyActions).then((pick) => {
      if (pick === "Reply") void this.actions.promptSession(e.sessionId);
      else if (pick === "Show") void this.actions.showPet(e.sessionId);
    });
  }

  private onHubNotification(n: HubNotification): void {
    const level = this.level;
    if (level === "off" || Date.now() - n.ts > FRESH_MS || !this.here()) return;
    const pending = n.sessionId
      ? Object.values(this.hub.state.pending).filter((p) => p.sessionId === n.sessionId)
      : [];
    // "permissions" mode still lets watchdog nudges about waiting approvals through.
    if (level === "permissions" && pending.length === 0) return;
    if (!this.relevant(n.sessionId)) return;
    if (n.sessionId) {
      const kind = pending.length > 0 ? "perm" : "turn";
      if (this.isRecent(n.sessionId, kind)) return;
      this.markRecent(n.sessionId, kind);
    }

    const title = `${n.title}${this.where(n.machineId)}`;
    const text = n.title && n.body ? `${title}: ${n.body}` : n.title ? title : n.body;
    const answerable = pending.length === 1;
    const actions = answerable ? ["Approve", "Deny", "Show"] : n.sessionId ? ["Show"] : [];
    const show =
      n.level === "alert" || n.level === "warning"
        ? vscode.window.showWarningMessage
        : vscode.window.showInformationMessage;
    void show(`🐾 ${text}`, ...actions).then(async (pick) => {
      if (!pick) return;
      if (pick === "Show") return this.actions.showPet(n.sessionId);
      const req = pending[0];
      if (req) await this.resolve(req.requestId, pick === "Approve");
    });
  }

  private isRecent(sessionId: string, kind: "perm" | "turn"): boolean {
    return Date.now() - (this.recent.get(`${sessionId}:${kind}`) ?? 0) < DEDUPE_MS;
  }

  private markRecent(sessionId: string, kind: "perm" | "turn"): void {
    this.recent.set(`${sessionId}:${kind}`, Date.now());
    this.pruneRecent();
  }

  private pruneRecent(): void {
    if (this.recent.size < 128) return;
    const cutoff = Date.now() - DEDUPE_MS;
    for (const [k, t] of this.recent) if (t < cutoff) this.recent.delete(k);
  }

  dispose(): void {
    for (const d of this.disposables) d.dispose();
  }
}
