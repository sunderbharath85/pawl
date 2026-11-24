import * as vscode from "vscode";
import { machineCounts, sortedMachines, type HubState } from "@krypto/hub-client";
import { SPECIES_META } from "@krypto/pet-engine";
import { DEFAULT_SPECIES } from "@krypto/protocol";
import { OS_LABEL, machineDisplayName, machinesLine, presenceLabel } from "../shared/machine";
import type { HubConnection } from "./connection";
import { ACTIVITY_LABEL, petViewsOf, primaryView } from "./petViews";
import { agentName, isBusy } from "./util";

/** Re-render cadence so time-based pet activities (happy → idle, napping) decay without hub traffic. */
const TICK_MS = 2000;

export class PetStatusBar implements vscode.Disposable {
  private readonly item: vscode.StatusBarItem;
  private readonly disposables: vscode.Disposable[] = [];
  private readonly timer: ReturnType<typeof setInterval>;
  private lastKey = "";
  private lastHasPending: boolean | null = null;

  constructor(private readonly hub: HubConnection) {
    this.item = vscode.window.createStatusBarItem("krypto.status", vscode.StatusBarAlignment.Right, 100);
    this.item.name = "Krypto";
    this.item.command = "krypto.showPet";
    this.disposables.push(this.item, hub.onState(() => this.render()));
    this.timer = setInterval(() => this.render(), TICK_MS);
    this.render();
    this.item.show();
  }

  render(): void {
    const conn = this.hub.info;
    const state = this.hub.state;
    const pendingCount = Object.keys(state.pending).length;
    const hasPending = conn.status === "connected" && pendingCount > 0;
    if (hasPending !== this.lastHasPending) {
      this.lastHasPending = hasPending;
      void vscode.commands.executeCommand("setContext", "krypto.hasPending", hasPending);
    }

    if (conn.status !== "connected") {
      const key = `off|${conn.offline}|${conn.missingToken}|${conn.status}|${state.machine?.name ?? ""}`;
      if (key === this.lastKey) return;
      this.lastKey = key;
      this.item.backgroundColor = undefined;
      if (conn.offline) {
        this.item.text = `$(debug-disconnect) ${SPECIES_META[DEFAULT_SPECIES].emoji} hub offline`;
        this.item.tooltip = this.offlineTooltip(state, conn.status === "unauthorized", conn.missingToken);
      } else {
        this.item.text = `$(sync~spin) ${SPECIES_META[DEFAULT_SPECIES].emoji}`;
        this.item.tooltip = "Krypto: connecting to the hub…";
      }
      return;
    }

    const views = petViewsOf(state);
    const primary = primaryView(views);
    const species = primary?.species ?? DEFAULT_SPECIES;
    const activity = primary?.activity ?? "napping";
    const busy = Object.values(state.sessions).filter(isBusy).length;
    const machineKey = sortedMachines(state)
      .map((m) => `${m.id}:${m.name}:${m.online}:${m.online ? "" : m.lastSeen}`)
      .join(",");
    const petsKey = views.map((v) => `${v.petId}:${v.name}:${v.activity}`).join(",");
    const key = `on|${machineKey}|${species}|${activity}|${pendingCount}|${busy}|${petsKey}`;
    if (key === this.lastKey) return;
    this.lastKey = key;

    // Pets have names: lead with the most relevant pet's ("🐕 Biscuit working").
    let text = `${SPECIES_META[species].emoji} ${primary ? `${primary.name} ` : ""}${ACTIVITY_LABEL[activity]}`;
    if (pendingCount > 0) text += `  $(bell-dot) ${pendingCount}`;
    this.item.text = text;
    this.item.backgroundColor = pendingCount > 0 ? new vscode.ThemeColor("statusBarItem.warningBackground") : undefined;
    this.item.tooltip = this.onlineTooltip(state, views, pendingCount, busy);
  }

  private offlineTooltip(state: HubState, unauthorized: boolean, missingToken: boolean): vscode.MarkdownString {
    const md = new vscode.MarkdownString(undefined, true);
    md.isTrusted = { enabledCommands: ["krypto.startHub", "krypto.reconnect", "krypto.showPet"] };
    md.appendMarkdown("**Krypto — hub offline**");
    // The last machine name we heard from this hub (kept across reconnects).
    if (state.machine) md.appendMarkdown(` · ${escapeMd(machineDisplayName(state.machine))}`);
    md.appendMarkdown("\n\n");
    if (unauthorized) md.appendMarkdown("The hub rejected our token. It may have been regenerated.\n\n");
    else if (missingToken) md.appendMarkdown("No hub token yet — the hub has never run on this machine.\n\n");
    else md.appendMarkdown("Reconnecting quietly in the background.\n\n");
    md.appendMarkdown("[$(play) Start hub](command:krypto.startHub) · [$(refresh) Retry](command:krypto.reconnect) · [Show pet](command:krypto.showPet)");
    return md;
  }

  private onlineTooltip(
    state: HubState,
    views: ReturnType<typeof petViewsOf>,
    pendingCount: number,
    busy: number,
  ): vscode.MarkdownString {
    const md = new vscode.MarkdownString(undefined, true);
    md.isTrusted = {
      enabledCommands: [
        "krypto.showPet",
        "krypto.approvePending",
        "krypto.denyPending",
        "krypto.startSession",
        "krypto.renameMachine",
        "krypto.showMachines",
      ],
    };
    md.appendMarkdown("**Krypto**");
    const machine = state.machine;
    if (machine) {
      const os = OS_LABEL[machine.os] ?? machine.os;
      md.appendMarkdown(
        ` · $(device-desktop) ${escapeMd(machineDisplayName(machine))} _(${escapeMd(os)})_ [$(edit)](command:krypto.renameMachine "Rename this machine")`,
      );
    }
    md.appendMarkdown("\n\n");
    this.appendMachines(md, state);
    if (views.length === 0) md.appendMarkdown(`${SPECIES_META[DEFAULT_SPECIES].emoji} No pets yet — start a session.\n\n`);
    for (const v of views) {
      const name = agentName(state.agents, v.agentId, v.machineId);
      md.appendMarkdown(`${SPECIES_META[v.species].emoji} **${escapeMd(v.name)}** (${escapeMd(name)}) — ${ACTIVITY_LABEL[v.activity]}\n\n`);
    }
    md.appendMarkdown(`${busy} busy session${busy === 1 ? "" : "s"}`);
    if (pendingCount > 0) {
      md.appendMarkdown(` · **${pendingCount} waiting for approval**\n\n`);
      md.appendMarkdown("[$(check) Approve](command:krypto.approvePending) · [$(close) Deny](command:krypto.denyPending) · ");
    } else {
      md.appendMarkdown("\n\n");
    }
    md.appendMarkdown("[Show pet](command:krypto.showPet) · [$(add) Start session](command:krypto.startSession)");
    return md;
  }

  /** Paired machines with their online state (this machine is the header line). */
  private appendMachines(md: vscode.MarkdownString, state: HubState): void {
    const { total, online } = machineCounts(state);
    if (total === 0) return;
    const show = "[$(list-flat) Machines](command:krypto.showMachines \"Show machines\")";
    if (total === 1) {
      md.appendMarkdown(`$(remote-explorer) No other machines paired yet · ${show}\n\n`);
      return;
    }
    md.appendMarkdown(`$(remote-explorer) **${machinesLine(total, online)}** · ${show}\n\n`);
    const now = Date.now();
    for (const m of sortedMachines(state)) {
      if (m.id === state.machine?.id) continue;
      const os = OS_LABEL[m.os] ?? m.os;
      const dot = m.online ? "$(circle-filled)" : "$(circle-outline)";
      md.appendMarkdown(`${dot} ${escapeMd(machineDisplayName(m))} _(${escapeMd(os)})_ — ${escapeMd(presenceLabel(m, now))}\n\n`);
    }
  }

  dispose(): void {
    clearInterval(this.timer);
    for (const d of this.disposables) d.dispose();
  }
}

function escapeMd(s: string): string {
  return s.replace(/[\\`*_{}[\]()#+\-.!|<>]/g, "\\$&");
}
