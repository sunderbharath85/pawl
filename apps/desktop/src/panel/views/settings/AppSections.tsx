// Settings sections that are not in config.toml: This machine (its name, via update_machine), Notifications and
// the pet strip (desktop preferences kept per machine), and This app (hub process, setup, quit).
import { useEffect, useRef, useState } from "react";
import { MACHINE_NAME_MAX, OS_LABEL, checkMachineName, renameErrorText, trimWhiteSpace } from "@pawl/hub-client";
import { isTauri, native, onNative, type HubCredentials } from "../../../lib/native";
import { notify } from "../../../lib/notifications";
import { useSettings, type DesktopSettings } from "../../../lib/settings";
import { usePanel } from "../../context";
import { SettingsSection, Toggle } from "./shared";

/** Rename this hub's machine. Also the first-run setup's "name this machine" step. */
export function MachineNameForm({ onSaved, autoFocus }: { onSaved?: (name: string) => void; autoFocus?: boolean }) {
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
  // Show a length or character problem (the peer name rule) as soon as it happens; an empty field only complains on save.
  const liveError = !check.ok && trimWhiteSpace(draft).length > 0 ? check.error : null;
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
      onSaved?.(res.name);
      return;
    }
    setSaving(true);
    setError(null);
    try {
      const updated = await client.updateMachine(res.name);
      base.current = updated.name;
      setDraft(updated.name);
      toast(`This machine is now “${updated.name}”`);
      onSaved?.(updated.name);
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
            autoFocus={autoFocus}
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
          <button className="pill pill-primary" type="submit" disabled={!connected || saving || (!dirty && !onSaved)}>
            {saving ? "Saving…" : dirty || !onSaved ? "Rename" : "Keep name"}
          </button>
        </span>
        {shownError ? (
          <span className="field-error" id="machine-name-error" role="alert">
            {shownError}
          </span>
        ) : (
          <span className="hint">
            1–{MACHINE_NAME_MAX} characters, no invisible characters. {connected ? "" : "The hub must be running to rename."}
          </span>
        )}
      </div>
    </form>
  );
}

export function MachineSection() {
  const { state } = usePanel();
  const machine = state.machine;
  return (
    <SettingsSection id="machine" title="This machine" description="How this computer appears on your other machines and on the pets' tags.">
      <MachineNameForm />
      <dl className="kv">
        <dt>System</dt>
        <dd>{machine ? (OS_LABEL[machine.os] ?? machine.os) : "…"}</dd>
        <dt>Machine id</dt>
        <dd className="mono">{machine?.id ?? "…"}</dd>
      </dl>
    </SettingsSection>
  );
}

export function NotificationsSection() {
  const [settings, setSettings] = useSettings();
  const toggle = (key: keyof Pick<DesktopSettings, "notifyPermissions" | "notifyTurns" | "notifyHub">) => setSettings({ [key]: !settings[key] });
  return (
    <SettingsSection id="notifications" title="Notifications" description="Desktop notifications from this app (kept on this computer).">
      <Toggle label="Permission requests" hint="When a pet barks for approval." checked={settings.notifyPermissions} onChange={() => toggle("notifyPermissions")} />
      <Toggle label="Turn finished" hint="When a pet-run session ends its turn or errors." checked={settings.notifyTurns} onChange={() => toggle("notifyTurns")} />
      <Toggle label="Hub alerts" hint="Watchdog nudges, hook events and reports." checked={settings.notifyHub} onChange={() => toggle("notifyHub")} />
      <button className="pill" onClick={() => void notify("Woof!", "This is how your pet will get your attention.")}>
        Send a test
      </button>
    </SettingsSection>
  );
}

export function StripSection() {
  const { meta, toast } = usePanel();
  const [settings, setSettings] = useSettings();
  const [paused, setPaused] = useState(false);
  useEffect(() => {
    if (!isTauri) return;
    native.getPetsPaused().then(setPaused).catch(() => {});
    return onNative("pets-paused", setPaused);
  }, [meta.phase]);
  return (
    <SettingsSection id="strip" title="On the strip" description="The pets walking above your taskbar.">
      <Toggle
        label="Pause pets"
        hint="Hide the strip above the taskbar. Notifications keep coming."
        checked={paused}
        onChange={() => native.setPetsPaused(!paused).catch((err) => toast(String(err), "error"))}
        disabled={!isTauri}
      />
      <Toggle label="Show pet names" hint="A name tag under every pet on the strip." checked={settings.showPetNames} onChange={() => setSettings({ showPetNames: !settings.showPetNames })} />
      <label className="field">
        <span className="field-label">
          Pet size <span className="muted">{settings.petSize}px</span>
        </span>
        <input type="range" min={72} max={168} step={8} value={settings.petSize} onChange={(e) => setSettings({ petSize: Number(e.target.value) })} />
      </label>
    </SettingsSection>
  );
}

/** The hub process this app talks to, the first-run setup, and quitting. */
export function AppSection({ configPath }: { configPath?: string | undefined }) {
  const { meta, conn, client, navigate } = usePanel();
  const [creds, setCreds] = useState<HubCredentials | null>(null);
  const [hubBin, setHubBin] = useState<string | null | undefined>(undefined);
  useEffect(() => {
    if (!isTauri) return;
    native.hubCredentials().then(setCreds).catch(() => {});
    native.locateHub().then(setHubBin).catch(() => setHubBin(null));
  }, [meta.phase]);

  return (
    <SettingsSection id="app" title="This app" description="The desktop app's connection to the hub.">
      <dl className="kv">
        <dt>Status</dt>
        <dd>
          <span className={`status-pill tone-${meta.phase === "connected" ? "done" : meta.phase === "hub-down" ? "error" : "waiting"}`}>{meta.phase.replace("-", " ")}</span>
        </dd>
        <dt>Address</dt>
        <dd className="mono">{meta.phase === "connected" ? client.wsUrl.replace(/^ws/, "http").replace(/\/ws$/, "") : meta.url}</dd>
        <dt>Version</dt>
        <dd>{meta.hubVersion ?? "unknown"}</dd>
        <dt>Token</dt>
        <dd>{creds ? (creds.tokenSource === "missing" ? "not created yet" : `from ${creds.tokenSource === "env" ? "PAWL_HUB_TOKEN" : "data folder"}`) : "…"}</dd>
        <dt>Data folder</dt>
        <dd className="mono">{creds?.dataDir ?? "…"}</dd>
        {configPath && (
          <>
            <dt>Config</dt>
            <dd className="mono">{configPath}</dd>
          </>
        )}
        <dt>pawl</dt>
        <dd className="mono">{hubBin === undefined ? "…" : (hubBin ?? "not found (PATH, next to the app, or PAWL_HUB_BIN)")}</dd>
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
        <button className="pill" onClick={() => navigate({ view: "setup" })}>
          Run setup again
        </button>
        <span className="spacer" />
        {isTauri && (
          <button className="pill pill-bad" onClick={() => void native.quit()}>
            Quit Pawl
          </button>
        )}
      </div>
    </SettingsSection>
  );
}
