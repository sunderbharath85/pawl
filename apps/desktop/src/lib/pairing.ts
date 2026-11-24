// Pairing + presence helpers for the Machines view (PLAN.md migration step 2).
// An invite URI holds a one-time secret: it lives only in memory here (never logged, never in
// localStorage) and is dropped the moment it expires.
import { useSyncExternalStore } from "react";
import { PEER_DEFAULT_PORT, type Device, type Machine } from "@krypto/protocol";
import { timeAgo } from "./format";

/** Request kinds whose failures we explain. */
export type PairingAction = "create_invite" | "join_peer" | "revoke_device";

const NOT_CONNECTED = "Krypto isn't connected to the hub right now. Try again once it's back.";

const PAIRING_ERRORS: Record<string, string> = {
  peer_listener_off:
    "This hub isn't listening for other machines yet. Turn on its peer listener (see below), then create the invite again.",
  bad_uri: "That isn't a Krypto invite. Copy the whole krypto://peer?… link (or the krypto peer join line) from the other machine.",
  self_invite: "That invite was made on this machine. Paste it into Krypto on the other machine instead.",
  unreachable:
    "Couldn't reach the other machine. Check that both are on the same network, its peer listener is on, and its firewall allows the port (or that both machines are connected to the relay).",
  invite_rejected:
    "The other machine turned the invite down: it may have expired or been used already. Create a new invite there and try again.",
  timeout: "The other machine didn't answer in time. Make sure it's awake and on the same network, then try again.",
  not_found: "That machine isn't paired any more.",
  unauthorized: "The hub did not accept our token. Reconnect from Settings.",
  closed: NOT_CONNECTED,
  disconnected: NOT_CONNECTED,
};

/** `invalid` means different things per request. */
const INVALID_BY_ACTION: Record<PairingAction, string> = {
  create_invite: `The hub can't tell other machines where to find it (its listener binds every network). Enter this machine's LAN address and port, e.g. 192.168.1.20:${PEER_DEFAULT_PORT}.`,
  join_peer: "That invite isn't valid. Ask for a new one.",
  revoke_device: "The hub refused to unpair that machine.",
};

function codeOf(err: unknown): string {
  return err && typeof err === "object" && "code" in err ? String((err as { code: unknown }).code) : "";
}

/** Friendly text for a failed `createInvite` / `joinPeer` / `revokeDevice` (HubRequestError carries the code). */
export function pairingErrorText(err: unknown, action: PairingAction): string {
  const code = codeOf(err);
  if (code === "invalid") return INVALID_BY_ACTION[action];
  const known = PAIRING_ERRORS[code];
  if (known) return known;
  // Unknown code from a newer hub: show it rather than a vague message. Never echo the request itself.
  if (code) return `The hub said “${code}”.`;
  return err instanceof Error && err.message ? err.message : "Something went wrong.";
}

/** "9:41" / "1:02:03" until `expiresAt`; "0:00" once past. */
export function formatCountdown(expiresAt: number, now: number): string {
  const total = Math.max(0, Math.ceil((expiresAt - now) / 1000));
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = total % 60;
  const ss = String(s).padStart(2, "0");
  return h > 0 ? `${h}:${String(m).padStart(2, "0")}:${ss}` : `${m}:${ss}`;
}

/** Online state line for a machine row. */
export function presenceText(machine: Pick<Machine, "online" | "lastSeen">, now: number): string {
  if (machine.online) return "Online";
  if (!machine.lastSeen) return "Not seen yet";
  return `Last seen ${timeAgo(machine.lastSeen, now)}`;
}

/** The CLI equivalent shown next to an invite. */
export function joinCommand(uri: string): string {
  return `krypto peer join "${uri}"`;
}

/** Invite lifetimes offered in the UI (seconds); the hub allows 60..3600. */
export const INVITE_TTL_CHOICES: ReadonlyArray<{ secs: number; label: string }> = [
  { secs: 300, label: "5 minutes" },
  { secs: 600, label: "10 minutes" },
  { secs: 1800, label: "30 minutes" },
  { secs: 3600, label: "1 hour" },
];

export interface ShownInvite {
  uri: string;
  expiresAt: number;
  /** Local time the invite was created (to spot the pairing it led to). */
  createdAt: number;
  /** Roster ids before the invite existed: a new id afterwards means someone used it. */
  knownDeviceIds: readonly string[];
  /** Machines this hub joined while the invite was up (those pairings did not use it). */
  ignoredDeviceIds: readonly string[];
}

interface Timers {
  now(): number;
  setTimeout(fn: () => void, ms: number): unknown;
  clearTimeout(handle: unknown): void;
}

const realTimers: Timers = {
  now: () => Date.now(),
  setTimeout: (fn, ms) => setTimeout(fn, ms),
  clearTimeout: (h) => clearTimeout(h as ReturnType<typeof setTimeout>),
};

/** setTimeout's ceiling (~24.8 days); longer waits are re-armed. */
const MAX_TIMEOUT_MS = 2_147_483_647;

/**
 * The invite currently on screen, in memory only. Survives switching panel views (so the user can
 * look something up while the other machine joins) but not a reload, and clears itself at expiry.
 */
export class InviteHolder {
  private current: ShownInvite | null = null;
  private timer: unknown = null;
  private readonly listeners = new Set<() => void>();

  constructor(private readonly timers: Timers = realTimers) {}

  get = (): ShownInvite | null => this.current;

  subscribe = (fn: () => void): (() => void) => {
    this.listeners.add(fn);
    return () => {
      this.listeners.delete(fn);
    };
  };

  show(invite: ShownInvite): void {
    if (invite.expiresAt <= this.timers.now()) {
      this.clear();
      return;
    }
    this.current = invite;
    this.arm();
    this.emit();
  }

  /** This hub is joining `deviceId` itself: that pairing must not count as someone using the invite. */
  ignoreDevice(deviceId: string): void {
    const inv = this.current;
    if (!inv || inv.ignoredDeviceIds.includes(deviceId)) return;
    this.current = { ...inv, ignoredDeviceIds: [...inv.ignoredDeviceIds, deviceId] };
    this.emit();
  }

  clear(): void {
    this.disarm();
    if (this.current === null) return;
    this.current = null;
    this.emit();
  }

  private arm(): void {
    this.disarm();
    const inv = this.current;
    if (!inv) return;
    const wait = Math.min(MAX_TIMEOUT_MS, Math.max(0, inv.expiresAt - this.timers.now()));
    this.timer = this.timers.setTimeout(() => {
      this.timer = null;
      if (this.current !== inv) return;
      if (inv.expiresAt <= this.timers.now()) this.clear();
      else this.arm();
    }, wait);
  }

  private disarm(): void {
    if (this.timer !== null) this.timers.clearTimeout(this.timer);
    this.timer = null;
  }

  private emit(): void {
    for (const fn of [...this.listeners]) fn();
  }
}

export const inviteHolder = new InviteHolder();

export function useShownInvite(holder: InviteHolder = inviteHolder): ShownInvite | null {
  return useSyncExternalStore(holder.subscribe, holder.get, holder.get);
}

/**
 * The device that paired using the shown invite, if any: one that is new since the invite was made
 * (or re-paired after it), and not one this hub joined itself. Invites are single use.
 */
export function inviteUsedBy(invite: ShownInvite, devices: readonly Device[]): Device | undefined {
  const known = new Set(invite.knownDeviceIds);
  const ignored = new Set(invite.ignoredDeviceIds);
  return devices.find((d) => !ignored.has(d.id) && (!known.has(d.id) || d.pairedAt >= invite.createdAt));
}

/**
 * Copy text to the clipboard. Uses the async Clipboard API, then falls back to a hidden
 * textarea + execCommand for webviews that do not expose it. Resolves false if both fail.
 */
export async function copyText(text: string): Promise<boolean> {
  try {
    if (typeof navigator !== "undefined" && navigator.clipboard?.writeText) {
      await navigator.clipboard.writeText(text);
      return true;
    }
  } catch {
    /* fall through */
  }
  if (typeof document === "undefined") return false;
  const ta = document.createElement("textarea");
  ta.value = text;
  ta.setAttribute("readonly", "");
  ta.style.position = "fixed";
  ta.style.opacity = "0";
  document.body.appendChild(ta);
  try {
    ta.select();
    return document.execCommand("copy");
  } catch {
    return false;
  } finally {
    ta.remove();
  }
}
