import * as vscode from "vscode";
import * as path from "node:path";
import { SPECIES_META } from "@ai-pet/pet-engine";
import type { Agent, PermissionRequest, Pet, Session, Species } from "@ai-pet/protocol";
import { HubOfflineError, type HubConnection } from "./connection";
import { installHooks, startHub, type HookTargetId } from "./hubProcess";
import type { PetWebviews } from "./petView";
import { agentName, errorMessage, isBusy, isInWorkspace, isOpen, relativeTime, sessionLabel } from "./util";

const LAST_AGENT_KEY = "ai-pet.lastAgentId";
const LAST_CWD_KEY = "ai-pet.lastCwd";

type SessionItem = vscode.QuickPickItem & { session: Session };
type RequestItem = vscode.QuickPickItem & { request: PermissionRequest };

export class Commands {
  constructor(
    private readonly context: vscode.ExtensionContext,
    private readonly hub: HubConnection,
    private readonly views: PetWebviews,
    private readonly log: vscode.LogOutputChannel,
  ) {}

  register(): vscode.Disposable[] {
    const reg = (id: string, fn: (...args: any[]) => Promise<unknown> | unknown) =>
      vscode.commands.registerCommand(id, async (...args: unknown[]) => {
        try {
          await fn(...args);
        } catch (err) {
          this.reportError(err);
        }
      });
    return [
      reg("ai-pet.startSession", (agentId?: string, cwd?: string, prompt?: string) => this.startSession(agentId, cwd, prompt)),
      reg("ai-pet.promptSession", (sessionId?: string, text?: string) => this.promptSession(sessionId, text)),
      reg("ai-pet.cancelSession", (sessionId?: string) => this.cancelSession(sessionId)),
      reg("ai-pet.approvePending", (requestId?: string) => this.resolvePending(true, requestId)),
      reg("ai-pet.denyPending", (requestId?: string) => this.resolvePending(false, requestId)),
      reg("ai-pet.showPet", (sessionId?: string) => this.showPet(sessionId)),
      reg("ai-pet.openPetPanel", () => this.views.openPanel()),
      reg("ai-pet.changeSpecies", (petId?: string, species?: Species) => this.changeSpecies(petId, species)),
      reg("ai-pet.installHooks", (target?: HookTargetId) => installHooks(this.log, target)),
      reg("ai-pet.startHub", () => startHub(this.hub, this.log)),
      reg("ai-pet.reconnect", () => this.hub.restart()),
    ];
  }

  reportError(err: unknown): void {
    if (err instanceof HubOfflineError) {
      void vscode.window.showWarningMessage(err.message, "Start Hub").then((pick) => {
        if (pick === "Start Hub") void vscode.commands.executeCommand("ai-pet.startHub");
      });
      return;
    }
    this.log.error(errorMessage(err));
    void vscode.window.showErrorMessage(`AI Pet: ${errorMessage(err)}`);
  }

  async showPet(sessionId?: string): Promise<void> {
    await this.views.revealView();
    if (typeof sessionId === "string" && sessionId) this.views.focusSession(sessionId);
  }

  // ---------------- sessions ----------------

  async startSession(agentId?: string, cwd?: string, prompt?: string): Promise<void> {
    const api = this.hub.api;
    const agent = typeof agentId === "string" && agentId ? this.agentById(agentId) : await this.pickAgent();
    if (!agent) return;
    const dir = typeof cwd === "string" && cwd ? cwd : await this.pickCwd();
    if (!dir) return;
    let text = typeof prompt === "string" ? prompt : undefined;
    if (text === undefined) {
      text = await vscode.window.showInputBox({
        title: `AI Pet: Start ${agent.name} in ${path.basename(dir) || dir}`,
        prompt: "What should the agent do? (leave empty to start an idle session)",
        placeHolder: "e.g. run the tests and fix what fails",
        ignoreFocusOut: true,
      });
      if (text === undefined) return; // Esc
    }
    await this.context.globalState.update(LAST_AGENT_KEY, agent.id);
    await this.context.globalState.update(LAST_CWD_KEY, dir);
    const session = await vscode.window.withProgress(
      { location: vscode.ProgressLocation.Notification, title: `AI Pet: starting ${agent.name}…` },
      () => api.startSession(agent.id, dir, text?.trim() ? text.trim() : undefined),
    );
    this.log.info(`started session ${session.id} (${agent.id}) in ${dir}`);
    await this.showPet(session.id);
  }

  private agentById(id: string): Agent {
    const agent = this.hub.state.agents.find((a) => a.id === id);
    if (!agent) throw new Error(`unknown agent “${id}”`);
    return agent;
  }

  private async pickAgent(): Promise<Agent | undefined> {
    const agents = [...this.hub.state.agents];
    if (agents.length === 0) {
      const refreshed = await this.hub.api.listAgents(true);
      agents.push(...refreshed);
    }
    if (agents.length === 0) {
      void vscode.window.showWarningMessage("AI Pet: the hub found no agents. Install an ACP agent (e.g. Claude Code or Codex) and try again.");
      return undefined;
    }
    const preferred =
      vscode.workspace.getConfiguration("ai-pet").get<string>("defaultAgent")?.trim() ||
      this.context.globalState.get<string>(LAST_AGENT_KEY);
    agents.sort((a, b) => {
      if (a.id === preferred) return -1;
      if (b.id === preferred) return 1;
      if (a.installed !== b.installed) return a.installed ? -1 : 1;
      return a.name.localeCompare(b.name);
    });
    const pets = Object.values(this.hub.state.pets);
    const items: Array<vscode.QuickPickItem & { agent: Agent }> = agents.map((agent) => {
      const pet = pets.find((p) => p.agentId === agent.id);
      return {
        agent,
        label: `${pet ? SPECIES_META[pet.species].emoji + " " : ""}${agent.name}`,
        description: [agent.version, agent.installed ? undefined : "not installed (will try npx)"].filter(Boolean).join(" · "),
        detail: agent.description,
      };
    });
    const pick = await vscode.window.showQuickPick(items, {
      title: "AI Pet: Start Session",
      placeHolder: "Which agent?",
      matchOnDescription: true,
      matchOnDetail: true,
    });
    return pick?.agent;
  }

  private async pickCwd(): Promise<string | undefined> {
    const folders = (vscode.workspace.workspaceFolders ?? []).filter((f) => f.uri.scheme === "file");
    if (folders.length === 1) return folders[0].uri.fsPath;
    if (folders.length > 1) {
      const pick = await vscode.window.showWorkspaceFolderPick({ placeHolder: "Working directory for the agent" });
      return pick?.uri.fsPath;
    }
    const last = this.context.globalState.get<string>(LAST_CWD_KEY);
    const chosen = await vscode.window.showOpenDialog({
      canSelectFiles: false,
      canSelectFolders: true,
      canSelectMany: false,
      openLabel: "Use as working directory",
      title: "AI Pet: working directory for the agent",
      defaultUri: last ? vscode.Uri.file(last) : undefined,
    });
    return chosen?.[0]?.fsPath;
  }

  private sessionItems(filter: (s: Session) => boolean): SessionItem[] {
    const state = this.hub.state;
    const now = Date.now();
    return Object.values(state.sessions)
      .filter(filter)
      .sort((a, b) => {
        const wa = isInWorkspace(a.cwd) ? 1 : 0;
        const wb = isInWorkspace(b.cwd) ? 1 : 0;
        if (wa !== wb) return wb - wa;
        return b.lastEventAt - a.lastEventAt;
      })
      .map((session) => ({
        session,
        label: sessionLabel(session, state.agents),
        description: `${session.status.replace("_", " ")} · ${relativeTime(session.lastEventAt, now)}`,
        detail: session.cwd,
      }));
  }

  private async pickSession(title: string, filter: (s: Session) => boolean, empty: string): Promise<Session | undefined> {
    const items = this.sessionItems(filter);
    if (items.length === 0) {
      void vscode.window.showInformationMessage(`AI Pet: ${empty}`);
      return undefined;
    }
    if (items.length === 1) return items[0].session;
    const pick = await vscode.window.showQuickPick(items, { title, placeHolder: "Which session?", matchOnDetail: true });
    return pick?.session;
  }

  async promptSession(sessionId?: string, text?: string): Promise<void> {
    const api = this.hub.api;
    const session =
      typeof sessionId === "string" && sessionId
        ? this.hub.state.sessions[sessionId]
        : await this.pickSession(
            "AI Pet: Prompt Session",
            (s) => s.source === "owned" && isOpen(s),
            "no open sessions started by the pet. Use “AI Pet: Start Session” first.",
          );
    if (!session) return;
    if (session.source !== "owned") {
      throw new Error("only sessions started by the pet can be prompted (this one runs in another tool)");
    }
    let message = typeof text === "string" ? text : undefined;
    if (message === undefined) {
      message = await vscode.window.showInputBox({
        title: `AI Pet: Prompt ${sessionLabel(session, this.hub.state.agents)}`,
        prompt: isBusy(session) ? "The agent is busy; your message is sent as the next prompt." : undefined,
        placeHolder: "Message for the agent",
        ignoreFocusOut: true,
      });
    }
    if (!message?.trim()) return;
    await api.prompt(session.id, message.trim());
  }

  async cancelSession(sessionId?: string): Promise<void> {
    const api = this.hub.api;
    const session =
      typeof sessionId === "string" && sessionId
        ? this.hub.state.sessions[sessionId]
        : await this.pickSession("AI Pet: Cancel Session", (s) => s.source === "owned" && isBusy(s), "nothing to cancel — no busy sessions.");
    if (!session) return;
    await api.cancel(session.id);
    void vscode.window.setStatusBarMessage(`AI Pet: cancelled ${agentName(this.hub.state.agents, session.agentId)}`, 3000);
  }

  // ---------------- approvals ----------------

  async resolvePending(approve: boolean, requestId?: string): Promise<void> {
    const api = this.hub.api;
    const state = this.hub.state;
    let req: PermissionRequest | undefined =
      typeof requestId === "string" && requestId ? state.pending[requestId] : undefined;
    if (!req) {
      const pending = Object.values(state.pending).sort((a, b) => a.createdAt - b.createdAt);
      if (pending.length === 0) {
        void vscode.window.showInformationMessage("AI Pet: nothing is waiting for approval.");
        return;
      }
      if (pending.length === 1) req = pending[0];
      else {
        const items: RequestItem[] = pending.map((request) => ({
          request,
          label: `${agentName(state.agents, request.agentId)}: ${request.toolCall.title || request.toolCall.kind || "tool call"}`,
          description: relativeTime(request.createdAt),
          detail: request.toolCall.locations?.map((l) => l.path).join(", "),
        }));
        const pick = await vscode.window.showQuickPick(items, {
          title: approve ? "AI Pet: Approve Pending" : "AI Pet: Deny Pending",
          placeHolder: approve ? "Approve which request?" : "Deny which request?",
          matchOnDetail: true,
        });
        req = pick?.request;
      }
    }
    if (!req) return;
    if (approve) await api.approve(req.requestId);
    else await api.deny(req.requestId);
  }

  // ---------------- pets ----------------

  async changeSpecies(petId?: string, species?: Species): Promise<void> {
    const api = this.hub.api;
    const state = this.hub.state;
    const pets = Object.values(state.pets);
    let pet: Pet | undefined = typeof petId === "string" && petId ? state.pets[petId] : undefined;
    if (!pet) {
      if (pets.length === 0) {
        void vscode.window.showInformationMessage("AI Pet: no pets yet — each agent gets one when the hub discovers it.");
        return;
      }
      if (pets.length === 1) pet = pets[0];
      else {
        const pick = await vscode.window.showQuickPick(
          pets.map((p) => ({
            pet: p,
            label: `${SPECIES_META[p.species].emoji} ${p.name}`,
            description: agentName(state.agents, p.agentId),
          })),
          { title: "AI Pet: Change Pet Species", placeHolder: "Which pet?" },
        );
        pet = pick?.pet;
      }
    }
    if (!pet) return;
    const chosen = pet;
    let next = species && species in SPECIES_META ? species : undefined;
    if (!next) {
      const pick = await vscode.window.showQuickPick(
        (Object.keys(SPECIES_META) as Species[]).map((s) => ({
          species: s,
          label: `${SPECIES_META[s].emoji} ${SPECIES_META[s].label}`,
          description: s === chosen.species ? "current" : undefined,
        })),
        { title: `AI Pet: ${chosen.name} should be a…`, placeHolder: "Pick a species" },
      );
      next = pick?.species;
    }
    if (!next || next === chosen.species) return;
    const updated = await api.updatePet(chosen.id, { species: next });
    void vscode.window.setStatusBarMessage(`AI Pet: ${updated.name} is now a ${SPECIES_META[updated.species].label.toLowerCase()} ${SPECIES_META[updated.species].emoji}`, 4000);
  }
}
