// Paired devices (PLAN.md migration step 7): phones next to hubs, the group roster ("Added by …"), and the state of
// a pairing invite on screen (single use, expiry). Pure, no I/O. Invite URIs hold secrets (the one-time code, the relay
// pairing token and the relay access key): nothing here logs, stores or returns them.
import type { AckResults, Device, DeviceKind, MachineId } from "@krypto/protocol";
import { machineDisplayName } from "./machine";
import { parseInviteUri, relayStatusText } from "./peer";
import type { HubState } from "./state";

/** The protocol version that pairs phones (`create_invite.forPhone`, `Device.os`, `Device.introducedBy`). */
export const PHONE_PROTOCOL_VERSION = 8;

// ---------------------------------------------------------------- the roster

export function isPhone(device: Pick<Device, "kind">): boolean {
  return device.kind === "phone";
}

/** "ios" / "android" for a phone's icon, or null when the hub does not know (an older hub, or not a phone). */
export function phoneOs(device: Pick<Device, "kind" | "os">): "ios" | "android" | null {
  if (device.kind !== "phone") return null;
  return device.os === "ios" || device.os === "android" ? device.os : null;
}

/** Online first, then by name, then by id (stable). */
function byPresenceThenName(a: Device, b: Device): number {
  return Number(!!b.path) - Number(!!a.path) || a.name.localeCompare(b.name) || (a.id < b.id ? -1 : a.id > b.id ? 1 : 0);
}

export interface DeviceGroups {
  /** Paired hubs (machines). */
  hubs: Device[];
  /** Paired phones: listed as devices, never as machines. */
  phones: Device[];
  /** Kinds this client does not know yet (from a newer hub). */
  other: Device[];
}

/** The pairing roster split by kind, each list sorted (online first, then name). This hub is never in it. */
export function groupDevices(devices: readonly Device[]): DeviceGroups {
  const out: DeviceGroups = { hubs: [], phones: [], other: [] };
  for (const d of devices) {
    if (d.kind === "hub") out.hubs.push(d);
    else if (d.kind === "phone") out.phones.push(d);
    else out.other.push(d);
  }
  out.hubs.sort(byPresenceThenName);
  out.phones.sort(byPresenceThenName);
  out.other.sort(byPresenceThenName);
  return out;
}

/** Paired phones, online first. */
export function pairedPhones(state: Pick<HubState, "devices">): Device[] {
  return groupDevices(state.devices).phones;
}

/** How many phones are paired and how many are linked right now (a phone is online while it has a link path). */
export function phoneCounts(state: Pick<HubState, "devices">): { total: number; online: number } {
  const phones = state.devices.filter(isPhone);
  return { total: phones.length, online: phones.filter((p) => !!p.path).length };
}

/** "No phones paired" / "1 phone" / "2 phones · 1 online" (`online` omitted when unknown, e.g. hub offline). */
export function phonesLine(total: number, online?: number): string {
  if (total === 0) return "No phones paired";
  const n = `${total} phone${total === 1 ? "" : "s"}`;
  return online === undefined ? n : `${n} · ${online} online`;
}

/** The name of the paired machine `id` (a machine record first, then the roster), or null when unknown. */
function machineName(state: Pick<HubState, "machines" | "devices">, id: MachineId): string | null {
  const m = state.machines[id];
  if (m?.name?.trim()) return machineDisplayName(m);
  const d = state.devices.find((x) => x.id === id);
  return d?.name?.trim() || null;
}

/**
 * "Added by Mac mini" when another paired hub introduced this device (group roster), or null when it paired here
 * directly. An introducer that is no longer paired is named by its id.
 */
export function introducedByText(
  device: Pick<Device, "introducedBy">,
  state: Pick<HubState, "machine" | "machines" | "devices">,
): string | null {
  const by = device.introducedBy;
  if (!by || by === state.machine?.id) return null;
  const name = machineName(state, by);
  return name ? `Added by ${name}` : `Added by machine ${by} (no longer paired)`;
}

// ---------------------------------------------------------------- an invite on screen

/** A pairing invite on screen. Its URI is a secret: keep it in memory only (never log or persist it). */
export interface PendingInvite {
  uri: string;
  /** Unix ms by the hub's clock (the same machine). */
  expiresAt: number;
  /** Local time the invite was created (to spot the pairing it led to). */
  createdAt: number;
  /** Roster ids before the invite existed: a new id afterwards means someone used it. */
  knownDeviceIds: readonly string[];
  /** Devices this hub joined itself while the invite was up (those pairings did not use it). */
  ignoredDeviceIds: readonly string[];
  /** A phone (QR) invite: only a new phone can use it. A machine invite is only used by a new hub. */
  forPhone?: boolean | undefined;
}

/**
 * The device that paired using the shown invite, if any: one of the invite's kind that is new since the invite was
 * made (or re-paired after it), not one this hub joined itself, and not one another hub introduced (intros arrive
 * without an invite). Invites are single use.
 */
export function inviteUsedBy(invite: PendingInvite, devices: readonly Device[]): Device | undefined {
  const known = new Set(invite.knownDeviceIds);
  const ignored = new Set(invite.ignoredDeviceIds);
  const kind: DeviceKind = invite.forPhone ? "phone" : "hub";
  return devices.find(
    (d) => d.kind === kind && !d.introducedBy && !ignored.has(d.id) && (!known.has(d.id) || d.pairedAt >= invite.createdAt),
  );
}

export type InviteState = { state: "waiting"; msLeft: number } | { state: "used"; by: Device } | { state: "expired" };

/** Where a shown invite stands: still waiting (with the time left), used by a device, or expired. */
export function inviteState(invite: PendingInvite, devices: readonly Device[], now: number): InviteState {
  const by = inviteUsedBy(invite, devices);
  if (by) return { state: "used", by };
  const msLeft = invite.expiresAt - now;
  return msLeft > 0 ? { state: "waiting", msLeft } : { state: "expired" };
}

// ---------------------------------------------------------------- phone (QR) invites

/** True for a relay-only invite (a relay, no LAN address): the kind a phone's QR scanner can use. */
export function isPhoneInvite(uri: string): boolean {
  const info = parseInviteUri(uri);
  return !!info && info.addr === null && info.relay !== null;
}

/** Shown next to every phone QR code (it carries the relay access key, `rk`). */
export const PHONE_QR_WARNING =
  "This QR code contains your relay access key. Anyone who sees it (over your shoulder, in a screenshot or a screen share) " +
  "can use your relay until you rotate the key, and can pair a phone with this machine until the code is used or expires.";

export type PhonePairingBlockCode = "disconnected" | "old_hub" | "relay_off" | "relay_down";

/**
 * Why this hub cannot make a phone invite right now, or null when it can: phones pair and connect through the relay
 * only, so it must be set up and connected. `connected` = this client's hub link.
 */
export function phonePairingBlock(
  state: Pick<HubState, "protocolVersion" | "peer">,
  connected: boolean,
): { code: PhonePairingBlockCode; text: string } | null {
  if (!connected) return { code: "disconnected", text: "Connect to the hub first." };
  const v = state.protocolVersion;
  if ((v !== undefined && v < PHONE_PROTOCOL_VERSION) || !state.peer) {
    return { code: "old_hub", text: "This hub can't pair phones yet. Update krypto on this machine and restart the hub." };
  }
  const relay = state.peer.relay;
  if (!relay?.url) {
    return {
      code: "relay_off",
      text: "Phones pair and stay in touch through your relay, and this hub has none set up. Add the relay URL and access key in Settings first.",
    };
  }
  if (!relay.connected) return { code: "relay_down", text: `Phones pair through the relay. ${relayStatusText(relay).text}` };
  return null;
}

// ---------------------------------------------------------------- revoking a hub that introduced devices

/** A device a revoked hub had introduced here (the `revoke_device` ack lists them; protocol v8). */
export type IntroducedDevice = NonNullable<AckResults["revoke_device"]["introduced"]>[number];

/**
 * The devices a just-revoked hub introduced that are still paired here (the ack's `introduced`), cleaned up: entries
 * without an id or name, duplicates and the revoked device itself are dropped. Empty for an older hub (no list).
 */
export function introducedByRevoked(result: AckResults["revoke_device"] | null | undefined, revokedId: string): IntroducedDevice[] {
  const list = result && Array.isArray(result.introduced) ? result.introduced : [];
  const seen = new Set<string>([revokedId]);
  const out: IntroducedDevice[] = [];
  for (const d of list) {
    if (!d || typeof d.id !== "string" || !d.id || seen.has(d.id)) continue;
    seen.add(d.id);
    const name = typeof d.name === "string" && d.name.trim() ? d.name.trim() : d.id;
    out.push({ id: d.id, name, kind: d.kind });
  }
  return out;
}

export interface RevokeIntroducedPrompt {
  title: string;
  body: string;
  /** One line per device: its name and what it is. */
  items: Array<{ title: string; body: string }>;
  confirm: string;
  cancel: string;
}

/** The follow-up question after revoking `hubName`: also remove the devices it added here? */
export function revokeIntroducedPrompt(hubName: string, devices: readonly IntroducedDevice[]): RevokeIntroducedPrompt {
  const n = devices.length;
  const them = n === 1 ? "it" : "them";
  return {
    title: `Also remove the devices ${hubName} added?`,
    body:
      `${hubName} introduced ${n === 1 ? "this device" : `these ${n} devices`} to this machine, and ${n === 1 ? "it is" : "they are"} ` +
      `still paired here: unpairing ${hubName} did not remove ${them}. Remove ${them} too unless you paired ${them} on purpose.`,
    items: devices.map((d) => ({ title: d.name, body: d.kind === "phone" ? "Phone" : d.kind === "hub" ? "Machine" : "Device" })),
    confirm: n === 1 ? "Remove it" : `Remove all ${n}`,
    cancel: n === 1 ? "Keep it" : "Keep them",
  };
}

export interface RevokeManyResult {
  /** Removed now, or already gone (`not_found`). */
  removed: IntroducedDevice[];
  /** Still paired; `code` is the hub's error code (or "disconnected" / "error"). */
  failed: Array<IntroducedDevice & { code: string }>;
}

/** A toast for `revokeDevices`: what was removed, and what is still paired. */
export function revokeManyText(result: RevokeManyResult, hubName: string): { text: string; tone: "info" | "error" } {
  const names = (list: readonly { name: string }[]) => list.map((d) => d.name).join(", ");
  if (result.failed.length === 0) {
    const n = result.removed.length;
    return { text: n === 1 ? `Also removed ${result.removed[0]!.name}` : `Also removed the ${n} devices ${hubName} added`, tone: "info" };
  }
  const done = result.removed.length > 0 ? `Removed ${names(result.removed)}. ` : "";
  return { text: `${done}Couldn't remove ${names(result.failed)}; revoke ${result.failed.length === 1 ? "it" : "them"} in Machines.`, tone: "error" };
}
