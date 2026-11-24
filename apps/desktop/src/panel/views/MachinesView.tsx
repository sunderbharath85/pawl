// Machines: this hub and every paired hub (online state, OS, last seen, address), pairing by
// invite, and revoking. PLAN.md migration step 2. Paired machines' pets arrive in step 3.
import { useEffect, useMemo, useRef, useState, type KeyboardEvent } from "react";
import {
  INVITE_TTL_DEFAULT_SECS,
  extractInviteUri,
  isUnspecifiedAddr,
  machineCounts,
  parseInviteUri,
  sortedMachines,
} from "@krypto/hub-client";
import { PEER_DEFAULT_PORT, type Device, type MachineOs } from "@krypto/protocol";
import { clockTime, timeAgo } from "../../lib/format";
import { OS_LABEL, machineDisplayName } from "../../lib/machine";
import { isTauri, native } from "../../lib/native";
import {
  INVITE_TTL_CHOICES,
  copyText,
  formatCountdown,
  inviteHolder,
  inviteUsedBy,
  joinCommand,
  machinesLine,
  pairingErrorText,
  presenceText,
  useShownInvite,
  type ShownInvite,
} from "../../lib/pairing";
import { usePanel } from "../context";
import * as I from "../icons";

/** One row: this hub, a paired hub, or a roster device with no machine (a phone, later). */
interface Row {
  id: string;
  name: string;
  self: boolean;
  kind: Device["kind"];
  os?: MachineOs | undefined;
  online: boolean;
  lastSeen?: number | undefined;
  addr?: string | null | undefined;
  pairedAt?: number | undefined;
  hubVersion?: string | undefined;
}

export function MachinesView() {
  const { state, meta, now } = usePanel();
  const connected = meta.phase === "connected";
  const ownId = state.machine?.id;

  const rows = useMemo<Row[]>(() => {
    const devices = new Map(state.devices.map((d) => [d.id, d]));
    const out: Row[] = sortedMachines(state).map((m) => {
      const self = m.id === ownId;
      const device = devices.get(m.id);
      return {
        id: m.id,
        name: machineDisplayName(m),
        self,
        kind: "hub",
        os: m.os,
        online: self ? connected : m.online,
        lastSeen: m.lastSeen || device?.lastSeen,
        addr: self ? (state.peer?.listening ?? null) : device?.addr,
        pairedAt: device?.pairedAt,
        hubVersion: m.hubVersion,
      };
    });
    for (const d of state.devices) {
      if (d.id in state.machines || d.id === ownId) continue;
      out.push({ id: d.id, name: d.name, self: false, kind: d.kind, online: false, lastSeen: d.lastSeen, addr: d.addr, pairedAt: d.pairedAt });
    }
    return out;
  }, [state, ownId, connected]);

  const counts = machineCounts(state);
  const peers = rows.filter((r) => !r.self).length;
  const pairing = !!state.peer; // a protocol v3 hub

  return (
    <section className="view">
      <header className="view-head">
        <h1>Machines</h1>
        <p>
          {counts.total > 0 ? `${machinesLine(counts.total, counts.online, connected)}. ` : ""}
          Pair the Krypto hubs on your network so they can see each other. Pets from other machines come over in a later update.
        </p>
      </header>

      {connected && !pairing && (
        <div className="card notice-card">
          <strong>This hub can't pair yet.</strong> Update krypto on this machine and restart the hub to pair machines.
        </div>
      )}

      <div className="machine-list" aria-label="Machines">
        {rows.map((r) => (
          <MachineRow key={r.id} row={r} connected={connected} now={now} stale={!connected && !r.self} />
        ))}
        {rows.length === 0 && <div className="machine-empty">Waiting for the hub to introduce this machine…</div>}
        {rows.length > 0 && peers === 0 && (
          <div className="machine-empty">No other machines yet. Create an invite here, or join one from another machine.</div>
        )}
      </div>

      {pairing && (
        <>
          <h2 className="section-title">Pair a machine</h2>
          <div className="settings-grid pair-grid">
            <InviteCard />
            <JoinCard />
          </div>
        </>
      )}
    </section>
  );
}

function MachineRow({ row, connected, now, stale }: { row: Row; connected: boolean; now: number; stale: boolean }) {
  const { client, toast } = usePanel();
  const [confirming, setConfirming] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const online = row.online && !stale;
  const presence = row.self
    ? connected
      ? "Online"
      : "Hub offline"
    : stale
      ? "Unknown while the hub is offline"
      : presenceText({ online: row.online, lastSeen: row.lastSeen ?? 0 }, now);
  const addr = row.self ? (row.addr ? `listening on ${row.addr}` : "peer listener off") : row.addr || "no address yet";

  const revoke = async () => {
    if (busy) return;
    setBusy(true);
    setError(null);
    try {
      await client.revokeDevice(row.id);
      toast(`Unpaired ${row.name}`);
      // The row disappears with the hub's `machine_removed`.
    } catch (err) {
      setError(pairingErrorText(err, "revoke_device"));
      setBusy(false);
    }
  };

  const onConfirmKey = (e: KeyboardEvent) => {
    if (e.key === "Escape" && !busy) setConfirming(false);
  };

  return (
    <div className={`card machine-row${row.self ? " is-self" : ""}${online ? " is-online" : ""}`}>
      <span className={`dot${online ? " tone-done" : ""}`} title={online ? "Online" : "Offline"} aria-hidden />
      <div className="machine-row-main">
        <div className="machine-row-title">
          <strong>{row.name}</strong>
          {row.self && <span className="chip">This machine</span>}
          {row.kind === "phone" && <span className="chip chip-soft">Phone</span>}
          {row.os && <span className="chip chip-soft">{OS_LABEL[row.os] ?? row.os}</span>}
        </div>
        <div className="machine-row-sub">
          <span className={online ? "presence-online" : undefined}>{presence}</span>
          <span aria-hidden>·</span>
          <span className={row.addr ? "mono" : "muted"}>{addr}</span>
          {row.pairedAt ? (
            <>
              <span aria-hidden>·</span>
              <span className="muted" title={new Date(row.pairedAt).toLocaleString()}>
                paired {timeAgo(row.pairedAt, now)}
              </span>
            </>
          ) : null}
          {row.hubVersion && (
            <>
              <span aria-hidden>·</span>
              <span className="muted">krypto {row.hubVersion}</span>
            </>
          )}
        </div>
        {error && (
          <span className="field-error" role="alert">
            {error}
          </span>
        )}
      </div>
      {!row.self && !confirming && (
        <button className="pill pill-ghost machine-row-action" disabled={!connected} onClick={() => setConfirming(true)}>
          Revoke
        </button>
      )}
      {!row.self && confirming && (
        <div className="machine-confirm" role="group" aria-label={`Unpair ${row.name}?`} onKeyDown={onConfirmKey}>
          <span className="machine-confirm-text">
            Unpair <strong>{row.name}</strong>? It stops connecting until you pair it again, and it is told it was removed.
          </span>
          <span className="machine-confirm-actions">
            <button className="pill" autoFocus disabled={busy} onClick={() => setConfirming(false)}>
              Keep
            </button>
            <button className="pill pill-bad" disabled={busy || !connected} onClick={() => void revoke()}>
              {busy ? "Revoking…" : "Revoke"}
            </button>
          </span>
        </div>
      )}
    </div>
  );
}

/** Create an invite on this hub: URI + copy, expiry countdown and the CLI line. */
function InviteCard() {
  const { client, state, meta, now } = usePanel();
  const invite = useShownInvite();
  const connected = meta.phase === "connected";
  const listening = state.peer?.listening ?? null;
  const wildcard = listening !== null && isUnspecifiedAddr(listening);
  const [addr, setAddr] = useState("");
  const [ttl, setTtl] = useState(INVITE_TTL_DEFAULT_SECS);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [note, setNote] = useState<string | null>(null);
  const prev = useRef<ShownInvite | null>(invite);

  // Single use: once a new machine pairs with it, the invite is spent.
  useEffect(() => {
    if (!invite) return;
    const used = inviteUsedBy(invite, state.devices);
    if (!used) return;
    inviteHolder.clear();
    setNote(`${used.name} paired with this machine using that invite.`);
  }, [invite, state.devices]);

  // The holder drops the invite at its expiry; say so instead of silently emptying the card.
  useEffect(() => {
    const before = prev.current;
    prev.current = invite;
    if (before && !invite && before.expiresAt <= Date.now() + 1000) {
      setNote((n) => n ?? "That invite expired. Create a new one if you still need it.");
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
      const res = await client.createInvite({ ttlSecs: ttl, addr: addr.trim() || undefined });
      inviteHolder.show({ uri: res.uri, expiresAt: res.expiresAt, createdAt, knownDeviceIds, ignoredDeviceIds: [] });
    } catch (err) {
      setError(pairingErrorText(err, "create_invite"));
    } finally {
      setBusy(false);
    }
  };

  if (invite) {
    const left = invite.expiresAt - now;
    return (
      <div className="card invite-card">
        <div className="invite-head">
          <h3>Invite ready</h3>
          <span className={`invite-timer${left < 60_000 ? " is-ending" : ""}`} title={`Expires at ${clockTime(invite.expiresAt)}`}>
            expires in {formatCountdown(invite.expiresAt, now)}
          </span>
        </div>
        <p className="hint">On the other machine open Machines → Join a machine and paste this, or run the command below there.</p>
        <CopyField label="Invite link" text={invite.uri} primary />
        <CopyField label="Command line" text={joinCommand(invite.uri)} />
        <p className="hint">
          Works once, until {clockTime(invite.expiresAt)}. Anyone who has it can pair while it is valid, so only paste it into your own machines.
        </p>
        <div className="form-actions">
          <button
            className="pill pill-ghost"
            title="Hides it here. The hub keeps it valid until it expires or a machine uses it."
            onClick={() => inviteHolder.clear()}
          >
            Hide
          </button>
          <button className="pill" disabled={!connected || busy} onClick={() => void create()}>
            New invite
          </button>
        </div>
      </div>
    );
  }

  return (
    <div className="card invite-card">
      <h3>Invite a machine</h3>
      {listening === null ? (
        <ListenerHelp />
      ) : (
        <>
          <p className="hint">
            Create a one-time link on this machine and paste it into the other one. This hub is listening on{" "}
            <span className="mono">{listening}</span>.
          </p>
          <div className="field">
            <label className="field-label" htmlFor="invite-addr">
              Address to share <span className="muted">{wildcard ? "required" : "optional"}</span>
            </label>
            <input
              id="invite-addr"
              className="input mono"
              value={addr}
              placeholder={wildcard ? `192.168.1.20:${PEER_DEFAULT_PORT}` : listening}
              spellCheck={false}
              autoComplete="off"
              disabled={!connected || busy}
              onChange={(e) => {
                setAddr(e.target.value);
                setError(null);
              }}
            />
            <span className="hint">
              {wildcard
                ? "The listener binds every network, so enter this machine's LAN IP and port for the other machine to dial."
                : `Leave empty to share ${listening}, or enter another host:port the other machine can reach.`}
            </span>
          </div>
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
        </>
      )}
      {error && (
        <span className="field-error" role="alert">
          {error}
        </span>
      )}
      {note && !error && <span className="hint invite-note">{note}</span>}
      <div className="form-actions">
        <button
          className="pill pill-primary"
          disabled={!connected || busy || listening === null || (wildcard && !addr.trim())}
          onClick={() => void create()}
        >
          {busy ? "Creating…" : "Create invite"}
        </button>
      </div>
    </div>
  );
}

/** The peer listener is off (the default): how to turn it on. */
function ListenerHelp() {
  const [dataDir, setDataDir] = useState<string | null>(null);
  useEffect(() => {
    if (!isTauri) return;
    native
      .hubCredentials()
      .then((c) => setDataDir(c.dataDir))
      .catch(() => {});
  }, []);
  return (
    <div className="listener-help">
      <strong>Turn on the peer listener first</strong>
      <span className="hint">
        Inviting needs this hub to accept connections from your network. That is off by default. Restart the hub with:
      </span>
      <CopyField label="Command" text="krypto serve --peer-listen <LAN-IP>" />
      <span className="hint">
        or add this to <span className="mono">config.toml</span>
        {dataDir ? (
          <>
            {" "}
            in <span className="mono">{dataDir}</span>
          </>
        ) : (
          " in the hub's data folder"
        )}{" "}
        and restart the hub:
      </span>
      <CopyField label="config.toml" text={`[peer]\nlisten = "192.168.1.20:${PEER_DEFAULT_PORT}"`} multiline />
      <span className="hint">
        Use this machine's LAN address (port {PEER_DEFAULT_PORT} by default) and allow it through the firewall. Only the machine that
        invites needs the listener; the joining machine dials out.
      </span>
    </div>
  );
}

/** Paste an invite from another machine and join it. */
function JoinCard() {
  const { client, state, meta, now, toast } = usePanel();
  const connected = meta.phase === "connected";
  const [text, setText] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const uri = extractInviteUri(text);
  const info = uri ? parseInviteUri(uri) : null;
  const typed = text.trim().length > 0;
  const liveError = !typed
    ? null
    : !info
      ? pairingErrorText({ code: "bad_uri" }, "join_peer")
      : info.machineId === state.machine?.id
        ? pairingErrorText({ code: "self_invite" }, "join_peer")
        : null;
  const shownError = error ?? liveError;
  const known = info ? state.machines[info.machineId] : undefined;

  const join = async () => {
    if (busy || !uri || !info || liveError) {
      if (!busy && typed && liveError) setError(liveError);
      return;
    }
    setBusy(true);
    setError(null);
    // Joining adds the inviter to our roster; that pairing must not look like our own invite being used.
    inviteHolder.ignoreDevice(info.machineId);
    try {
      const res = await client.joinPeer(uri);
      setText("");
      toast(`Paired with ${machineDisplayName(res.machine)}`);
    } catch (err) {
      setError(pairingErrorText(err, "join_peer"));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="card join-card">
      <h3>Join a machine</h3>
      <p className="hint">Paste the invite link (or the whole krypto peer join line) created on the other machine.</p>
      <div className="field">
        <label className="field-label" htmlFor="join-uri">
          Invite
        </label>
        <textarea
          id="join-uri"
          className={`input mono join-input${shownError ? " is-invalid" : ""}`}
          rows={3}
          value={text}
          placeholder="krypto://peer?v=1&mid=…"
          spellCheck={false}
          autoComplete="off"
          autoCorrect="off"
          autoCapitalize="off"
          disabled={!connected || busy}
          aria-invalid={shownError ? true : undefined}
          aria-describedby={shownError ? "join-error" : "join-preview"}
          onChange={(e) => {
            setText(e.target.value);
            setError(null);
          }}
          onKeyDown={(e) => {
            if (e.key === "Enter" && !e.shiftKey) {
              e.preventDefault();
              void join();
            }
          }}
        />
        {shownError ? (
          <span className="field-error" id="join-error" role="alert">
            {shownError}
          </span>
        ) : info ? (
          <span className="hint" id="join-preview">
            {known ? `${machineDisplayName(known)} (already paired; joining again refreshes the link)` : `Machine ${info.machineId}`} at{" "}
            <span className="mono">{info.addr}</span> ·{" "}
            {info.expiresAt > now
              ? `expires in ${formatCountdown(info.expiresAt, now)}`
              : "looks expired by this machine's clock, so it may be turned down"}
          </span>
        ) : (
          <span className="hint" id="join-preview">
            The other machine must have its peer listener on and be reachable on your network.
          </span>
        )}
      </div>
      <div className="form-actions">
        <button className="pill pill-primary" disabled={!connected || busy || !info || !!liveError} onClick={() => void join()}>
          {busy ? "Pairing…" : "Join"}
        </button>
        {busy && <span className="hint">Dialing {info?.addr}… this can take up to 10 seconds.</span>}
      </div>
    </div>
  );
}

/** Read-only value with a copy button. */
function CopyField({ label, text, primary, multiline }: { label: string; text: string; primary?: boolean; multiline?: boolean }) {
  const [copied, setCopied] = useState<boolean | null>(null);
  useEffect(() => {
    if (copied === null) return;
    const t = setTimeout(() => setCopied(null), 1800);
    return () => clearTimeout(t);
  }, [copied]);
  return (
    <div className="copy-field">
      <code
        className={`copy-value${multiline ? " is-multiline" : ""}`}
        aria-label={label}
        tabIndex={0}
        onFocus={(e) => window.getSelection()?.selectAllChildren(e.currentTarget)}
      >
        {text}
      </code>
      <button
        className={`pill${primary ? " pill-primary" : ""} copy-btn`}
        title={`Copy ${label.toLowerCase()}`}
        onClick={() => void copyText(text).then(setCopied)}
      >
        <I.Copy size={14} /> {copied === true ? "Copied" : copied === false ? "Select to copy" : "Copy"}
      </button>
    </div>
  );
}
