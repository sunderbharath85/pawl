// Device side of pairing (CONTRACTS.md "Relay"):
//   1. connect `wss://<relay>/v1/connect?role=pairing&hubId=..`
//   2. send `{t:"pair_request", deviceId, devicePk, deviceName, code}`
//   3. hub answers `{t:"pair_ok", deviceId, nonce, box}`; box = nacl.box(hubSk → devicePk) of a small JSON
//      welcome. Opening it with the QR's hub key proves the answer came from that hub (not the relay).
import nacl from "tweetnacl";
import { BoxChannel, encodeBase64, generateKeyPair } from "./crypto";
import { decodeBase64, toHex } from "./encoding";
import { parseRelayFrame, relayConnectUrl, type PairRequestFrame } from "./frames";
import type { PairingOffer } from "./pairing-link";

/** Decrypted `pair_ok` payload. All fields optional: the hub may send `{}`. */
export interface PairWelcome {
  hubName?: string;
  /** Token to put in the protocol `hello` (if the hub wants one over the relay). */
  token?: string;
  hubVersion?: string;
  [k: string]: unknown;
}

export interface PairedHub {
  hubId: string;
  hubName: string;
  relay: string;
  hubPk: string;
  deviceId: string;
  devicePk: string;
  deviceSk: string;
  token?: string;
  pairedAt: number;
}

export class PairingError extends Error {
  readonly code: string;
  constructor(code: string, message: string) {
    super(message);
    this.name = "PairingError";
    this.code = code;
  }
}

const FRIENDLY: Record<string, string> = {
  bad_code: "The hub rejected the pairing code. Show a fresh QR code on your desktop and scan again.",
  invalid_code: "The hub rejected the pairing code. Show a fresh QR code on your desktop and scan again.",
  code_expired: "This pairing code expired (codes last 5 minutes). Show a fresh QR code and scan again.",
  expired: "This pairing code expired (codes last 5 minutes). Show a fresh QR code and scan again.",
  hub_offline: "Your hub is not connected to the relay right now. Make sure the desktop hub is running.",
  not_found: "The relay does not know this hub. Make sure the desktop hub is running with the relay enabled.",
  rate_limited: "Too many pairing attempts. Wait a minute and try again.",
};

/** 16 random bytes as hex — the device's routing id for this hub. */
export function newDeviceId(): string {
  return toHex(nacl.randomBytes(16));
}

export interface PairOptions {
  deviceName: string;
  timeoutMs?: number;
  WebSocketImpl?: new (url: string) => WebSocket;
  signal?: { aborted: boolean; addEventListener?: (type: "abort", fn: () => void) => void };
}

export function pairWithHub(offer: PairingOffer, opts: PairOptions): Promise<PairedHub> {
  const Ctor = opts.WebSocketImpl ?? (globalThis as { WebSocket?: new (url: string) => WebSocket }).WebSocket;
  if (!Ctor) return Promise.reject(new PairingError("no_websocket", "WebSocket is not available."));

  const keys = generateKeyPair();
  const deviceId = newDeviceId();
  const devicePk = encodeBase64(keys.publicKey);
  const channel = new BoxChannel(decodeBase64(offer.hubPk), keys.secretKey);

  return new Promise<PairedHub>((resolve, reject) => {
    let settled = false;
    let ws: WebSocket;
    try {
      ws = new Ctor(relayConnectUrl(offer.relay, { role: "pairing", hubId: offer.hubId, deviceId }));
    } catch (err) {
      reject(new PairingError("bad_relay", err instanceof Error ? err.message : String(err)));
      return;
    }

    const done = (err: PairingError | null, value?: PairedHub) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      try {
        ws.close(1000, err ? "pairing failed" : "paired");
      } catch {
        /* ignore */
      }
      if (err) reject(err);
      else resolve(value!);
    };

    const timer = setTimeout(
      () => done(new PairingError("timeout", "The hub did not answer. Check that the desktop hub is running and online, then try again.")),
      opts.timeoutMs ?? 30_000,
    );
    if (opts.signal) {
      if (opts.signal.aborted) {
        done(new PairingError("aborted", "Pairing cancelled."));
        return;
      }
      opts.signal.addEventListener?.("abort", () => done(new PairingError("aborted", "Pairing cancelled.")));
    }

    ws.onopen = () => {
      const req: PairRequestFrame = {
        t: "pair_request",
        deviceId,
        devicePk,
        deviceName: opts.deviceName.slice(0, 64),
        code: offer.code,
      };
      ws.send(JSON.stringify(req));
    };
    ws.onmessage = (ev: { data: unknown }) => {
      if (typeof ev.data !== "string") return;
      const frame = parseRelayFrame(ev.data);
      if (!frame) return;
      if (frame.t === "error") {
        const code = frame.code ?? "error";
        done(new PairingError(code, FRIENDLY[code] ?? frame.message ?? "Pairing failed."));
        return;
      }
      if (frame.t !== "pair_ok" || frame.deviceId !== deviceId) return;
      const welcome = channel.openJson<PairWelcome>(frame);
      if (!welcome || typeof welcome !== "object") {
        done(
          new PairingError(
            "bad_hub_signature",
            "The pairing answer could not be verified with the hub key from the QR code. Pairing was stopped for your safety.",
          ),
        );
        return;
      }
      const hubName = typeof welcome.hubName === "string" && welcome.hubName.trim() ? welcome.hubName.trim().slice(0, 64) : "My hub";
      done(null, {
        hubId: offer.hubId,
        hubName,
        relay: offer.relay,
        hubPk: offer.hubPk,
        deviceId,
        devicePk,
        deviceSk: encodeBase64(keys.secretKey),
        ...(typeof welcome.token === "string" && welcome.token ? { token: welcome.token } : {}),
        pairedAt: Date.now(),
      });
    };
    ws.onerror = (ev: unknown) => {
      const message = (ev as { message?: unknown } | null)?.message;
      done(new PairingError("network", typeof message === "string" && message ? `Could not reach the relay: ${message}` : "Could not reach the relay."));
    };
    ws.onclose = (ev: { code?: number; reason?: string }) => {
      done(new PairingError("closed", ev?.reason ? `The relay closed the connection: ${ev.reason}` : "The relay closed the connection before the hub answered."));
    };
  });
}
