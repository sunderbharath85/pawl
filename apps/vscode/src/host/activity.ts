import * as vscode from "vscode";
import type { HubConnection } from "./connection";

/**
 * Tells the hub the user is on this machine (ClientMsg `user_activity`, PLAN.md migration step 3):
 * window focus, editor edits, selection and active-editor changes. Only while this window is focused,
 * so files an agent rewrites in the background do not count. HubClient throttles the reports.
 */
export class ActivityReporter implements vscode.Disposable {
  private readonly disposables: vscode.Disposable[];
  private wasConnected = false;

  constructor(private readonly hub: HubConnection) {
    const report = () => this.report();
    this.disposables = [
      vscode.window.onDidChangeWindowState((s) => {
        if (s.focused) report();
      }),
      vscode.window.onDidChangeTextEditorSelection(report),
      vscode.window.onDidChangeActiveTextEditor(report),
      vscode.workspace.onDidChangeTextDocument((e) => {
        if (e.contentChanges.length > 0) report();
      }),
      // A (re)connect while the window is focused: say so right away.
      hub.onState((state) => {
        const connected = state.status === "connected";
        if (connected && !this.wasConnected) report();
        this.wasConnected = connected;
      }),
    ];
  }

  private report(): void {
    if (!vscode.window.state.focused || !this.hub.connected) return;
    this.hub.api.reportUserActivity();
  }

  dispose(): void {
    for (const d of this.disposables) d.dispose();
  }
}
