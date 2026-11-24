import * as vscode from "vscode";
import * as path from "node:path";
import {
  checkPetName,
  isLocal,
  MACHINE_NAME_MAX,
  machineDisplayName,
  normalizeMachineName,
  OS_LABEL,
  petFor,
  petRenameErrorText,
  readOnlyReason,
  renameErrorText,
  sortedMachines,
  startNeedsConfirmation,
} from "@krypto/hub-client";
import { SPECIES_META } from "@krypto/pet-engine";
import type { Agent, Machine, PermissionRequest, Pet, Session, Species } from "@krypto/protocol";
import { HubOfflineError, type HubConnection } from "./connection";
import { installHooks, startHub, type HookTargetId } from "./hubProcess";
import { machinePickItems } from "./machineList";
import { answerRequest, confirmModal, remoteError } from "./remote";
import { machineSuffix, openSettingsErrorText, remoteStartText } from "./remoteText";
import type { PetWebviews } from "./petView";
import { agentName, errorMessage, isBusy, isInWorkspace, isOpen, relativeTime, sessionLabel } from "./util";

const LAST_AGENT_KEY = "krypto.lastAgentId";
const LAST_CWD_KEY = "krypto.lastCwd";

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
      reg("krypto.startSession", (agentId?: string, cwd?: string, prompt?: string, machineId?: string) =>
        this.startSession(agentId, cwd, prompt, machineId),
      ),
      reg("krypto.promptSession", (sessionId?: string, text?: string) => this.promptSession(sessionId, text)),
      reg("krypto.cancelSession", (sessionId?: string) => this.cancelSession(sessionId)),
      reg("krypto.approvePending", (requestId?: string) => this.resolvePending(true, requestId)),
      reg("krypto.denyPending", (requestId?: string) => this.resolvePending(false, requestId)),
      reg("krypto.showPet", (sessionId?: string) => this.showPet(sessionId)),
      reg("krypto.openPetPanel", () => this.views.openPanel()),
      reg("krypto.changeSpecies", (petId?: string, species?: Species) => this.changeSpecies(petId, species)),
      reg("krypto.renamePet", (petId?: string, name?: string) => this.renamePet(petId, name)),
      reg("krypto.installHooks", (target?: HookTargetId) => installHooks(this.log, target)),
      reg("krypto.startHub", () => startHub(this.hub, this.log)),
      reg("krypto.reconnect", () => this.hub.restart()),
      reg("krypto.renameMachine", (name?: string) => this.renameMachine(name)),
      reg("krypto.showMachines", () => this.showMachines()),
      reg("krypto.openSettings", () => this.openSettings()),
    ];
  }

  reportError(err: unknown): void {
    if (err instanceof HubOfflineError) {
      void vscode.window.showWarningMessage(err.message, "Start Hub").then((pick) => {
        if (pick === "Start Hub") void vscode.commands.executeCommand("krypto.startHub");
      });
      return;
    }
    this.log.error(errorMessage(err));
    void vscode.window.showErrorMessage(`Krypto: ${errorMessage(err)}`);
  }

  async showPet(sessionId?: string): Promise<void> {
    await this.views.revealView();
    if (typeof sessionId === "string" && sessionId) this.views.focusSession(sessionId);
  }

  // ---------------- sessions ----------------

  /**
   * Start a session on this machine or, with the machine picker, on an online paired machine (PLAN.md
   * migration step 4): its agents come from its presence, the folder from its recent sessions (or typed), and
   * a modal asks "Start Claude Code on Mac mini in /repo?" before anything runs there.
   */
  async startSession(agentId?: string, cwd?: string, prompt?: string, machineId?: string): Promise<void> {
    const api = this.hub.api;
    const target = typeof machineId === "string" && machineId ? machineId : await this.pickMachine();
    if (!target) return;
    const remote = startNeedsConfirmation(this.hub.state, target);
    const where = remote ? ` on ${machineDisplayName(this.hub.state.machines[target])}` : "";
    const agents = remote ? await api.listAgents({ machineId: target }).catch((err) => Promise.reject(remoteError(this.hub, target, "start", err))) : undefined;
    const agent =
      typeof agentId === "string" && agentId ? this.agentById(agentId, agents) : await this.pickAgent(agents, where);
    if (!agent) return;
    const dir = typeof cwd === "string" && cwd ? cwd : remote ? await this.pickRemoteCwd(target) : await this.pickCwd();
    if (!dir) return;
    let text = typeof prompt === "string" ? prompt : undefined;
    if (text === undefined) {
      text = await vscode.window.showInputBox({
        title: `Krypto: Start ${agent.name}${where} in ${path.basename(dir) || dir}`,
        prompt: "What should the agent do? (leave empty to start an idle session)",
        placeHolder: "e.g. run the tests and fix what fails",
        ignoreFocusOut: true,
      });
      if (text === undefined) return; // Esc
    }
    // Starting on another machine runs code there: confirm on this device first.
    if (remote && !(await confirmModal(remoteStartText(this.hub.state, target, agent, dir)))) return;
    if (!remote) {
      await this.context.globalState.update(LAST_AGENT_KEY, agent.id);
      await this.context.globalState.update(LAST_CWD_KEY, dir);
    }
    const session = await vscode.window.withProgress(
      { location: vscode.ProgressLocation.Notification, title: `Krypto: starting ${agent.name}${where}…` },
      () =>
        api
          .startSession({
            agentId: agent.id,
            cwd: dir,
            prompt: text?.trim() ? text.trim() : undefined,
            machineId: remote ? target : undefined,
            confirmed: remote ? true : undefined,
          })
          .catch((err) => Promise.reject(remoteError(this.hub, target, "start", err))),
    );
    this.log.info(`started session ${session.id} (${agent.id})${remote ? ` on ${target}` : ""}`);
    await this.showPet(session.id);
  }

  /** This machine, or a QuickPick of it and the online paired machines. Undefined on Esc. */
  private async pickMachine(): Promise<string | undefined> {
    const state = this.hub.state;
    const own = state.machine?.id ?? "";
    const machines = sortedMachines(state).filter((m) => m.id === own || m.online);
    if (machines.length <= 1) return own;
    const pick = await vscode.window.showQuickPick(
      machines.map((m) => ({
        id: m.id,
        label: `$(device-desktop) ${machineDisplayName(m)}`,
        description: m.id === own ? "this machine" : OS_LABEL[m.os] ?? m.os,
      })),
      { title: "Krypto: Start Session", placeHolder: "Where should the agent run?" },
    );
    return pick?.id;
  }

  /** A folder on another machine: one of its repos (its repo index, `list_repos`), or one typed in. */
  private async pickRemoteCwd(machineId: string): Promise<string | undefined> {
    const name = machineDisplayName(this.hub.state.machines[machineId]);
    const repos = await this.hub.api.listRepos({ machineId }).catch((err) => Promise.reject(remoteError(this.hub, machineId, "start", err)));
    const TYPE = `$(edit) Enter another folder ${name} already uses…`;
    if (repos.length > 0) {
      const pick = await vscode.window.showQuickPick(
        [...repos.map((r) => ({ label: `$(repo) ${r.name}`, description: r.path, path: r.path })), { label: TYPE, description: "", path: "" }],
        { title: `Krypto: working folder on ${name}`, placeHolder: `Only folders ${name} already uses`, matchOnDescription: true },
      );
      if (!pick) return undefined;
      if (pick.path) return pick.path;
    }
    const typed = await vscode.window.showInputBox({
      title: `Krypto: working folder on ${name}`,
      prompt: `Only folders ${name} already uses (absolute path); it refuses others.`,
      placeHolder: "/home/me/code/project or C:\\code\\project",
      ignoreFocusOut: true,
      validateInput: (v) => (v.trim() ? null : "Enter a folder path."),
    });
    return typed?.trim() || undefined;
  }

  private agentById(id: string, agents: Agent[] = this.hub.state.agents.filter((a) => isLocal(this.hub.state, a.machineId))): Agent {
    const agent = agents.find((a) => a.id === id);
    if (!agent) throw new Error(`unknown agent “${id}”`);
    return agent;
  }

  /** Pick one of `remoteAgents` (another machine's), else of this machine's agents. */
  private async pickAgent(remoteAgents?: Agent[], where = ""): Promise<Agent | undefined> {
    // Sessions start where the agent is installed: this machine's agents, or the chosen machine's.
    const agents = remoteAgents ? [...remoteAgents] : this.hub.state.agents.filter((a) => isLocal(this.hub.state, a.machineId));
    if (agents.length === 0 && !remoteAgents) {
      const refreshed = await this.hub.api.listAgents(true);
      agents.push(...refreshed.filter((a) => isLocal(this.hub.state, a.machineId)));
    }
    if (agents.length === 0) {
      void vscode.window.showWarningMessage(
        remoteAgents
          ? `Krypto: no agents reported${where}.`
          : "Krypto: the hub found no agents. Install an ACP agent (e.g. Claude Code or Codex) and try again.",
      );
      return undefined;
    }
    const preferred =
      vscode.workspace.getConfiguration("krypto").get<string>("defaultAgent")?.trim() ||
      this.context.globalState.get<string>(LAST_AGENT_KEY);
    agents.sort((a, b) => {
      if (a.id === preferred) return -1;
      if (b.id === preferred) return 1;
      if (a.installed !== b.installed) return a.installed ? -1 : 1;
      return a.name.localeCompare(b.name);
    });
    const pets = Object.values(this.hub.state.pets);
    const items: Array<vscode.QuickPickItem & { agent: Agent }> = agents.map((agent) => {
      const pet = petFor(pets, agent.machineId, agent.id);
      return {
        agent,
        label: `${pet ? SPECIES_META[pet.species].emoji + " " : ""}${agent.name}`,
        description: [pet ? `${pet.name} watches` : undefined, agent.version, agent.installed ? undefined : "not installed (will try npx)"]
          .filter(Boolean)
          .join(" · "),
        detail: agent.description,
      };
    });
    const pick = await vscode.window.showQuickPick(items, {
      title: `Krypto: Start Session${where}`,
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
    // No workspace folder: this machine's repos first, then a folder dialog.
    const repos = await this.hub.api.listRepos().catch(() => []);
    if (repos.length > 0) {
      const BROWSE = "$(folder-opened) Browse…";
      const pick = await vscode.window.showQuickPick(
        [...repos.map((r) => ({ label: `$(repo) ${r.name}`, description: r.path, path: r.path })), { label: BROWSE, description: "", path: "" }],
        { title: "Krypto: working directory for the agent", placeHolder: "Pick a repo, or browse for a folder", matchOnDescription: true },
      );
      if (!pick) return undefined;
      if (pick.path) return pick.path;
    }
    const last = this.context.globalState.get<string>(LAST_CWD_KEY);
    const chosen = await vscode.window.showOpenDialog({
      canSelectFiles: false,
      canSelectFolders: true,
      canSelectMany: false,
      openLabel: "Use as working directory",
      title: "Krypto: working directory for the agent",
      defaultUri: last ? vscode.Uri.file(last) : undefined,
    });
    return chosen?.[0]?.fsPath;
  }

  /** Another machine's pets belong to its hub (sessions and approvals can be acted on since step 4). */
  private assertLocal(machineId: string): void {
    const why = readOnlyReason(this.hub.state, machineId);
    if (why) throw new Error(why);
  }

  /** Sessions of every machine matching `filter` (another machine's are forwarded to it). */
  private sessionItems(filter: (s: Session) => boolean): SessionItem[] {
    const state = this.hub.state;
    const now = Date.now();
    return Object.values(state.sessions)
      .filter((s) => filter(s) && (isLocal(state, s.machineId) || state.machines[s.machineId]?.online === true))
      .sort((a, b) => {
        const wa = isInWorkspace(a.cwd) ? 1 : 0;
        const wb = isInWorkspace(b.cwd) ? 1 : 0;
        if (wa !== wb) return wb - wa;
        return b.lastEventAt - a.lastEventAt;
      })
      .map((session) => ({
        session,
        label: sessionLabel(session, state.agents),
        description: `${session.status.replace("_", " ")} · ${relativeTime(session.lastEventAt, now)}${machineSuffix(state, session.machineId)}`,
        detail: session.cwd,
      }));
  }

  private async pickSession(title: string, filter: (s: Session) => boolean, empty: string): Promise<Session | undefined> {
    const items = this.sessionItems(filter);
    if (items.length === 0) {
      void vscode.window.showInformationMessage(`Krypto: ${empty}`);
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
            "Krypto: Prompt Session",
            (s) => s.source === "owned" && isOpen(s),
            "no open sessions started by the pet. Use “Krypto: Start Session” first.",
          );
    if (!session) return;
    if (session.source !== "owned") {
      throw new Error("only sessions started by the pet can be prompted (this one runs in another tool)");
    }
    let message = typeof text === "string" ? text : undefined;
    if (message === undefined) {
      message = await vscode.window.showInputBox({
        title: `Krypto: Prompt ${sessionLabel(session, this.hub.state.agents)}`,
        prompt: isBusy(session) ? "The agent is busy; your message is sent as the next prompt." : undefined,
        placeHolder: "Message for the agent",
        ignoreFocusOut: true,
      });
    }
    if (!message?.trim()) return;
    await api.prompt(session.id, message.trim()).catch((err) => Promise.reject(remoteError(this.hub, session.machineId, "prompt", err)));
  }

  async cancelSession(sessionId?: string): Promise<void> {
    const api = this.hub.api;
    const session =
      typeof sessionId === "string" && sessionId
        ? this.hub.state.sessions[sessionId]
        : await this.pickSession("Krypto: Cancel Session", (s) => s.source === "owned" && isBusy(s), "nothing to cancel — no busy sessions.");
    if (!session) return;
    await api.cancel(session.id).catch((err) => Promise.reject(remoteError(this.hub, session.machineId, "cancel", err)));
    void vscode.window.setStatusBarMessage(
      `Krypto: cancelled ${agentName(this.hub.state.agents, session.agentId, session.machineId)}`,
      3000,
    );
  }

  // ---------------- approvals ----------------

  /** Approve or deny a pending request of any machine (another machine's approval asks in a modal first). */
  async resolvePending(approve: boolean, requestId?: string): Promise<void> {
    void this.hub.api; // throws HubOfflineError while offline
    const state = this.hub.state;
    let req: PermissionRequest | undefined =
      typeof requestId === "string" && requestId ? state.pending[requestId] : undefined;
    if (!req) {
      // This machine's requests first, then other (online) machines'.
      const pending = Object.values(state.pending)
        .filter((p) => isLocal(state, p.machineId) || state.machines[p.machineId]?.online === true)
        .sort((a, b) => Number(isLocal(state, b.machineId)) - Number(isLocal(state, a.machineId)) || a.createdAt - b.createdAt);
      if (pending.length === 0) {
        void vscode.window.showInformationMessage("Krypto: nothing is waiting for approval.");
        return;
      }
      if (pending.length === 1) req = pending[0];
      else {
        const items: RequestItem[] = pending.map((request) => ({
          request,
          label: `${agentName(state.agents, request.agentId, request.machineId)}: ${request.toolCall.title || request.toolCall.kind || "tool call"}`,
          description: `${relativeTime(request.createdAt)}${machineSuffix(state, request.machineId)}`,
          detail: request.toolCall.locations?.map((l) => l.path).join(", "),
        }));
        const pick = await vscode.window.showQuickPick(items, {
          title: approve ? "Krypto: Approve Pending" : "Krypto: Deny Pending",
          placeHolder: approve ? "Approve which request?" : "Deny which request?",
          matchOnDetail: true,
        });
        req = pick?.request;
      }
    }
    if (!req) return;
    await answerRequest(this.hub, req.requestId, { kind: approve ? "approve" : "deny" });
  }

  // ---------------- machine ----------------

  /** Rename this hub's machine (input box unless `name` is given, e.g. from a keybinding's args). */
  async renameMachine(name?: string): Promise<void> {
    const api = this.hub.api;
    const machine = this.hub.state.machine;
    if (!machine) throw new Error("the hub has not introduced this machine yet; try again in a moment");
    let next = typeof name === "string" ? name : undefined;
    if (next === undefined) {
      next = await vscode.window.showInputBox({
        title: "Krypto: Rename This Machine",
        prompt: `Shown above this machine's pets on every paired screen. ${OS_LABEL[machine.os] ?? machine.os} · id ${machine.id}`,
        value: machine.name,
        valueSelection: [0, machine.name.length],
        placeHolder: "e.g. Windows PC",
        ignoreFocusOut: true,
        validateInput: (v) => (normalizeMachineName(v) === null ? `Use 1–${MACHINE_NAME_MAX} characters.` : null),
      });
      if (next === undefined) return; // Esc
    }
    const trimmed = normalizeMachineName(next);
    if (trimmed === null) throw new Error(`the machine name must be 1–${MACHINE_NAME_MAX} characters`);
    if (trimmed === machine.name) return;
    let updated: Machine;
    try {
      updated = await api.updateMachine(trimmed);
    } catch (err) {
      throw new Error(renameErrorText(err));
    }
    this.log.info(`renamed machine ${updated.id} to "${updated.name}"`);
    void vscode.window.setStatusBarMessage(`Krypto: this machine is now “${updated.name}”`, 4000);
  }

  /**
   * Read-only list of this machine and every paired hub with its online state, kept live while open.
   * Pairing and revoking happen in the desktop app (or `krypto peer …`), not here.
   */
  /**
   * Every setting lives in the Krypto desktop app (PLAN.md migration step 5b): ask it to open its Settings view.
   * The hub forwards `open_panel` to connected desktop apps; without one, say so.
   */
  async openSettings(): Promise<void> {
    let delivered: boolean;
    try {
      delivered = await this.hub.api.openPanel("settings");
    } catch (err) {
      throw new Error(openSettingsErrorText(err));
    }
    if (!delivered) {
      void vscode.window.showInformationMessage("Open the Krypto desktop app to change settings.");
      return;
    }
    this.log.info("asked the desktop app to open Settings");
  }

  showMachines(): void {
    const qp = vscode.window.createQuickPick<vscode.QuickPickItem>();
    qp.placeholder = "Read-only. Pair or revoke machines in the Krypto desktop app or with krypto peer.";
    qp.matchOnDescription = true;
    qp.matchOnDetail = true;
    const render = () => {
      const { title, items } = machinePickItems(this.hub.state, this.hub.connected);
      qp.title = title;
      qp.items = items;
    };
    render();
    const sub = this.hub.onState(() => render());
    qp.onDidAccept(() => qp.hide());
    qp.onDidHide(() => {
      sub.dispose();
      qp.dispose();
    });
    qp.show();
  }

  // ---------------- pets ----------------

  /** `petId` if known, else the only pet, else a quick pick. Undefined when there is none / Esc. */
  private async pickPet(title: string, petId?: string): Promise<Pet | undefined> {
    const state = this.hub.state;
    const known = typeof petId === "string" && petId ? state.pets[petId] : undefined;
    if (known) {
      this.assertLocal(known.machineId);
      return known;
    }
    // Another machine's pets belong to its hub.
    const pets = Object.values(state.pets)
      .filter((p) => isLocal(state, p.machineId))
      .sort((a, b) => a.name.localeCompare(b.name));
    if (pets.length === 0) {
      void vscode.window.showInformationMessage("Krypto: no pets yet — each agent gets one when the hub discovers it.");
      return undefined;
    }
    if (pets.length === 1) return pets[0];
    const pick = await vscode.window.showQuickPick(
      pets.map((p) => ({
        pet: p,
        label: `${SPECIES_META[p.species].emoji} ${p.name}`,
        description: agentName(state.agents, p.agentId, p.machineId),
      })),
      { title, placeHolder: "Which pet?" },
    );
    return pick?.pet;
  }

  /** Rename a pet (input box unless `name` is given). Names are unique across every known pet. */
  async renamePet(petId?: string, name?: string): Promise<void> {
    const api = this.hub.api;
    const pet = await this.pickPet("Krypto: Rename Pet", petId);
    if (!pet) return;
    const others = () => Object.values(this.hub.state.pets);
    let next = typeof name === "string" ? name : undefined;
    if (next === undefined) {
      next = await vscode.window.showInputBox({
        title: `Krypto: Rename ${pet.name}`,
        prompt: `Watches ${agentName(this.hub.state.agents, pet.agentId, pet.machineId)}. Shown above the pet everywhere.`,
        value: pet.name,
        valueSelection: [0, pet.name.length],
        ignoreFocusOut: true,
        validateInput: (v) => {
          if (v.trim() === pet.name) return null;
          const check = checkPetName(v, pet.id, others());
          return check.ok ? null : check.error;
        },
      });
      if (next === undefined) return; // Esc
    }
    if (next.trim() === pet.name) return;
    const check = checkPetName(next, pet.id, others());
    if (!check.ok) throw new Error(check.error);
    let updated: Pet;
    try {
      updated = await api.updatePet(pet.id, { name: check.name });
    } catch (err) {
      throw new Error(petRenameErrorText(err, check.name) ?? errorMessage(err));
    }
    void vscode.window.setStatusBarMessage(`Krypto: ${SPECIES_META[updated.species].emoji} ${pet.name} is now ${updated.name}`, 4000);
  }

  async changeSpecies(petId?: string, species?: Species): Promise<void> {
    const api = this.hub.api;
    const chosen = await this.pickPet("Krypto: Change Pet Species", petId);
    if (!chosen) return;
    let next = species && species in SPECIES_META ? species : undefined;
    if (!next) {
      const pick = await vscode.window.showQuickPick(
        (Object.keys(SPECIES_META) as Species[]).map((s) => ({
          species: s,
          label: `${SPECIES_META[s].emoji} ${SPECIES_META[s].label}`,
          description: s === chosen.species ? "current" : undefined,
        })),
        { title: `Krypto: ${chosen.name} should be a…`, placeHolder: "Pick a species" },
      );
      next = pick?.species;
    }
    if (!next || next === chosen.species) return;
    const updated = await api.updatePet(chosen.id, { species: next });
    void vscode.window.setStatusBarMessage(`Krypto: ${updated.name} is now a ${SPECIES_META[updated.species].label.toLowerCase()} ${SPECIES_META[updated.species].emoji}`, 4000);
  }
}
