// Settings > A2A > "Connect coding agents" (docs/CONTRACTS.md "v0.2 A2A: connect coding agents"), the pure part:
// state labels and tones, what the main button does, the confirm texts, and telling an older hub apart. Connect puts a
// "pawl" MCP entry (`pawl mcp`, a thin client of Pawl's localhost A2A server) into the agent's own config, with that
// agent's own A2A caller token; Remove takes the entry out and deletes the caller.
import type { A2aCallerSetting, A2aConnectStatus, A2aConnectTargetName, A2aSettings, AckResults } from "@pawl/protocol";
import type { RiskyChange } from "./settings";
import { HOOK_TARGET_LABEL } from "./settings";

/**
 * The a2a_connect ack: `newToken` (Connect makes / made a new token, so the agent's old one stops working) and `warning`
 * (Remove revoked the token but could not edit the agent's file). Hubs from before these fields omit them at runtime.
 */
export type A2aConnectResult = AckResults["a2a_connect"];

/** The rows, in the hub's order. */
export const A2A_CONNECT_TARGETS: readonly A2aConnectTargetName[] = ["claude", "codex", "copilot-cli", "copilot-vscode"];

export const A2A_CONNECT_STATE_LABEL: Record<A2aConnectStatus["state"], string> = {
  connected: "Connected",
  outdated: "Needs update",
  missing: "Not connected",
  unsupported: "Not supported",
};

/** The status pill's tone (the same tones as the hook rows). */
export const A2A_CONNECT_STATE_TONE: Record<A2aConnectStatus["state"], "done" | "waiting" | "idle"> = {
  connected: "done",
  outdated: "waiting",
  missing: "idle",
  unsupported: "idle",
};

/** "Claude Code": the hub's name for the row, else the hook target's label. */
export function a2aConnectName(s: Pick<A2aConnectStatus, "target"> & { name?: string | undefined }): string {
  return s.name?.trim() || HOOK_TARGET_LABEL[s.target] || s.target;
}

/** What the main button does: connect (no entry), update (a stale entry), or nothing (connected, unsupported). */
export function a2aConnectAction(s: Pick<A2aConnectStatus, "state">): "connect" | "update" | null {
  return s.state === "missing" ? "connect" : s.state === "outdated" ? "update" : null;
}

/**
 * The agent's caller was made (or taken over) by Connect (`callerMarked`; a hub without the field: false). Only such a
 * caller is removed by Remove: a caller the user added by hand with the same id is never touched.
 */
export function a2aConnectCallerMarked(s: { callerExists?: boolean | undefined; callerMarked?: boolean | undefined }): boolean {
  return s.callerExists === true && s.callerMarked === true;
}

/**
 * Remove is offered whenever there may be something to take away: an entry (current or stale), a config Pawl cannot
 * read (the hub still revokes the token and says what it could not do), or a caller Connect made, alone. A missing row
 * whose caller was added by hand has no Remove.
 */
export function a2aConnectRemovable(s: Pick<A2aConnectStatus, "state"> & { callerExists?: boolean | undefined; callerMarked?: boolean | undefined }): boolean {
  return s.state === "connected" || s.state === "outdated" || s.state === "unsupported" || a2aConnectCallerMarked(s);
}

/** Preview is offered while the config can be read (an unsupported one has nothing to show). */
export function a2aConnectPreviewable(s: Pick<A2aConnectStatus, "state">): boolean {
  return s.state !== "unsupported";
}

export const A2A_SERVER_STARTS = "This turns on Pawl's A2A server, on 127.0.0.1 only.";

/** What the fresh preview says Connect will do, for the confirm's wording. */
export interface ConnectFacts {
  /** Connect turns the A2A server on (127.0.0.1 only). */
  startsServer: boolean;
  /** Connect makes the caller a new token (missing from earlier hubs: then assumed when the caller exists). */
  newToken?: boolean | undefined;
  /** The agent's caller is already there. */
  callerExists: boolean;
  /** That caller is one the user added by hand (no `connectTarget`): Connect takes it over. */
  takesOver: boolean;
  callerId: string;
}

/** The facts from a preview ack and the callers in the settings now. */
export function connectFacts(
  preview: Pick<A2aConnectResult, "startsServer" | "newToken"> & { status: Pick<A2aConnectStatus, "callerId" | "callerExists" | "callerMarked"> },
  callers: readonly A2aCallerSetting[],
): ConnectFacts {
  const { callerId, callerExists, callerMarked } = preview.status;
  const existing = callers.find((c) => c.id === callerId);
  // The hub says whether Connect made the caller (`callerMarked`); a hub without it: the caller's `connectTarget`.
  const handAdded = callerMarked !== undefined ? !callerMarked : !!existing && callerConnectTarget(existing) === null;
  return {
    startsServer: preview.startsServer,
    newToken: preview.newToken,
    callerExists,
    takesOver: callerExists && handAdded,
    callerId,
  };
}

/**
 * The confirm before Connect / Update, worded from the fresh preview: which file gets the "pawl" entry and that the
 * agent's token is stored there; "the old token stops working" only when a new token replaces one; taking over a
 * caller the user added; Settings Sync for VS Code's mcp.json; and the A2A server going on.
 */
export function a2aConnectConfirm(s: Pick<A2aConnectStatus, "target" | "state" | "configPath"> & { name?: string | undefined }, facts: ConnectFacts): RiskyChange {
  const name = a2aConnectName(s);
  const update = s.state === "outdated";
  const replacesToken = facts.callerExists && (facts.newToken ?? true);
  // No new token for an existing caller: the entry is current and Connect only turns the server back on.
  const entryCurrent = facts.callerExists && facts.newToken === false;
  let token: string;
  if (facts.takesOver) {
    token = `It takes over the caller "${facts.callerId}" you added: ${name} uses it from now on${replacesToken ? ", with a new token stored in that file; its old token stops working" : ""}.`;
  } else if (replacesToken) {
    token = `${name} gets a new token, stored in that file; its old token stops working.`;
  } else if (facts.callerExists) {
    token = `${name} keeps its token, stored in that file.`;
  } else {
    token = `${name} gets its own token, stored in that file.`;
  }
  const body = [
    entryCurrent
      ? `The "pawl" entry in ${s.configPath} stays as it is.`
      : update
        ? `Pawl rewrites the "pawl" entry in ${s.configPath}.`
        : `Pawl adds a "pawl" entry to ${s.configPath}.`,
    token,
    `${name} can then start work in folders Pawl knows and check on it. Everything it does still asks you here, and it can never answer approvals.`,
  ];
  if (!entryCurrent) body.push("A backup of the file is made first.");
  if (s.target === "copilot-vscode") body.push("If VS Code's Settings Sync is on, it can copy mcp.json, token included, to your other computers.");
  if (facts.startsServer) body.push(A2A_SERVER_STARTS);
  return {
    key: `a2a.connect.${s.target}`,
    title: update ? `Update ${name}'s connection to Pawl?` : `Connect ${name} to Pawl?`,
    body: body.join(" "),
    confirm: update ? "Update" : "Connect",
  };
}

/**
 * The confirm before Remove (in the Connect list, or a Connect caller's Remove in Callers): the entry goes and the
 * agent's token stops working at once. Without the path (the list has not loaded) it says "its config file".
 */
export function a2aConnectRemoveConfirm(
  s: Pick<A2aConnectStatus, "target"> & {
    name?: string | undefined;
    configPath?: string | undefined;
    state?: A2aConnectStatus["state"] | undefined;
    callerExists?: boolean | undefined;
    callerMarked?: boolean | undefined;
  },
): RiskyChange {
  const name = a2aConnectName(s);
  const path = s.configPath || `${name}'s config file`;
  // The token only stops working if Remove deletes a caller, and it deletes only one Connect made.
  const revokes = a2aConnectCallerMarked(s);
  const parts: string[] = [];
  if (s.state === "unsupported") {
    // Pawl cannot read the file: it tries, and may not manage; the token goes either way.
    parts.push(`Pawl tries to take its "pawl" entry out of ${path} (a backup is made first), but it may not be able to: it cannot read that file.`);
    if (revokes) parts.push(`Either way it removes ${name}'s caller: its token stops working at once and anything it has open with Pawl ends.`);
  } else {
    parts.push(
      `Pawl takes its "pawl" entry out of ${path} (a backup is made first)` +
        (revokes ? ` and removes ${name}'s caller: its token stops working at once and anything it has open with Pawl ends.` : "."),
    );
  }
  parts.push("Pawl's A2A server stays as it is.");
  return {
    key: `a2a.connect.${s.target}`,
    title: `Disconnect ${name} from Pawl?`,
    body: parts.join(" "),
    confirm: "Remove",
  };
}

/**
 * A connected row whose agent cannot reach Pawl right now, shown once as the row's error: the hub's own detail (newer
 * hubs name a stopped or unbound A2A server there), else the server's error from the settings (`a2a.error`, e.g. the
 * port is in use). Null when fine.
 */
export function a2aConnectServerProblem(
  s: Pick<A2aConnectStatus, "target" | "state"> & { name?: string | undefined; detail?: string | undefined },
  a2a: Pick<A2aSettings, "error"> | null | undefined,
): string | null {
  if (s.state !== "connected") return null;
  if (s.detail) return s.detail;
  if (!a2a?.error) return null;
  return `${a2aConnectName(s)} cannot reach Pawl: the A2A server is not running (${a2a.error}).`;
}

/**
 * The coding agent Connect made this caller for (the hub's `connectTarget` on the caller), or null for a caller added
 * by hand. The hub never lets such a caller answer approvals.
 */
export function callerConnectTarget(c: Pick<A2aCallerSetting, "id"> & { connectTarget?: string | null | undefined }): A2aConnectTargetName | null {
  const t = c.connectTarget;
  return t && (A2A_CONNECT_TARGETS as readonly string[]).includes(t) ? (t as A2aConnectTargetName) : null;
}

/** In Callers, under a caller Connect made (in place of "Can answer approvals"). */
export function connectCallerHint(target: A2aConnectTargetName, name?: string): string {
  return `Connected by Pawl for ${a2aConnectName({ target, name })}. It can never answer approvals.`;
}

/** The toast after Connect / Remove went through (or found nothing to do). A Remove with a warning: "Token revoked". */
export function a2aConnectDoneText(s: Pick<A2aConnectStatus, "target"> & { name?: string | undefined }, action: "apply" | "remove", applied: boolean, warning?: string): string {
  const name = a2aConnectName(s);
  if (action === "remove" && warning) return "Token revoked";
  if (action === "remove") return applied ? `Disconnected ${name}` : `${name} was not connected`;
  return applied ? `Connected ${name} to Pawl` : `${name} was already connected`;
}

/**
 * The hub does not know this message (it is older than the feature): its error code is
 * "invalid message: unknown variant `<type>`, …" (older replies may say unknown_type / unknown_message / unsupported).
 */
export function hubLacksMessage(err: unknown, type: string): boolean {
  const code = err && typeof err === "object" && typeof (err as { code?: unknown }).code === "string" ? (err as { code: string }).code : typeof err === "string" ? err : "";
  if (code === "unknown_type" || code === "unknown_message" || code === "unsupported") return true;
  return code.startsWith("invalid message:") && code.includes(`unknown variant \`${type}\``);
}
