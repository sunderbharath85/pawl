// Settings (PLAN.md migration step 5b): every hub setting is changeable here; the CLI and config.toml are only for
// initial setup. Hub sections read and write the hub's config.toml through get_settings / update_settings (local
// only); Notifications, the pet on screen and This app are desktop preferences on this computer.
import { useEffect, type ReactNode } from "react";
import { machineDisplayName, restartText } from "@pawl/hub-client";
import { usePanel } from "../context";
import { A2aSection } from "./settings/A2aSection";
import { AgentsSection } from "./settings/AgentsSection";
import { AppSection, MachineSection, NotificationsSection, PetSection } from "./settings/AppSections";
import { PairingSection } from "./settings/PairingSection";
import { VoiceSection } from "./settings/VoiceSection";
import { BrowserSection } from "./settings/BrowserSection";
import { PetsSection } from "./settings/PetsSection";
import { BotSection, HubSection, NudgesSection, RelaySection, RemoteSection, ReposSection } from "./settings/sections";
import { SectionGuard, useHubSettings, useRestartHub, type HubSettingsView } from "./settings/shared";

export const SETTINGS_SECTIONS: ReadonlyArray<{ id: string; label: string; hub: boolean }> = [
  { id: "hub", label: "Hub", hub: true },
  { id: "machine", label: "This machine", hub: false },
  { id: "pairing", label: "Machines & pairing", hub: true },
  { id: "remote", label: "Remote control", hub: true },
  { id: "repos", label: "Repos", hub: true },
  { id: "agents", label: "Agents & hooks", hub: true },
  { id: "a2a", label: "A2A", hub: true },
  { id: "bot", label: "Bot", hub: true },
  { id: "browser", label: "Browser", hub: true },
  { id: "nudges", label: "Nudges & reports", hub: true },
  { id: "notifications", label: "Notifications", hub: false },
  { id: "relay", label: "Relay", hub: true },
  { id: "pet", label: "Pet on screen", hub: false },
  { id: "voice", label: "Voice", hub: false },
  { id: "pets", label: "Pets (advanced)", hub: false },
  { id: "app", label: "This app", hub: false },
];

/** A section in its own error boundary (a crash shows a card there, never a blank window). */
function guard(id: string, section: ReactNode): ReactNode {
  const title = SETTINGS_SECTIONS.find((s) => s.id === id)?.label ?? id;
  return (
    <SectionGuard key={id} id={id} title={title}>
      {section}
    </SectionGuard>
  );
}

function scrollToSection(id: string): void {
  document.getElementById(`settings-${id}`)?.scrollIntoView({ block: "start", behavior: "smooth" });
}

export function SettingsView({ section }: { section?: string | undefined }) {
  const { state } = usePanel();
  const hs = useHubSettings();
  const loaded = hs.settings !== undefined;

  // Jump to a requested section (open_panel, links from other views) once it is rendered.
  useEffect(() => {
    if (!section) return;
    const t = setTimeout(() => scrollToSection(section), 50);
    return () => clearTimeout(t);
  }, [section, loaded]);

  const props = hs.settings ? { settings: hs.settings, restartRequired: hs.restartRequired, live: hs.connected } : null;

  return (
    <section className="view settings-view">
      <header className="view-head">
        <h1>Settings</h1>
        <p>
          Everything Pawl does on {state.machine ? machineDisplayName(state.machine) : "this computer"}. Hub settings are saved to its
          config.toml and apply right away unless a section says otherwise.
        </p>
      </header>

      <RestartBanner keys={hs.restartRequired} />
      <HubNotice hs={hs} />

      <div className="settings-layout">
        <nav className="settings-nav" aria-label="Settings sections">
          {SETTINGS_SECTIONS.map((s) => (
            <button key={s.id} className={`settings-nav-item${section === s.id ? " is-active" : ""}`} disabled={s.hub && !props} onClick={() => scrollToSection(s.id)}>
              {s.label}
            </button>
          ))}
        </nav>
        <div className="settings-sections">
          {props && guard("hub", <HubSection {...props} configPath={hs.configPath} />)}
          {guard("machine", <MachineSection />)}
          {props && (
            <>
              {guard("pairing", <PairingSection {...props} />)}
              {guard("remote", <RemoteSection {...props} />)}
              {guard("repos", <ReposSection {...props} />)}
              {guard("agents", <AgentsSection {...props} />)}
              {guard("a2a", <A2aSection {...props} />)}
              {guard("bot", <BotSection {...props} />)}
              {guard("browser", <BrowserSection {...props} />)}
              {guard("nudges", <NudgesSection {...props} />)}
            </>
          )}
          {guard("notifications", <NotificationsSection />)}
          {props && guard("relay", <RelaySection {...props} />)}
          {guard("pet", <PetSection />)}
          {guard("voice", <VoiceSection />)}
          {guard("pets", <PetsSection />)}
          {guard("app", <AppSection configPath={hs.configPath} />)}
        </div>
      </div>
    </section>
  );
}

/** Settings saved to config.toml that the running hub only picks up after a restart. */
function RestartBanner({ keys }: { keys: string[] }) {
  const { restart, restarting, available } = useRestartHub();
  const text = restartText(keys);
  if (!text && !restarting) return null;
  return (
    <div className="card restart-banner" role="status">
      <div className="restart-banner-text">
        <strong>{restarting ? "Restarting the hub…" : text}</strong>
        <span>
          {restarting
            ? "It comes back by itself in a few seconds with the saved settings."
            : "The change is saved; the running hub keeps the old value until it restarts. Restarting ends the sessions Pawl is running, so pick a quiet moment."}
        </span>
      </div>
      {restarting ? (
        <span className="spinner" aria-hidden />
      ) : (
        <button className="pill pill-primary" disabled={!available} onClick={() => void restart(keys)}>
          Restart hub
        </button>
      )}
    </div>
  );
}

/** Why hub settings are missing: hub not running, too old, or the load failed. */
function HubNotice({ hs }: { hs: HubSettingsView }) {
  const { meta, conn } = usePanel();
  let body: ReactNode = null;
  if (!hs.connected) {
    body = (
      <>
        <strong>{hs.settings ? "The hub is not connected." : "Hub settings appear once the hub is running."}</strong>{" "}
        {hs.settings ? "Showing the last values it sent; changes are disabled until it is back." : "Start it to change its settings here."}
        {meta.phase === "hub-down" && (
          <button className="pill pill-primary notice-action" onClick={() => void conn.startHub()}>
            Start hub
          </button>
        )}
      </>
    );
  } else if (!hs.supported) {
    body = (
      <>
        <strong>This hub is too old to change settings from here.</strong> Update pawl on this machine and restart the hub.
      </>
    );
  } else if (hs.loadError) {
    body = (
      <>
        <strong>Could not load the hub's settings.</strong> {hs.loadError}{" "}
        <button className="link" onClick={() => void hs.reload()}>
          Try again
        </button>
      </>
    );
  } else if (!hs.settings) {
    body = <>Loading the hub's settings…</>;
  }
  if (!body) return null;
  return <div className="card notice-card settings-notice">{body}</div>;
}
