import * as vscode from "vscode";
import type { WebviewToHost } from "./shared/messages";
import { sessionBackfill } from "./host/backfill";
import { Commands } from "./host/commands";
import { HubConnection } from "./host/connection";
import { EditedFilesWatcher } from "./host/editedFiles";
import { Notifier } from "./host/notifications";
import { PetWebviews } from "./host/petView";
import { PetStatusBar } from "./host/statusBar";
import { toFileUri } from "./host/util";

let connection: HubConnection | undefined;

export function activate(context: vscode.ExtensionContext): void {
  const log = vscode.window.createOutputChannel("Krypto", { log: true });
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
    ...commands.register(),
  );

  hub.start();
  log.info(`Krypto for VS Code ${version} activated`);
}

export function deactivate(): void {
  connection?.dispose();
  connection = undefined;
}

const WEBVIEW_COMMANDS = {
  startSession: "krypto.startSession",
  startHub: "krypto.startHub",
  installHooks: "krypto.installHooks",
  changeSpecies: "krypto.changeSpecies",
  reconnect: "krypto.reconnect",
  openPanel: "krypto.openPetPanel",
  renameMachine: "krypto.renameMachine",
  renamePet: "krypto.renamePet",
} as const;

async function handleWebviewMessage(msg: WebviewToHost, hub: HubConnection): Promise<void> {
  switch (msg.type) {
    case "approve":
      await hub.api.approve(msg.requestId);
      return;
    case "deny":
      await hub.api.deny(msg.requestId);
      return;
    case "resolve":
      await hub.api.resolvePermission(msg.requestId, msg.optionId);
      return;
    case "prompt":
      if (msg.text.trim()) await hub.api.prompt(msg.sessionId, msg.text.trim());
      return;
    case "cancel":
      await hub.api.cancel(msg.sessionId);
      return;
    case "chat":
      if (msg.text.trim()) await hub.api.chat(msg.text.trim(), msg.petId);
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
