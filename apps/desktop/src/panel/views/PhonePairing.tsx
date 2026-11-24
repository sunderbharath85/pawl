// Phones (PLAN.md migration step 7): pair one by QR code (a relay-only invite drawn locally, never by a web service)
// and list the paired phones with their link and who added them. Phones are devices, never machines.
import { useEffect, useMemo, useRef, useState } from "react";
import {
  INVITE_TTL_DEFAULT_SECS,
  OS_LABEL,
  PHONE_QR_WARNING,
  introducedByText,
  inviteUsedBy,
  linkPathLabel,
  maskInviteSecrets,
  pairedPhones,
  pairingAllowsText,
  phoneCounts,
  phoneOs,
  phonePairingBlock,
  phonesLine,
  relayHost,
} from "@krypto/hub-client";
import type { Device } from "@krypto/protocol";
import { clockTime, timeAgo } from "../../lib/format";
import {
  INVITE_TTL_CHOICES,
  formatCountdown,
  pairingErrorText,
  phoneInviteHolder,
  presenceText,
  useShownInvite,
  type ShownInvite,
} from "../../lib/pairing";
import { qrSvg } from "../../lib/qr";
import { useConfirm } from "../Confirm";
import { usePanel } from "../context";
import { CopyField } from "../CopyField";
import { useRevokeDevice } from "../useRevokeDevice";
import * as I from "../icons";
import { useHubSettings } from "./settings/shared";

/** A QR code drawn as SVG (always dark on light, whatever the theme, so phone cameras can read it). */
export function QrCode({ text, label }: { text: string; label: string }) {
  const qr = useMemo(() => {
    try {
      return qrSvg(text);
    } catch {
      return null;
    }
  }, [text]);
  if (!qr) {
    return (
      <p className="field-error" role="alert">
        This invite is too long for a QR code. Copy the link below instead.
      </p>
    );
  }
  return (
    <svg className="qr-code" viewBox={`0 0 ${qr.size} ${qr.size}`} role="img" aria-label={label} shapeRendering="crispEdges">
      <rect width={qr.size} height={qr.size} fill="#fff" />
      <path d={qr.path} fill="#000" />
    </svg>
  );
}

/** iOS / Android / generic phone glyph (plain shapes, not brand logos). */
export function PhoneIcon({ device, size = 20 }: { device: Pick<Device, "kind" | "os">; size?: number }) {
  const os = phoneOs(device);
  const Icon = os === "ios" ? I.PhoneIos : os === "android" ? I.PhoneAndroid : I.Phone;
  return <Icon size={size} />;
}

/**
 * Create a phone invite and show it as a QR code (plus the link, masked, with copy), its countdown and the access key
 * warning. Needs the relay: without it, says so and links to the Relay settings.
 */
export function PhoneInviteCard({ onPaired, bare }: { onPaired?: (phone: Device) => void; bare?: boolean }) {
  const { client, state, meta, now, navigate, toast } = usePanel();
  const hs = useHubSettings();
  const connected = meta.phase === "connected";
  const invite = useShownInvite(phoneInviteHolder);
  const block = phonePairingBlock(state, connected);
  const relayUrl = state.peer?.relay?.url ?? null;
  const [ttl, setTtl] = useState(INVITE_TTL_DEFAULT_SECS);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [note, setNote] = useState<string | null>(null);
  const prev = useRef<ShownInvite | null>(invite);

  // Single use: once a new phone pairs with it, the code is spent.
  useEffect(() => {
    if (!invite) return;
    const used = inviteUsedBy(invite, state.devices);
    if (!used) return;
    phoneInviteHolder.clear();
    setNote(`${used.name} is paired with this machine. Your other paired machines that accept introductions add it too.`);
    toast(`Paired ${used.name}`);
    onPaired?.(used);
  }, [invite, state.devices, toast, onPaired]);

  // The holder drops the code at its expiry; say so instead of silently emptying the card.
  useEffect(() => {
    const before = prev.current;
    prev.current = invite;
    if (before && !invite && before.expiresAt <= Date.now() + 1000) {
      setNote((n) => n ?? "That code expired. Show a new one if you still need it.");
    }
  }, [invite]);

  const create = async () => {
    if (busy) return;
    setBusy(true);
    setError(null);
    setNote(null);
    const createdAt = Date.now();
    const knownDeviceIds = client.getState().devices.map((d) => d.id);
    try {
      const res = await client.createInvite({ forPhone: true, ttlSecs: ttl });
      phoneInviteHolder.show({ uri: res.uri, expiresAt: res.expiresAt, createdAt, knownDeviceIds, ignoredDeviceIds: [], forPhone: true });
    } catch (err) {
      setError(pairingErrorText(err, "create_phone_invite"));
    } finally {
      setBusy(false);
    }
  };

  const relayLink = (
    <button className="link" onClick={() => navigate({ view: "settings", section: "relay" })}>
      Relay settings
    </button>
  );

  if (invite) {
    const left = invite.expiresAt - now;
    return (
      <div className={`${bare ? "" : "card "}invite-card phone-card`}>
        <div className="invite-head">
          <h3>Scan with your phone</h3>
          <span className={`invite-timer${left < 60_000 ? " is-ending" : ""}`} title={`Expires at ${clockTime(invite.expiresAt)}`}>
            expires in {formatCountdown(invite.expiresAt, now)}
          </span>
        </div>
        <p className="hint">Open Krypto on your phone, tap Pair and point the camera at this code.</p>
        <div className="qr-frame">
          <QrCode text={invite.uri} label="Pairing code for a phone" />
        </div>
        <p className="qr-warning" role="note">
          <I.Shield size={14} /> <span>{PHONE_QR_WARNING}</span>
        </p>
        <p className="hint">
          Single use: it pairs one phone, until {clockTime(invite.expiresAt)}. Can't scan? Copy the link and paste it into Krypto on the
          phone, but only send it through something private.
        </p>
        <CopyField label="Phone invite link" text={invite.uri} shown={maskInviteSecrets(invite.uri)} />
        {error && (
          <span className="field-error" role="alert">
            {error}
          </span>
        )}
        <div className="form-actions">
          <button
            className="pill pill-ghost"
            title="Hides it here. The hub keeps it valid until it expires or a phone uses it."
            onClick={() => phoneInviteHolder.clear()}
          >
            Hide
          </button>
          <button className="pill" disabled={!connected || busy || !!block} onClick={() => void create()}>
            New code
          </button>
        </div>
      </div>
    );
  }

  return (
    <div className={`${bare ? "" : "card "}invite-card phone-card`}>
      <h3>Pair a phone</h3>
      {block ? (
        <div className="listener-help">
          <span className="hint">{block.text}</span>
          {(block.code === "relay_off" || block.code === "relay_down") && relayLink}
        </div>
      ) : (
        <>
          <p className="hint">
            Show a one-time QR code here and scan it with the Krypto app. The phone links through your relay{" "}
            <span className="mono">{relayUrl ? relayHost(relayUrl) : ""}</span>, so it works away from this network too.
          </p>
          <label className="field">
            <span className="field-label">Valid for</span>
            <select className="input select" value={ttl} disabled={!connected || busy} onChange={(e) => setTtl(Number(e.target.value))}>
              {INVITE_TTL_CHOICES.map((c) => (
                <option key={c.secs} value={c.secs}>
                  {c.label}
                </option>
              ))}
            </select>
          </label>
          <p className="hint">The code includes your relay access key, so show it only where no one else can see your screen.</p>
        </>
      )}
      {error && (
        <span className="field-error" role="alert">
          {error}
        </span>
      )}
      {note && !error && <span className="hint invite-note">{note}</span>}
      {!block && (
        <p className="pairing-allows">
          {pairingAllowsText(hs.settings?.peer.acceptIntents, "phone")}{" "}
          <button className="link" onClick={() => navigate({ view: "settings", section: "remote" })}>
            Remote control settings
          </button>
        </p>
      )}
      <div className="form-actions">
        <button className="pill pill-primary" disabled={!connected || busy || !!block} onClick={() => void create()}>
          {busy ? "Creating…" : "Show QR code"}
        </button>
      </div>
    </div>
  );
}

/** The paired phones (never listed as machines): OS, presence, LAN / relay link, who added it, and revoke. */
export function PhoneList() {
  const { state, meta, now } = usePanel();
  const connected = meta.phase === "connected";
  const phones = pairedPhones(state);
  const counts = phoneCounts(state);
  return (
    <>
      <h2 className="section-title">Phones</h2>
      <p className="hint section-hint">{phonesLine(counts.total, connected ? counts.online : undefined)}.</p>
      <div className="machine-list" aria-label="Phones">
        {phones.map((p) => (
          <PhoneRow key={p.id} phone={p} connected={connected} now={now} />
        ))}
        {phones.length === 0 && (
          <div className="machine-empty">No phones yet. Pair one to check on your pets and answer approvals from anywhere.</div>
        )}
      </div>
    </>
  );
}

function PhoneRow({ phone, connected, now }: { phone: Device; connected: boolean; now: number }) {
  const { state } = usePanel();
  const revokeDevice = useRevokeDevice();
  const confirm = useConfirm();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const online = connected && !!phone.path;
  const via = online ? linkPathLabel(phone.path) : null;
  const os = phoneOs(phone);
  const added = introducedByText(phone, state);
  const presence = connected ? presenceText({ online: !!phone.path, lastSeen: phone.lastSeen ?? 0 }, now) : "Unknown while the hub is offline";

  const revoke = async () => {
    if (busy) return;
    const ok = await confirm.ask({
      title: `Unpair ${phone.name}?`,
      body:
        `${phone.name} can no longer reach this machine, directly or through the relay, until you pair it again, and it is told it ` +
        "was removed. Other machines it is paired with keep it until you unpair it there too.",
      confirmLabel: "Revoke",
      tone: "danger",
    });
    if (!ok) return;
    setBusy(true);
    setError(null);
    try {
      await revokeDevice({ id: phone.id, name: phone.name });
      // The row disappears with the hub's `devices` broadcast.
    } catch (err) {
      setError(pairingErrorText(err, "revoke_device"));
      setBusy(false);
    }
  };

  return (
    <div className={`card machine-row phone-row${online ? " is-online" : ""}`}>
      <span className={`dot${online ? " tone-done" : ""}`} title={online ? "Online" : "Offline"} aria-hidden />
      <span className="phone-icon" title={os ? OS_LABEL[os] : "Phone"}>
        <PhoneIcon device={phone} />
      </span>
      <div className="machine-row-main">
        <div className="machine-row-title">
          <strong>{phone.name}</strong>
          <span className="chip chip-soft">{os ? OS_LABEL[os] : "Phone"}</span>
          {via && (
            <span
              className={`chip link-chip link-${phone.path}`}
              title={phone.path === "relay" ? "Linked through the relay (end-to-end encrypted)" : "Linked directly on your network"}
            >
              {via}
            </span>
          )}
        </div>
        <div className="machine-row-sub">
          <span className={online ? "presence-online" : undefined}>{presence}</span>
          <span aria-hidden>·</span>
          <span className="muted" title={new Date(phone.pairedAt).toLocaleString()}>
            paired {timeAgo(phone.pairedAt, now)}
          </span>
          {added && (
            <>
              <span aria-hidden>·</span>
              <span className="muted" title="Another paired machine introduced this phone">
                {added}
              </span>
            </>
          )}
        </div>
        {error && (
          <span className="field-error" role="alert">
            {error}
          </span>
        )}
      </div>
      <button className="pill pill-ghost machine-row-action" disabled={!connected || busy} onClick={() => void revoke()}>
        {busy ? "Revoking…" : "Revoke"}
      </button>
    </div>
  );
}
