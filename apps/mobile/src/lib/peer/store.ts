// What the phone persists, split over small keys (expo-secure-store values should stay under ~2 KB):
//   pawl.sk           device secret key (64 hex chars). Never leaves the device.
//   pawl.identity     { deviceId, name, os, createdAt }
//   pawl.relay        { url, accessKey }          (the access key is a secret)
//   pawl.hubs         [hub ids]
//   pawl.hub.<id>     StoredHub
//   pawl.seq.<id>     { rx, tx }                  last accepted / last sent envelope seq for that hub
//   pawl.revoked      { ids, pks }                devices that revoked us or that we removed (never re-added by intros)
//   pawl.settings     { pushEnabled, pushToken, pushClear }
// Writes are coalesced (write-behind, a few ms) and serialized; `flush()` waits for them.
import type { MachineId, MachineOs, UnixMs } from "@pawl/protocol";
import { fromHex, toHex } from "./b64";
import type { Clock, KeyValueStore, TimerHandle } from "./types";

export const K_SECRET = "pawl.sk";
export const K_IDENTITY = "pawl.identity";
export const K_RELAY = "pawl.relay";
export const K_HUBS = "pawl.hubs";
export const K_REVOKED = "pawl.revoked";
export const K_SETTINGS = "pawl.settings";
export const hubKey = (id: string) => `pawl.hub.${id}`;
export const seqKey = (id: string) => `pawl.seq.${id}`;

export const REVOKED_MAX = 64;

export interface StoredIdentity {
  deviceId: string;
  name: string;
  os: MachineOs;
  createdAt: UnixMs;
}

export interface StoredRelay {
  url: string;
  accessKey: string | null;
}

export interface StoredHub {
  id: MachineId;
  name: string;
  os?: MachineOs;
  hubVersion?: string;
  /** Base64 (standard) X25519 public key. */
  publicKey: string;
  pairedAt: UnixMs;
  lastSeen?: UnixMs;
  addr?: string;
  /** The hub that introduced this one (group roster). */
  introducedBy?: MachineId;
}

export interface StoredSeq {
  rx: number;
  tx: number;
}

export interface StoredRevoked {
  ids: string[];
  pks: string[];
}

export interface StoredSettings {
  pushEnabled: boolean;
  pushToken: string | null;
  /** Push was turned off (or the token dropped): the relay must still be told `unregister_push`. */
  pushClear: boolean;
}

export interface LoadedState {
  secret: Uint8Array | null;
  identity: StoredIdentity | null;
  relay: StoredRelay | null;
  hubs: { hub: StoredHub; seq: StoredSeq }[];
  revoked: StoredRevoked;
  settings: StoredSettings;
}

function parse<T>(raw: string | null): T | null {
  if (raw === null) return null;
  try {
    return JSON.parse(raw) as T;
  } catch {
    return null;
  }
}

const safeSeq = (n: unknown) => (typeof n === "number" && Number.isSafeInteger(n) && n >= 0 ? n : 0);

export class PhoneStore {
  readonly #kv: KeyValueStore;
  readonly #clock: Clock;
  readonly #pending = new Map<string, string | null>();
  #timer: TimerHandle | null = null;
  #chain: Promise<void> = Promise.resolve();
  #lastError: unknown = null;

  constructor(kv: KeyValueStore, clock: Clock) {
    this.#kv = kv;
    this.#clock = clock;
  }

  get lastError(): unknown {
    return this.#lastError;
  }

  async load(): Promise<LoadedState> {
    const [sk, identity, relay, ids, revoked, settings] = await Promise.all([
      this.#kv.get(K_SECRET),
      this.#kv.get(K_IDENTITY),
      this.#kv.get(K_RELAY),
      this.#kv.get(K_HUBS),
      this.#kv.get(K_REVOKED),
      this.#kv.get(K_SETTINGS),
    ]);
    const hubIds = (parse<unknown[]>(ids) ?? []).filter((x): x is string => typeof x === "string");
    const hubs: LoadedState["hubs"] = [];
    for (const id of hubIds) {
      const [h, s] = await Promise.all([this.#kv.get(hubKey(id)), this.#kv.get(seqKey(id))]);
      const hub = parse<StoredHub>(h);
      if (!hub || hub.id !== id || typeof hub.publicKey !== "string") continue;
      const seq = parse<StoredSeq>(s);
      hubs.push({ hub, seq: { rx: safeSeq(seq?.rx), tx: safeSeq(seq?.tx) } });
    }
    const secret = sk ? fromHex(sk.trim()) : null;
    const rev = parse<StoredRevoked>(revoked);
    const set = parse<StoredSettings>(settings);
    return {
      secret: secret && secret.length === 32 ? secret : null,
      identity: parse<StoredIdentity>(identity),
      relay: parse<StoredRelay>(relay),
      hubs,
      revoked: {
        ids: Array.isArray(rev?.ids) ? rev.ids.filter((x) => typeof x === "string") : [],
        pks: Array.isArray(rev?.pks) ? rev.pks.filter((x) => typeof x === "string") : [],
      },
      settings: {
        pushEnabled: set?.pushEnabled !== false,
        pushToken: typeof set?.pushToken === "string" ? set.pushToken : null,
        pushClear: set?.pushClear === true,
      },
    };
  }

  /** Write the secret now (not coalesced): it must exist before anything is sealed with it. */
  async saveSecret(secret: Uint8Array): Promise<void> {
    await this.#kv.set(K_SECRET, toHex(secret));
  }

  /** Queue a write (null = delete). */
  put(key: string, value: unknown | null): void {
    this.#pending.set(key, value === null ? null : JSON.stringify(value));
    if (this.#timer === null) this.#timer = this.#clock.setTimeout(() => void this.flush(), 30);
  }

  putHubs(ids: string[]): void {
    this.put(K_HUBS, ids);
  }

  /** Write every queued change; resolves once they are stored. */
  flush(): Promise<void> {
    if (this.#timer !== null) {
      this.#clock.clearTimeout(this.#timer);
      this.#timer = null;
    }
    if (this.#pending.size === 0) return this.#chain;
    const batch = [...this.#pending];
    this.#pending.clear();
    this.#chain = this.#chain.then(async () => {
      for (const [key, value] of batch) {
        try {
          if (value === null) await this.#kv.delete(key);
          else await this.#kv.set(key, value);
        } catch (e) {
          // Keep going: one failed key must not lose the others. Surfaced through `lastError`; the value is queued
          // again (unless something newer replaced it) so the next flush retries it.
          this.#lastError = e;
          if (!this.#pending.has(key)) this.#pending.set(key, value);
        }
      }
    });
    return this.#chain;
  }

  /** Delete everything the phone stored (unpair this phone). */
  async wipe(hubIds: string[]): Promise<void> {
    this.#pending.clear();
    if (this.#timer !== null) this.#clock.clearTimeout(this.#timer);
    this.#timer = null;
    await this.#chain;
    const keys = [K_SECRET, K_IDENTITY, K_RELAY, K_HUBS, K_REVOKED, K_SETTINGS, ...hubIds.flatMap((id) => [hubKey(id), seqKey(id)])];
    for (const k of keys) {
      try {
        await this.#kv.delete(k);
      } catch (e) {
        this.#lastError = e;
      }
    }
  }
}
