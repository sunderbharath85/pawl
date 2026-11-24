import * as vscode from "vscode";
import type { HubState } from "@ai-pet/hub-client";
import { SPECIES_META } from "@ai-pet/pet-engine";
import { DEFAULT_SPECIES } from "@ai-pet/protocol";
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
    this.item = vscode.window.createStatusBarItem("ai-pet.status", vscode.StatusBarAlignment.Right, 100);
    this.item.name = "AI Pet";
    this.item.command = "ai-pet.showPet";
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
      void vscode.commands.executeCommand("setContext", "ai-pet.hasPending", hasPending);
    }

    if (conn.status !== "connected") {
      const key = `off|${conn.offline}|${conn.missingToken}|${conn.status}`;
      if (key === this.lastKey) return;
      this.lastKey = key;
      this.item.backgroundColor = undefined;
      if (conn.offline) {
        this.item.text = `$(debug-disconnect) ${SPECIES_META[DEFAULT_SPECIES].emoji} hub offline`;
        this.item.tooltip = this.offlineTooltip(conn.status === "unauthorized", conn.missingToken);
      } else {
        this.item.text = `$(sync~spin) ${SPECIES_META[DEFAULT_SPECIES].emoji}`;
        this.item.tooltip = "AI Pet: connecting to the hub…";
      }
      return;
    }

    const views = petViewsOf(state);
    const primary = primaryView(views);
    const species = primary?.species ?? DEFAULT_SPECIES;
    const activity = primary?.activity ?? "napping";
    const busy = Object.values(state.sessions).filter(isBusy).length;
    const key = `on|${species}|${activity}|${pendingCount}|${busy}|${views.map((v) => `${v.petId}:${v.activity}`).join(",")}`;
    if (key === this.lastKey) return;
    this.lastKey = key;

    let text = `${SPECIES_META[species].emoji} ${ACTIVITY_LABEL[activity]}`;
    if (pendingCount > 0) text += `  $(bell-dot) ${pendingCount}`;
    this.item.text = text;
    this.item.backgroundColor = pendingCount > 0 ? new vscode.ThemeColor("statusBarItem.warningBackground") : undefined;
    this.item.tooltip = this.onlineTooltip(state, views, pendingCount, busy);
  }

  private offlineTooltip(unauthorized: boolean, missingToken: boolean): vscode.MarkdownString {
    const md = new vscode.MarkdownString(undefined, true);
    md.isTrusted = { enabledCommands: ["ai-pet.startHub", "ai-pet.reconnect", "ai-pet.showPet"] };
    md.appendMarkdown("**AI Pet — hub offline**\n\n");
    if (unauthorized) md.appendMarkdown("The hub rejected our token. It may have been regenerated.\n\n");
    else if (missingToken) md.appendMarkdown("No hub token yet — the hub has never run on this machine.\n\n");
    else md.appendMarkdown("Reconnecting quietly in the background.\n\n");
    md.appendMarkdown("[$(play) Start hub](command:ai-pet.startHub) · [$(refresh) Retry](command:ai-pet.reconnect) · [Show pet](command:ai-pet.showPet)");
    return md;
  }

  private onlineTooltip(
    state: HubState,
    views: ReturnType<typeof petViewsOf>,
    pendingCount: number,
    busy: number,
  ): vscode.MarkdownString {
    const md = new vscode.MarkdownString(undefined, true);
    md.isTrusted = { enabledCommands: ["ai-pet.showPet", "ai-pet.approvePending", "ai-pet.denyPending", "ai-pet.startSession"] };
    md.appendMarkdown("**AI Pet**\n\n");
    if (views.length === 0) md.appendMarkdown(`${SPECIES_META[DEFAULT_SPECIES].emoji} No pets yet — start a session.\n\n`);
    for (const v of views) {
      const name = agentName(state.agents, v.agentId);
      md.appendMarkdown(`${SPECIES_META[v.species].emoji} **${escapeMd(v.name)}** (${escapeMd(name)}) — ${ACTIVITY_LABEL[v.activity]}\n\n`);
    }
    md.appendMarkdown(`${busy} busy session${busy === 1 ? "" : "s"}`);
    if (pendingCount > 0) {
      md.appendMarkdown(` · **${pendingCount} waiting for approval**\n\n`);
      md.appendMarkdown("[$(check) Approve](command:ai-pet.approvePending) · [$(close) Deny](command:ai-pet.denyPending) · ");
    } else {
      md.appendMarkdown("\n\n");
    }
    md.appendMarkdown("[Show pet](command:ai-pet.showPet) · [$(add) Start session](command:ai-pet.startSession)");
    return md;
  }

  dispose(): void {
    clearInterval(this.timer);
    for (const d of this.disposables) d.dispose();
  }
}

function escapeMd(s: string): string {
  return s.replace(/[\\`*_{}[\]()#+\-.!|<>]/g, "\\$&");
}
