import { useEffect, useState } from "react";
import { errorText } from "../../lib/format";
import { isTauri, native, onNative, type HubCredentials } from "../../lib/native";
import { notify } from "../../lib/notifications";
import { useSettings, type DesktopSettings } from "../../lib/settings";
import { usePanel } from "../context";

export function SettingsView() {
  const { meta, conn, toast } = usePanel();
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
        <p>Desktop preferences for this computer.</p>
      </header>

      <div className="settings-grid">
        <div className="card">
          <h3>On the strip</h3>
          <Toggle
            label="Pause pets"
            hint="Hide the strip above the taskbar. Notifications keep coming."
            checked={paused}
            onChange={() => native.setPetsPaused(!paused).catch((err) => toast(errorText(err), "error"))}
            disabled={!isTauri}
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
