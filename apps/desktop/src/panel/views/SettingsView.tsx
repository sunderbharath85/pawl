import { useEffect, useRef, useState } from "react";
import { MACHINE_NAME_MAX, OS_LABEL, checkMachineName, machineDisplayName, renameErrorText } from "@krypto/hub-client";
import { errorText } from "../../lib/format";
import { isTauri, native, onNative, type HubCredentials } from "../../lib/native";
import { notify } from "../../lib/notifications";
import { useSettings, type DesktopSettings } from "../../lib/settings";
import { usePanel } from "../context";

export function SettingsView() {
  const { meta, conn, toast, state } = usePanel();
  const [settings, setSettings] = useSettings();
  const [paused, setPaused] = useState(false);
  const [creds, setCreds] = useState<HubCredentials | null>(null);
  const [hubBin, setHubBin] = useState<string | null | undefined>(undefined);

  useEffect(() => {
    if (!isTauri) return;
    native.getPetsPaused().then(setPaused).catch(() => {});
    native.hubCredentials().then(setCreds).catch(() => {});
    native.locateHub().then(setHubBin).catch(() => setHubBin(null));
    return onNative("pets-paused", setPaused);
  }, [meta.phase]);

  const toggle = (key: keyof Pick<DesktopSettings, "notifyPermissions" | "notifyTurns" | "notifyHub">) =>
    setSettings({ [key]: !settings[key] });

  return (
    <section className="view">
      <header className="view-head">
        <h1>Settings</h1>
        <p>Desktop preferences for {state.machine ? machineDisplayName(state.machine) : "this computer"}.</p>
      </header>

      <div className="settings-grid">
        <MachineCard />

        <div className="card">
          <h3>On the strip</h3>
          <Toggle
            label="Pause pets"
            hint="Hide the strip above the taskbar. Notifications keep coming."
            checked={paused}
            onChange={() => native.setPetsPaused(!paused).catch((err) => toast(errorText(err), "error"))}
            disabled={!isTauri}
          />
          <Toggle
            label="Show pet names"
            hint="A name tag under every pet on the strip."
            checked={settings.showPetNames}
            onChange={() => setSettings({ showPetNames: !settings.showPetNames })}
          />
          <label className="field">
            <span className="field-label">
              Pet size <span className="muted">{settings.petSize}px</span>
            </span>
            <input
              type="range"
              min={72}
              max={168}
              step={8}
              value={settings.petSize}
              onChange={(e) => setSettings({ petSize: Number(e.target.value) })}
            />
          </label>
        </div>

        <div className="card">
          <h3>Notifications</h3>
          <Toggle label="Permission requests" hint="When a pet barks for approval." checked={settings.notifyPermissions} onChange={() => toggle("notifyPermissions")} />
          <Toggle label="Turn finished" hint="When a pet-run session ends its turn or errors." checked={settings.notifyTurns} onChange={() => toggle("notifyTurns")} />
          <Toggle label="Hub alerts" hint="Watchdog nudges, hook events and reports." checked={settings.notifyHub} onChange={() => toggle("notifyHub")} />
          <button className="pill" onClick={() => void notify("Woof!", "This is how your pet will get your attention.")}>
            Send a test
          </button>
        </div>

        <div className="card">
          <h3>Hub</h3>
          <dl className="kv">
            <dt>Status</dt>
            <dd>
              <span className={`status-pill tone-${meta.phase === "connected" ? "done" : meta.phase === "hub-down" ? "error" : "waiting"}`}>
                {meta.phase.replace("-", " ")}
              </span>
            </dd>
            <dt>Address</dt>
            <dd className="mono">{meta.url}</dd>
            <dt>Version</dt>
            <dd>{meta.hubVersion ?? "unknown"}</dd>
            <dt>Token</dt>
            <dd>{creds ? (creds.tokenSource === "missing" ? "not created yet" : `from ${creds.tokenSource === "env" ? "KRYPTO_HUB_TOKEN" : "data folder"}`) : "…"}</dd>
            <dt>Data folder</dt>
            <dd className="mono">{creds?.dataDir ?? "…"}</dd>
            <dt>krypto</dt>
            <dd className="mono">{hubBin === undefined ? "…" : (hubBin ?? "not found (PATH, next to the app, or KRYPTO_HUB_BIN)")}</dd>
          </dl>
          <div className="form-actions">
            <button className="pill" onClick={() => void conn.refresh(true)}>
              Reconnect
            </button>
            {meta.phase === "hub-down" && (
              <button className="pill pill-primary" onClick={() => void conn.startHub()}>
                Start hub
              </button>
            )}
            <span className="spacer" />
            {isTauri && (
              <button className="pill pill-bad" onClick={() => void native.quit()}>
                Quit Krypto
              </button>
            )}
          </div>
        </div>
      </div>
    </section>
  );
}

/** This hub's machine: its name (renamable), OS and id. */
function MachineCard() {
  const { client, state, meta, toast } = usePanel();
  const machine = state.machine;
  const current = machine?.name ?? "";
  const [draft, setDraft] = useState(current);
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  /** The server name the draft was based on: follow remote renames unless the user has typed. */
  const base = useRef(current);

  useEffect(() => {
    const prev = base.current;
    base.current = current;
    setDraft((d) => (d === prev ? current : d));
  }, [current]);

  const connected = meta.phase === "connected" && !!machine;
  const check = checkMachineName(draft);
  const dirty = check.ok ? check.name !== current : draft.trim() !== current;
  // Show the length problem as soon as it happens; an empty field only complains on save.
  const liveError = !check.ok && draft.trim().length > 0 ? check.error : null;
  const shownError = error ?? liveError;

  const save = async () => {
    if (saving) return;
    const res = checkMachineName(draft);
    if (!res.ok) {
      setError(res.error);
      return;
    }
    if (res.name === current) {
      setDraft(res.name);
      setError(null);
      return;
    }
    setSaving(true);
    setError(null);
    try {
      const updated = await client.updateMachine(res.name);
      base.current = updated.name;
      setDraft(updated.name);
      toast(`This machine is now “${updated.name}”`);
    } catch (err) {
      setError(renameErrorText(err));
    } finally {
      setSaving(false);
    }
  };

  const reset = () => {
    setDraft(current);
    setError(null);
  };

  return (
    <div className="card">
      <h3>This machine</h3>
      <form
        className="machine-form"
        onSubmit={(e) => {
          e.preventDefault();
          void save();
        }}
      >
        <div className="field">
          <label className="field-label" htmlFor="machine-name">
            Name <span className="muted">shown on every screen paired with this hub</span>
          </label>
          <span className="machine-name-row">
            <input
              id="machine-name"
              className={`input${shownError ? " is-invalid" : ""}`}
              value={draft}
              placeholder={machine ? "e.g. Windows PC" : "Connect the hub to name this machine"}
              disabled={!connected || saving}
              aria-invalid={shownError ? true : undefined}
              aria-describedby={shownError ? "machine-name-error" : undefined}
              onChange={(e) => {
                setDraft(e.target.value);
                setError(null);
              }}
              onKeyDown={(e) => {
                if (e.key === "Escape") reset();
              }}
            />
            <button className="pill pill-primary" type="submit" disabled={!connected || saving || !dirty}>
              {saving ? "Saving…" : "Rename"}
            </button>
          </span>
          {shownError ? (
            <span className="field-error" id="machine-name-error" role="alert">
              {shownError}
            </span>
          ) : (
            <span className="hint">
              1–{MACHINE_NAME_MAX} characters. {connected ? "" : "The hub must be running to rename."}
            </span>
          )}
        </div>
      </form>
      <dl className="kv">
        <dt>System</dt>
        <dd>{machine ? (OS_LABEL[machine.os] ?? machine.os) : "…"}</dd>
        <dt>Machine id</dt>
        <dd className="mono">{machine?.id ?? "…"}</dd>
      </dl>
    </div>
  );
}

function Toggle(props: { label: string; hint?: string; checked: boolean; onChange(): void; disabled?: boolean }) {
  return (
    <label className={`toggle${props.disabled ? " is-disabled" : ""}`}>
      <span className="toggle-text">
        <strong>{props.label}</strong>
        {props.hint && <span>{props.hint}</span>}
      </span>
      <input type="checkbox" checked={props.checked} onChange={props.onChange} disabled={props.disabled} />
      <span className="toggle-track" aria-hidden>
        <span className="toggle-thumb" />
      </span>
    </label>
  );
}
