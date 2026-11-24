import * as vscode from "vscode";
import type { WebviewToHost } from "./shared/messages";
import { sessionBackfill } from "@pawl/hub-client";
import { ActivityReporter } from "./host/activity";
import { runProposal } from "./host/proposals";
import { answerRequest, remoteError } from "./host/remote";
import { Commands } from "./host/commands";
import { HubConnection } from "./host/connection";
import { EditedFilesWatcher } from "./host/editedFiles";
import { Notifier } from "./host/notifications";
import { PetWebviews } from "./host/petView";
import { PetStatusBar } from "./host/statusBar";
import { toFileUri } from "./host/util";

let connection: HubConnection | undefined;

export function activate(context: vscode.ExtensionContext): void {
  const log = vscode.window.createOutputChannel("Pawl", { log: true });
  const version = String(context.extension.packageJSON.version ?? "0.0.0");
  const hub = new HubConnection(version, log);
  connection = hub;

  const views = new PetWebviews(context, hub, (msg) => handleWebviewMessage(msg, hub), log);
  const commands = new Commands(context, hub, views, log);

  context.subscriptions.push(
    log,
    hub,
    views,
    new PetStatusBar(hub),
    new Notifier(
      hub,
      {
        showPet: (sessionId) => commands.showPet(sessionId),
        promptSession: async (sessionId) => {
          try {
            await commands.promptSession(sessionId);
          } catch (err) {
            commands.reportError(err);
          }
        },
      },
      log,
    ),
    new EditedFilesWatcher(hub, log),
    new ActivityReporter(hub),
    ...commands.register(),
  );

  hub.start();
  log.info(`Pawl for VS Code ${version} activated`);
}

export function deactivate(): void {
  connection?.dispose();
  connection = undefined;
}

const WEBVIEW_COMMANDS = {
  startSession: "pawl.startSession",
  startHub: "pawl.startHub",
  installHooks: "pawl.installHooks",
  changeSpecies: "pawl.changeSpecies",
  reconnect: "pawl.reconnect",
  openPanel: "pawl.openPetPanel",
  renameMachine: "pawl.renameMachine",
  renamePet: "pawl.renamePet",
  showMachines: "pawl.showMachines",
} as const;

/** The machine a session belongs to (by the held session, else its global id prefix). */
function sessionMachine(hub: HubConnection, sessionId: string): string {
  return hub.state.sessions[sessionId]?.machineId ?? sessionId.split(":")[0] ?? "";
}

async function handleWebviewMessage(msg: WebviewToHost, hub: HubConnection): Promise<void> {
  switch (msg.type) {
    // Another machine's approval asks in a modal first; its errors name that machine.
    case "approve":
      await answerRequest(hub, msg.requestId, { kind: "approve" });
      return;
    case "deny":
      await answerRequest(hub, msg.requestId, { kind: "deny" });
      return;
    case "resolve":
      await answerRequest(hub, msg.requestId, { kind: "option", optionId: msg.optionId });
      return;
    case "prompt": {
      const text = msg.text.trim();
      if (!text) return;
      await hub.api.prompt(msg.sessionId, text).catch((err) => Promise.reject(remoteError(hub, sessionMachine(hub, msg.sessionId), "prompt", err)));
      return;
    }
    case "cancel":
      await hub.api.cancel(msg.sessionId).catch((err) => Promise.reject(remoteError(hub, sessionMachine(hub, msg.sessionId), "cancel", err)));
      return;
    case "chat":
      if (msg.text.trim()) await hub.api.chat(msg.text.trim(), msg.petId);
      return;
    case "proposal":
      await runProposal(hub, msg.messageId, msg.proposalId);
      return;
    case "loadEvents":
      // First open: latest window; afterwards catch up from the newest held `seq`. HubClient merges
      // fetched events into HubState.events (deduped by global id); the next state push carries them over.
      await sessionBackfill.sync(hub.api, msg.sessionId);
      return;
    case "openFile": {
      const uri = toFileUri(msg.path, msg.cwd);
      const line = typeof msg.line === "number" && msg.line >= 1 ? msg.line - 1 : undefined;
      await vscode.window.showTextDocument(uri, {
        preview: true,
        selection: line !== undefined ? new vscode.Range(line, 0, line, 0) : undefined,
      });
      return;
    }
    case "command": {
      const id = WEBVIEW_COMMANDS[msg.command];
      if (!id) return;
      // Through the command registry so errors are reported the same way as from the palette.
      await vscode.commands.executeCommand(id, ...(Array.isArray(msg.args) ? msg.args : []));
      return;
    }
    case "ready":
      return;
  }
}
