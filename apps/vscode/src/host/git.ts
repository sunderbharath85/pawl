// Minimal typings for the built-in git extension API (vscode.git, API v1).
// Subset of microsoft/vscode extensions/git/src/api/git.d.ts; only what we use.
import * as vscode from "vscode";

type APIState = "uninitialized" | "initialized";

interface GitRepository {
  readonly rootUri: vscode.Uri;
}

interface GitAPI {
  readonly state: APIState;
  readonly onDidChangeState: vscode.Event<APIState>;
  readonly repositories: GitRepository[];
  toGitUri(uri: vscode.Uri, ref: string): vscode.Uri;
  getRepository(uri: vscode.Uri): GitRepository | null;
}

interface GitExtension {
  readonly enabled: boolean;
  getAPI(version: 1): GitAPI;
}

let cached: GitAPI | null | undefined;

async function gitApi(): Promise<GitAPI | null> {
  if (cached) return cached;
  const ext = vscode.extensions.getExtension<GitExtension>("vscode.git");
  if (!ext) return (cached = null);
  try {
    const exports = ext.isActive ? ext.exports : await ext.activate();
    if (!exports?.enabled) return null; // user may enable git later; don't cache the miss
    const api = exports.getAPI(1);
    if (api.state !== "initialized") {
      await new Promise<void>((resolve) => {
        const timer = setTimeout(() => {
          sub.dispose();
          resolve();
        }, 3000);
        const sub = api.onDidChangeState((s) => {
          if (s === "initialized") {
            clearTimeout(timer);
            sub.dispose();
            resolve();
          }
        });
      });
    }
    cached = api;
    return api;
  } catch {
    return null;
  }
}

/**
 * URI of `uri` at git HEAD, served by the git extension's content provider,
 * or null when the file is not inside a git repository (or git is disabled).
 */
export async function headUriFor(uri: vscode.Uri): Promise<vscode.Uri | null> {
  const api = await gitApi();
  if (!api) return null;
  const repo = api.getRepository(uri);
  if (!repo) return null;
  return api.toGitUri(uri, "HEAD");
}
