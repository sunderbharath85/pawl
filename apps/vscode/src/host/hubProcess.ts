import * as vscode from "vscode";
import { execFile } from "node:child_process";
import type { HubConnection } from "./connection";
import { portOf } from "./connection";
import { errorMessage, needsShell, quoteWinArg, resolveHubExecutable } from "./util";

const HUB_TERMINAL_NAME = "AI Pet Hub";

/** Find the hub binary or explain how to configure it. */
async function requireHubExecutable(): Promise<string | null> {
  const exe = resolveHubExecutable();
  if (exe) return exe;
  const configured = vscode.workspace.getConfiguration("ai-pet").get<string>("hubExecutable") || "pet-hub";
  const pick = await vscode.window.showErrorMessage(
    `AI Pet: could not find the hub executable “${configured}”. Install pet-hub on PATH or set “ai-pet.hubExecutable”.`,
    "Open Settings",
  );
  if (pick === "Open Settings") {
    await vscode.commands.executeCommand("workbench.action.openSettings", "ai-pet.hubExecutable");
  }
  return null;
}

/**
 * Run `pet-hub serve` in a VS Code terminal (reusing it if it is still alive) and reconnect once it is up.
 */
export async function startHub(hub: HubConnection, log: vscode.LogOutputChannel): Promise<void> {
  if (hub.connected) {
    void vscode.window.showInformationMessage("AI Pet: the hub is already running.");
    return;
  }
  const existing = vscode.window.terminals.find((t) => t.name === HUB_TERMINAL_NAME && t.exitStatus === undefined);
  if (existing) {
    existing.show(true);
    hub.restart();
    return;
  }
  const exe = await requireHubExecutable();
  if (!exe) return;
  const args = ["serve"];
  const port = portOf(hub.info.url);
  if (port !== null) args.push("--port", String(port));
  log.info(`starting hub: ${exe} ${args.join(" ")}`);
  const terminal = vscode.window.createTerminal({
    name: HUB_TERMINAL_NAME,
    shellPath: exe,
    shellArgs: args,
    iconPath: new vscode.ThemeIcon("server-process"),
    isTransient: true,
  });
  terminal.show(true);
  // The hub creates its token on first start; reconnect a few times while it boots.
  for (const delay of [700, 2000, 4500]) {
    setTimeout(() => {
      // Do not interrupt an attempt that is already in flight.
      if (!hub.connected && hub.info.status !== "connecting") hub.restart();
    }, delay);
  }
}

export type HookTargetId = "claude" | "codex" | "copilot-cli" | "copilot-vscode";

const HOOK_TARGETS: Array<vscode.QuickPickItem & { id: HookTargetId; needsRepo: boolean }> = [
  { id: "claude", label: "Claude Code", description: "~/.claude/settings.json", detail: "SessionStart, Notification, Stop, SessionEnd hooks (CLI + VS Code extension)", needsRepo: false },
  { id: "codex", label: "Codex", description: "~/.codex/config.toml", detail: "notify → pet-hub hook codex", needsRepo: false },
  { id: "copilot-cli", label: "Copilot CLI", description: "~/.copilot/hooks/ai-pet.json", detail: "Personal Copilot CLI hooks", needsRepo: false },
  { id: "copilot-vscode", label: "Copilot in VS Code", description: ".github/hooks/ai-pet.json", detail: "Agent hooks for this repository (preview)", needsRepo: true },
];

interface RunResult {
  code: number;
  stdout: string;
  stderr: string;
}

function runHub(exe: string, args: string[], cwd: string | undefined): Promise<RunResult> {
  return new Promise((resolve) => {
    const shell = needsShell(exe);
    const file = shell ? quoteWinArg(exe) : exe;
    const argv = shell ? args.map(quoteWinArg) : args;
    execFile(
      file,
      argv,
      { cwd, timeout: 60_000, windowsHide: true, shell, maxBuffer: 8 * 1024 * 1024, encoding: "utf8" },
      (err, stdout, stderr) => {
        const code = err ? (typeof (err as { code?: unknown }).code === "number" ? ((err as { code: number }).code) : 1) : 0;
        resolve({ code, stdout: stdout ?? "", stderr: stderr || (err && !stdout ? errorMessage(err) : "") });
      },
    );
  });
}

async function pickRepoFolder(): Promise<string | undefined> {
  const folders = (vscode.workspace.workspaceFolders ?? []).filter((f) => f.uri.scheme === "file");
  if (folders.length === 0) {
    void vscode.window.showWarningMessage("AI Pet: open a folder first — Copilot hooks are installed per repository.");
    return undefined;
  }
  if (folders.length === 1) return folders[0].uri.fsPath;
  const pick = await vscode.window.showWorkspaceFolderPick({ placeHolder: "Install Copilot hooks into which repository?" });
  return pick?.uri.fsPath;
}

/**
 * `pet-hub install-hooks <target>` dry-run → show the diff → ask → `--apply`.
 */
export async function installHooks(log: vscode.LogOutputChannel, preselect?: HookTargetId): Promise<void> {
  const target =
    HOOK_TARGETS.find((t) => t.id === preselect) ??
    (await vscode.window.showQuickPick(HOOK_TARGETS, {
      title: "AI Pet: Install Agent Hooks",
      placeHolder: "Which agent should report to your pet?",
      matchOnDescription: true,
      matchOnDetail: true,
    }));
  if (!target) return;
  const exe = await requireHubExecutable();
  if (!exe) return;
  const cwd = target.needsRepo ? await pickRepoFolder() : vscode.workspace.workspaceFolders?.[0]?.uri.fsPath;
  if (target.needsRepo && !cwd) return;

  const args = ["install-hooks", target.id];
  if (target.needsRepo && cwd) args.push("--repo", cwd);
  log.info(`dry run: ${exe} ${args.join(" ")}${cwd ? ` (cwd ${cwd})` : ""}`);
  const dry = await vscode.window.withProgress(
    { location: vscode.ProgressLocation.Notification, title: `AI Pet: checking ${target.label} hooks…` },
    () => runHub(exe, args, cwd),
  );
  if (dry.code !== 0) {
    log.error(`install-hooks dry run failed (${dry.code}): ${dry.stderr || dry.stdout}`);
    const choice = await vscode.window.showErrorMessage(
      `AI Pet: “pet-hub ${args.join(" ")}” failed: ${firstLine(dry.stderr || dry.stdout) || `exit code ${dry.code}`}`,
      "Show Log",
    );
    if (choice === "Show Log") log.show(true);
    return;
  }

  const preview = dry.stdout.trim() || "(no changes reported)";
  const doc = await vscode.workspace.openTextDocument({ content: preview + "\n", language: looksLikeDiff(preview) ? "diff" : "plaintext" });
  await vscode.window.showTextDocument(doc, { preview: true, viewColumn: vscode.ViewColumn.Active });
  if (/already (installed|up to date)|no changes/i.test(preview) && !looksLikeDiff(preview)) {
    void vscode.window.showInformationMessage(`AI Pet: ${target.label} hooks are already installed.`);
    return;
  }

  const apply = await vscode.window.showInformationMessage(
    `Apply these ${target.label} hook changes?`,
    { modal: true, detail: "A backup of the original file is written first." },
    "Apply",
  );
  if (apply !== "Apply") return;

  const applied = await vscode.window.withProgress(
    { location: vscode.ProgressLocation.Notification, title: `AI Pet: installing ${target.label} hooks…` },
    () => runHub(exe, [...args, "--apply"], cwd),
  );
  if (applied.code !== 0) {
    log.error(`install-hooks --apply failed (${applied.code}): ${applied.stderr || applied.stdout}`);
    const choice = await vscode.window.showErrorMessage(
      `AI Pet: installing ${target.label} hooks failed: ${firstLine(applied.stderr || applied.stdout) || `exit code ${applied.code}`}`,
      "Show Log",
    );
    if (choice === "Show Log") log.show(true);
    return;
  }
  log.info(`install-hooks ${target.id} applied:\n${applied.stdout}`);
  const msg = firstLine(applied.stdout) || `${target.label} hooks installed.`;
  const extra = target.id === "claude" ? " Restart running Claude Code sessions to pick them up." : "";
  void vscode.window.showInformationMessage(`AI Pet: ${msg}${extra}`);
}

function firstLine(s: string): string {
  return s.split(/\r?\n/).map((l) => l.trim()).find(Boolean) ?? "";
}

function looksLikeDiff(s: string): boolean {
  return /^(---|\+\+\+|@@|[+-][^+-])/m.test(s);
}
