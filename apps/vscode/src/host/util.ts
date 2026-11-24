import * as vscode from "vscode";
import * as fs from "node:fs";
import * as path from "node:path";
import { randomBytes } from "node:crypto";
import type { Agent, Session, SessionStatus } from "@krypto/protocol";

export function nonce(): string {
  return randomBytes(18).toString("base64").replace(/[^A-Za-z0-9]/g, "");
}

export function errorMessage(err: unknown): string {
  if (err instanceof Error) return err.message;
  if (typeof err === "string") return err;
  try {
    return JSON.stringify(err);
  } catch {
    return String(err);
  }
}

export const ACTIVE_STATUSES: ReadonlySet<SessionStatus> = new Set(["thinking", "working", "waiting_permission"]);

export function isBusy(s: Session): boolean {
  return !s.endedAt && ACTIVE_STATUSES.has(s.status);
}

export function isOpen(s: Session): boolean {
  return !s.endedAt;
}

export function agentName(agents: readonly Agent[], agentId: string): string {
  return agents.find((a) => a.id === agentId)?.name ?? agentId;
}

export function sessionLabel(s: Session, agents: readonly Agent[]): string {
  const name = agentName(agents, s.agentId);
  const where = s.title?.trim() || path.basename(s.cwd) || s.cwd;
  return `${name} · ${where}`;
}

/** Resolve a tool location to a file URI (relative paths are resolved against the session cwd). */
export function toFileUri(p: string, cwd?: string): vscode.Uri {
  if (p.startsWith("file://")) return vscode.Uri.parse(p);
  const abs = path.isAbsolute(p) ? p : path.resolve(cwd ?? "", p);
  return vscode.Uri.file(abs);
}

/** The workspace folder containing `uri`, or undefined. */
export function workspaceFolderOf(uri: vscode.Uri): vscode.WorkspaceFolder | undefined {
  return vscode.workspace.getWorkspaceFolder(uri);
}

export function isInWorkspace(p: string | undefined, cwd?: string): boolean {
  if (!p) return false;
  return workspaceFolderOf(toFileUri(p, cwd)) !== undefined;
}

const WIN = process.platform === "win32";

function isFile(p: string): boolean {
  try {
    return fs.statSync(p).isFile();
  } catch {
    return false;
  }
}

/** PATH lookup honoring PATHEXT on Windows. Returns an absolute path or null. */
export function which(cmd: string): string | null {
  const exts = WIN ? ["", ...(process.env.PATHEXT ?? ".EXE;.CMD;.BAT;.COM").split(";").filter(Boolean)] : [""];
  const hasDir = cmd.includes("/") || cmd.includes("\\");
  if (hasDir || path.isAbsolute(cmd)) {
    for (const ext of exts) {
      const candidate = cmd + ext;
      if (isFile(candidate)) return path.resolve(candidate);
    }
    return null;
  }
  const dirs = (process.env.PATH ?? process.env.Path ?? "").split(path.delimiter).filter(Boolean);
  for (const dir of dirs) {
    for (const ext of exts) {
      const candidate = path.join(dir.replace(/^"(.*)"$/, "$1"), cmd + ext);
      if (isFile(candidate)) return candidate;
    }
  }
  return null;
}

/**
 * Locate the hub binary: the `krypto.hubExecutable` setting (PATH lookup), else a cargo build output in a
 * workspace folder (target/release, target/debug). Returns null when nothing was found.
 */
export function resolveHubExecutable(): string | null {
  const configured = vscode.workspace.getConfiguration("krypto").get<string>("hubExecutable")?.trim() || "krypto";
  const found = which(configured);
  if (found) return found;
  const exe = WIN ? "krypto.exe" : "krypto";
  for (const folder of vscode.workspace.workspaceFolders ?? []) {
    if (folder.uri.scheme !== "file") continue;
    for (const profile of ["release", "debug"]) {
      const candidate = path.join(folder.uri.fsPath, "target", profile, exe);
      if (isFile(candidate)) return candidate;
    }
  }
  return null;
}

/** Windows .cmd/.bat shims cannot be spawned without a shell (Node CVE-2024-27980 hardening). */
export function needsShell(exe: string): boolean {
  return WIN && /\.(cmd|bat)$/i.test(exe);
}

/** Quote one argument for cmd.exe when we must spawn through a shell. */
export function quoteWinArg(arg: string): string {
  if (/^[\w\-.:\\/=]+$/.test(arg)) return arg;
  return `"${arg.replace(/(\\*)"/g, '$1$1\\"').replace(/(\\+)$/, "$1$1")}"`;
}

export function relativeTime(ts: number, now = Date.now()): string {
  const s = Math.max(0, Math.round((now - ts) / 1000));
  if (s < 45) return "just now";
  const m = Math.round(s / 60);
  if (m < 60) return `${m}m ago`;
  const h = Math.round(m / 60);
  if (h < 24) return `${h}h ago`;
  return `${Math.round(h / 24)}d ago`;
}
