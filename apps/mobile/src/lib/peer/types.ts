// Everything the platform-free peer core needs from its host, injected so it runs on React Native (Expo adapters
// in src/lib/expo/) and under plain bun (tests, headless smoke scripts).

/** `n` bytes from a CSPRNG (expo-crypto on the phone, `crypto.getRandomValues` under bun). */
export type RandomBytes = (n: number) => Uint8Array;

export type TimerHandle = unknown;

/** Wall clock + timers. Tests inject a fake one. */
export interface Clock {
  now(): number;
  setTimeout(fn: () => void, ms: number): TimerHandle;
  clearTimeout(handle: TimerHandle): void;
}

export const systemClock: Clock = {
  now: () => Date.now(),
  setTimeout: (fn, ms) => setTimeout(fn, ms),
  clearTimeout: (h) => clearTimeout(h as ReturnType<typeof setTimeout>),
};

/** The part of the WebSocket API the relay client uses (React Native, browsers and bun all provide it). */
export interface SocketLike {
  readonly readyState: number;
  send(data: string): void;
  close(code?: number, reason?: string): void;
  onopen: ((ev: unknown) => void) | null;
  onmessage: ((ev: { data: unknown }) => void) | null;
  onclose: ((ev: { code?: number; reason?: string }) => void) | null;
  onerror: ((ev: unknown) => void) | null;
}

export type SocketFactory = (url: string) => SocketLike;

export const SOCKET_OPEN = 1;

/** The global WebSocket as a factory. */
export const globalSocketFactory: SocketFactory = (url) => {
  const Ctor = (globalThis as unknown as { WebSocket?: new (u: string) => SocketLike }).WebSocket;
  if (!Ctor) throw new Error("no WebSocket implementation available");
  return new Ctor(url);
};

/**
 * Small async key/value store. On the phone: expo-secure-store (Keychain / Keystore); values stay small (< 2 KB
 * each), so the core splits its state over several keys. Keys match /^[A-Za-z0-9._-]+$/.
 */
export interface KeyValueStore {
  get(key: string): Promise<string | null>;
  set(key: string, value: string): Promise<void>;
  delete(key: string): Promise<void>;
}

/** In-memory store (tests, headless scripts that do not need to survive a restart). */
export class MemoryStore implements KeyValueStore {
  readonly data = new Map<string, string>();
  async get(key: string): Promise<string | null> {
    return this.data.get(key) ?? null;
  }
  async set(key: string, value: string): Promise<void> {
    this.data.set(key, value);
  }
  async delete(key: string): Promise<void> {
    this.data.delete(key);
  }
}

/** `crypto.getRandomValues` (bun, browsers, and React Native once expo-crypto's polyfill is installed). */
export const webRandomBytes: RandomBytes = (n) => {
  const out = new Uint8Array(n);
  const c = (globalThis as unknown as { crypto?: { getRandomValues(a: Uint8Array): Uint8Array } }).crypto;
  if (!c?.getRandomValues) throw new Error("no CSPRNG available");
  for (let i = 0; i < n; i += 65536) c.getRandomValues(out.subarray(i, Math.min(n, i + 65536)));
  return out;
};

/** Optional diagnostics sink. Never receives payloads, keys, codes or tokens: only short reasons. */
export type Logger = (level: "debug" | "info" | "warn", msg: string, fields?: Record<string, string | number | boolean>) => void;

export const silentLogger: Logger = () => {};
