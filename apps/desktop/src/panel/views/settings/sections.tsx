// Settings sections backed by the hub's config.toml: Hub, Remote control, Repos, Bot, Nudges & reports, Relay.
import { useEffect, useMemo, useState } from "react";
import { relayStatusText, secretPatch, type SecretKey } from "@krypto/hub-client";
import type { HubSettings, RepoRef } from "@krypto/protocol";
import { timeAgo } from "../../../lib/format";
import {
  botForm,
  botPatch,
  hubForm,
  HUB_LIMITS,
  hubPatch,
  nudgesForm,
  nudgesPatch,
  NUDGE_MAX_MINUTES,
  relayPatch,
  remoteForm,
  remotePatch,
  reposPatch,
  TICK_LIMITS,
} from "../../../lib/settingsForms";
import { usePanel } from "../../context";
import {
  errorFor,
  Field,
  FolderList,
  OverrideNote,
  RestartChip,
  SaveBar,
  SecretField,
  SettingsSection,
  Toggle,
  useDraft,
  useSaveSettings,
  useRestartHub,
  useSectionSave,
} from "./shared";

export interface SectionProps {
  settings: HubSettings;
  restartRequired: string[];
  /** The hub is connected (inputs are disabled otherwise). */
  live: boolean;
}

/** Save a write-only secret and report the outcome to a SecretField. */
export function useSecretSaver() {
  const save = useSaveSettings();
  return async (key: SecretKey, value: string | null) => {
    const out = await save(secretPatch(key, value), { success: value === null ? "Key cleared" : "Key saved" });
    if (out.ok) return { ok: true };
    if (out.cancelled) return { ok: false, error: null };
    return { ok: false, error: out.fieldError ? out.fieldError.reason : out.message };
  };
}

// ---------------------------------------------------------------- Hub

export function HubSection({ settings, restartRequired, live, configPath }: SectionProps & { configPath?: string | undefined }) {
  const source = useMemo(() => hubForm(settings), [settings]);
  const { draft, update, dirty, reset } = useDraft(source);
  const save = useSectionSave(["hub.port", "hub.offline", "hub.hookSessionStaleHours", "hub.ownedSessionIdleHours", "hub.eventRetentionDays"]);
  const err = (k: string) => errorFor(save.errors, k);
  const portPinned = !!settings.overriddenBy["hub.port"];
  const restarter = useRestartHub();

  return (
    <SettingsSection
      id="hub"
      title="Hub"
      aside={
        <button
          className="pill"
          disabled={!restarter.available || restarter.restarting}
          title="Running sessions end; paired machines reconnect"
          onClick={() => void restarter.restart(restartRequired)}
        >
          {restarter.restarting ? "Restarting…" : "Restart hub"}
        </button>
      }
      description={
        <>
          The krypto hub on this machine.{" "}
          {configPath && (
            <>
              Saved to <span className="mono">{configPath}</span>.
            </>
          )}
        </>
      }
    >
      <div className="settings-fields">
        <Field
          label={
            <>
              Port <RestartChip restartRequired={restartRequired} field="hub.port" />
            </>
          }
          error={err("hub.port")}
          hint={
            portPinned ? (
              <OverrideNote settings={settings} field="hub.port" />
            ) : (
              "Local port for the apps on this machine (127.0.0.1 only). Takes effect after a hub restart; the desktop app and VS Code follow it."
            )
          }
        >
          {(a) => (
            <input
              id={a.id}
              className={`input mono num-input${a.invalid ? " is-invalid" : ""}`}
              inputMode="numeric"
              value={draft.port}
              disabled={!live || save.saving}
              aria-invalid={a.invalid || undefined}
              aria-describedby={a.describedBy}
              onChange={(e) => update({ port: e.target.value })}
            />
          )}
        </Field>
        <NumberField
          label="Event history"
          unit="days"
          value={draft.eventRetentionDays}
          error={err("hub.eventRetentionDays")}
          hint={`Older session events are pruned when the hub starts. 0 keeps everything. ${HUB_LIMITS.eventRetentionDays[1]} days at most.`}
          disabled={!live || save.saving}
          onChange={(v) => update({ eventRetentionDays: v })}
        />
        <NumberField
          label="End idle Krypto sessions after"
          unit="hours"
          value={draft.ownedSessionIdleHours}
          error={err("hub.ownedSessionIdleHours")}
          hint="Sessions Krypto started are closed after this long without activity, which stops their agent process. 0 = never."
          disabled={!live || save.saving}
          onChange={(v) => update({ ownedSessionIdleHours: v })}
        />
        <NumberField
          label="Forget quiet hook sessions after"
          unit="hours"
          value={draft.hookSessionStaleHours}
          error={err("hub.hookSessionStaleHours")}
          hint="Sessions seen through agent hooks are marked finished when no hook arrives for this long."
          disabled={!live || save.saving}
          onChange={(v) => update({ hookSessionStaleHours: v })}
        />
      </div>
      <Toggle
        label="Offline mode"
        hint="Never download the ACP agent registry; use the cached list and the built-in agents only."
        checked={draft.offline}
        disabled={!live || save.saving || !!settings.overriddenBy["hub.offline"]}
        onChange={() => update({ offline: !draft.offline })}
      />
      <OverrideNote settings={settings} field="hub.offline" />
      <SaveBar dirty={dirty} saving={save.saving} message={save.message} disabled={!live} onReset={() => (reset(), save.clear())} onSave={() => void save.run(hubPatch(draft, settings))} />
    </SettingsSection>
  );
}

function NumberField(props: { label: string; unit: string; value: string; error: string | null; hint: string; disabled: boolean; onChange(v: string): void; decimal?: boolean }) {
  return (
    <Field label={props.label} error={props.error} hint={props.hint}>
      {(a) => (
        <span className="unit-input">
          <input
            id={a.id}
            className={`input num-input${a.invalid ? " is-invalid" : ""}`}
            inputMode={props.decimal ? "decimal" : "numeric"}
            value={props.value}
            disabled={props.disabled}
            aria-invalid={a.invalid || undefined}
            aria-describedby={a.describedBy}
            onChange={(e) => props.onChange(e.target.value)}
          />
          <span className="unit">{props.unit}</span>
        </span>
      )}
    </Field>
  );
}

// ---------------------------------------------------------------- Remote control

export function RemoteSection({ settings, live }: SectionProps) {
  const source = useMemo(() => remoteForm(settings), [settings]);
  const { draft, update, dirty, reset } = useDraft(source);
  const save = useSectionSave(["peer.acceptIntents", "peer.startRoots"]);

  return (
    <SettingsSection
      id="remote"
      title="Remote control"
      description="What machines paired with this one may do here. Watching (sessions, pets, approvals) always works between paired machines."
    >
      <Toggle
        label="Let paired machines act on this one"
        hint={
          draft.acceptIntents
            ? "On: they can start agents in allowed folders, prompt and cancel sessions, and approve once or deny requests (starting and approving ask on their side first)."
            : "Off: paired machines can watch, but every action they try here is refused."
        }
        checked={draft.acceptIntents}
        disabled={!live || save.saving || !!settings.overriddenBy["peer.acceptIntents"]}
        onChange={() => update({ acceptIntents: !draft.acceptIntents })}
      />
      <OverrideNote settings={settings} field="peer.acceptIntents" />
      <FolderList
        field="peer.startRoots"
        label="Start folders"
        folders={draft.startRoots}
        errors={save.errors}
        disabled={!live || save.saving}
        empty="None. Paired machines can only start agents in folders this machine's own sessions already used."
        addLabel="Folder to allow"
        pickTitle="Folder paired machines may start agents in"
        onChange={(startRoots) => update({ startRoots })}
      />
      <p className="hint">Besides the folders this machine's sessions already used, paired machines may start agents in these folders and everything inside them.</p>
      <SaveBar dirty={dirty} saving={save.saving} message={save.message} disabled={!live} onReset={() => (reset(), save.clear())} onSave={() => void save.run(remotePatch(draft, settings))} />
    </SettingsSection>
  );
}

// ---------------------------------------------------------------- Repos

export function ReposSection({ settings, live }: SectionProps) {
  const { client, now } = usePanel();
  const source = useMemo(() => ({ paths: [...settings.repos.paths] }), [settings]);
  const { draft, update, dirty, reset } = useDraft(source);
  const save = useSectionSave(["repos.paths"]);
  const [repos, setRepos] = useState<RepoRef[] | null>(null);
  const [repoError, setRepoError] = useState<string | null>(null);

  // The live index: refreshed when the hub is (re)connected and after the saved list changes.
  useEffect(() => {
    if (!live) return;
    let gone = false;
    client
      .listRepos()
      .then((r) => {
        if (gone) return;
        setRepos(r);
        setRepoError(null);
      })
      .catch(() => {
        if (!gone) setRepoError("Could not load the repo index.");
      });
    return () => {
      gone = true;
    };
  }, [client, live, settings.repos.paths]);

  return (
    <SettingsSection
      id="repos"
      title="Repos"
      description="Folders the bot and paired machines know by name (“run the tests in api”), next to the folders sessions already used."
    >
      <FolderList
        field="repos.paths"
        label="Extra repo folders"
        folders={draft.paths}
        errors={save.errors}
        disabled={!live || save.saving}
        empty="None yet. Folders appear in the index below once a session runs in them."
        addLabel="Repo folder to add"
        pickTitle="Repo folder"
        onChange={(paths) => update({ paths })}
      />
      <SaveBar dirty={dirty} saving={save.saving} message={save.message} disabled={!live} onReset={() => (reset(), save.clear())} onSave={() => void save.run(reposPatch(draft.paths, settings))} />
      <div className="field settings-field is-wide">
        <span className="field-label">
          Repo index on this machine <span className="muted">{repos ? `${repos.length}` : ""}</span>
        </span>
        {repoError ? (
          <span className="field-error">{repoError}</span>
        ) : !repos ? (
          <span className="hint">{live ? "Loading…" : "Connect the hub to see the index."}</span>
        ) : repos.length === 0 ? (
          <span className="hint">Empty. Start a session in a folder or add one above.</span>
        ) : (
          <ul className="repo-index">
            {repos.map((r) => (
              <li key={r.path}>
                <strong>{r.name}</strong>
                <span className="mono muted" title={r.path}>
                  {r.path}
                </span>
                {!!r.lastUsedAt && <span className="muted">used {timeAgo(r.lastUsedAt, now)}</span>}
              </li>
            ))}
          </ul>
        )}
      </div>
    </SettingsSection>
  );
}

// ---------------------------------------------------------------- Bot

export function BotSection({ settings, live }: SectionProps) {
  const source = useMemo(() => botForm(settings), [settings]);
  const { draft, update, dirty, reset } = useDraft(source);
  const save = useSectionSave(["bot.model", "bot.ollamaUrl", "bot.ollamaModel"]);
  const saveSecret = useSecretSaver();
  const err = (k: string) => errorFor(save.errors, k);

  return (
    <SettingsSection id="bot" title="Bot" description="The pets' chat brain: Anthropic when a key is set, else Ollama when both its URL and model are set, else simple built-in replies.">
      <div className="settings-fields">
        <Field label="Model" error={err("bot.model")} hint="Anthropic model id, e.g. claude-haiku-5-5.">
          {(a) => (
            <input
              id={a.id}
              className={`input mono${a.invalid ? " is-invalid" : ""}`}
              value={draft.model}
              spellCheck={false}
              disabled={!live || save.saving}
              aria-invalid={a.invalid || undefined}
              aria-describedby={a.describedBy}
              onChange={(e) => update({ model: e.target.value })}
            />
          )}
        </Field>
        <SecretField
          label="Anthropic API key"
          isSet={settings.bot.anthropicApiKeySet}
          disabled={!live}
          placeholder="sk-ant-…"
          hint="Stored by the hub on this machine. It is never shown again, here or anywhere."
          clearEffect="The bot stops using Anthropic until a key is set again."
          overriddenBy={settings.overriddenBy["bot.anthropicApiKey"]}
          onSave={(v) => saveSecret("bot.anthropicApiKey", v)}
        />
        <Field label="Ollama URL" error={err("bot.ollamaUrl")} hint="Optional local model server, e.g. http://127.0.0.1:11434. Used with the model below; empty = off.">
          {(a) => (
            <input
              id={a.id}
              className={`input mono${a.invalid ? " is-invalid" : ""}`}
              value={draft.ollamaUrl}
              placeholder="http://127.0.0.1:11434"
              spellCheck={false}
              disabled={!live || save.saving}
              aria-invalid={a.invalid || undefined}
              aria-describedby={a.describedBy}
              onChange={(e) => update({ ollamaUrl: e.target.value })}
            />
          )}
        </Field>
        <Field label="Ollama model" error={err("bot.ollamaModel")} hint="e.g. llama3.2. Ollama is used only when both the URL and the model are set.">
          {(a) => (
            <input
              id={a.id}
              className={`input mono${a.invalid ? " is-invalid" : ""}`}
              value={draft.ollamaModel}
              placeholder="llama3.2"
              spellCheck={false}
              disabled={!live || save.saving}
              aria-invalid={a.invalid || undefined}
              aria-describedby={a.describedBy}
              onChange={(e) => update({ ollamaModel: e.target.value })}
            />
          )}
        </Field>
      </div>
      <SaveBar dirty={dirty} saving={save.saving} message={save.message} disabled={!live} onReset={() => (reset(), save.clear())} onSave={() => void save.run(botPatch(draft, settings))} />
    </SettingsSection>
  );
}

// ---------------------------------------------------------------- Nudges & reports

export function NudgesSection({ settings, live }: SectionProps) {
  const source = useMemo(() => nudgesForm(settings), [settings]);
  const { draft, update, dirty, reset } = useDraft(source);
  const save = useSectionSave(["hub.dailyReportTime", "watchdog.waitingNudgeAfterMs", "watchdog.repeatEveryMs", "watchdog.tickSecs"]);
  const err = (k: string) => errorFor(save.errors, k);
  const off = !live || save.saving;

  return (
    <SettingsSection id="nudges" title="Nudges & reports" description="When pets remind you about agents waiting for you, and the end-of-day report.">
      <div className="settings-fields">
        <NumberField
          label="First nudge after"
          unit="minutes"
          decimal
          value={draft.nudgeMinutes}
          error={err("watchdog.waitingNudgeAfterMs")}
          hint="How long an agent waits for you before its pet speaks up."
          disabled={off}
          onChange={(v) => update({ nudgeMinutes: v })}
        />
        <NumberField
          label="Then repeat every"
          unit="minutes"
          decimal
          value={draft.repeatMinutes}
          error={err("watchdog.repeatEveryMs")}
          hint={`While it is still waiting. At most ${NUDGE_MAX_MINUTES} minutes.`}
          disabled={off}
          onChange={(v) => update({ repeatMinutes: v })}
        />
        <NumberField
          label="Check every"
          unit="seconds"
          value={draft.tickSecs}
          error={err("watchdog.tickSecs")}
          hint={`How often the watchdog looks (${TICK_LIMITS[0]}–${TICK_LIMITS[1]} s).`}
          disabled={off}
          onChange={(v) => update({ tickSecs: v })}
        />
      </div>
      <Toggle label="Daily report" hint="A notification summing up the day's sessions." checked={draft.reportOn} disabled={off} onChange={() => update({ reportOn: !draft.reportOn })} />
      {draft.reportOn && (
        <Field label="Report time" error={err("hub.dailyReportTime")} hint="Local time on this machine.">
          {(a) => (
            <input
              id={a.id}
              type="time"
              className={`input time-input${a.invalid ? " is-invalid" : ""}`}
              value={draft.reportTime}
              disabled={off}
              aria-invalid={a.invalid || undefined}
              aria-describedby={a.describedBy}
              onChange={(e) => update({ reportTime: e.target.value })}
            />
          )}
        </Field>
      )}
      <SaveBar dirty={dirty} saving={save.saving} message={save.message} disabled={!live} onReset={() => (reset(), save.clear())} onSave={() => void save.run(nudgesPatch(draft, settings))} />
    </SettingsSection>
  );
}

// ---------------------------------------------------------------- Relay

export function RelaySection({ settings, live }: SectionProps) {
  const { state } = usePanel();
  const source = useMemo(() => ({ url: settings.relay.url ?? "" }), [settings]);
  const { draft, update, dirty, reset } = useDraft(source);
  const save = useSectionSave(["relay.url"]);
  const saveSecret = useSecretSaver();
  const status = relayStatusText(state.peer?.relay);
  const savedOff = settings.relay.url === null;

  return (
    <SettingsSection
      id="relay"
      title="Relay"
      description="Reach your paired machines when they are not on the same network. The relay only passes on encrypted messages; it cannot read them."
      aside={
        <span className={`status-pill relay-pill tone-${status.tone}`} role="status">
          <span className="dot" aria-hidden />
          {status.tone === "connected" ? "Connected" : status.tone === "error" ? "Not connected" : status.tone === "connecting" ? "Connecting" : "Off"}
        </span>
      }
    >
      <p className={`relay-status tone-${status.tone}`}>{savedOff && status.tone === "off" ? "The relay is off. Paired machines connect only on your local network." : status.text}</p>
      <div className="settings-fields">
        <Field label="Relay URL" error={errorFor(save.errors, "relay.url")} hint="Your relay deployment, e.g. wss://relay.example.dev. Empty = relay off.">
          {(a) => (
            <input
              id={a.id}
              className={`input mono${a.invalid ? " is-invalid" : ""}`}
              value={draft.url}
              placeholder="wss://relay.example.dev"
              spellCheck={false}
              disabled={!live || save.saving}
              aria-invalid={a.invalid || undefined}
              aria-describedby={a.describedBy}
              onChange={(e) => update({ url: e.target.value })}
            />
          )}
        </Field>
        <SecretField
          label="Relay access key"
          isSet={settings.relay.accessKeySet}
          disabled={!live}
          hint="The access key from your relay deployment; paired devices get it through invites."
          clearEffect="This hub can no longer connect to a relay that needs a key."
          overriddenBy={settings.overriddenBy["relay.accessKey"]}
          onSave={(v) => saveSecret("relay.accessKey", v)}
        />
      </div>
      <SaveBar dirty={dirty} saving={save.saving} message={save.message} disabled={!live} onReset={() => (reset(), save.clear())} onSave={() => void save.run(relayPatch(draft.url, settings))} />
    </SettingsSection>
  );
}
