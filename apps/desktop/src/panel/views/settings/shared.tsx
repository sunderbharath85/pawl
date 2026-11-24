// Building blocks of the Settings view: loading the hub's settings, saving a patch (risky changes confirm first,
// field errors come back keyed by field), per-section drafts, and the field components.
import { useCallback, useEffect, useId, useMemo, useRef, useState, type ReactNode } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import {
  broadFolderReason,
  fieldMatches,
  isEmptyPatch,
  machineDisplayName,
  parseSettingsError,
  patchKeys,
  restartText,
  riskyChanges,
  SETTINGS_PROTOCOL_VERSION,
  settingsErrorText,
  settingsFieldLabel,
  type HubClient,
  type SettingsFieldError,
} from "@krypto/hub-client";
import type { HubSettings, HubSettingsPatch } from "@krypto/protocol";
import { homeFolder, isTauri } from "../../../lib/native";
import type { FormErrors } from "../../../lib/settingsForms";
import { useConfirm } from "../../Confirm";
import { usePanel } from "../../context";

// ---------------------------------------------------------------- loading

/** One in-flight `get_settings` per client (several sections mount at once). */
const inflight = new WeakMap<HubClient, Promise<unknown>>();

export interface HubSettingsView {
  settings: HubSettings | undefined;
  restartRequired: string[];
  configPath: string | undefined;
  connected: boolean;
  /** False for a hub older than protocol v7 (it has no settings API). */
  supported: boolean;
  loading: boolean;
  loadError: string | null;
  reload(): Promise<void>;
}

/** The hub's settings (fetched on every (re)connect; kept fresh by `settings` broadcasts). */
export function useHubSettings(): HubSettingsView {
  const { client, state, meta } = usePanel();
  const connected = meta.phase === "connected";
  const supported = state.protocolVersion === undefined || state.protocolVersion >= SETTINGS_PROTOCOL_VERSION;
  const [loading, setLoading] = useState(false);
  const [loadError, setLoadError] = useState<string | null>(null);

  const reload = useCallback(async () => {
    let p = inflight.get(client);
    if (!p) {
      p = client.getSettings().finally(() => inflight.delete(client));
      inflight.set(client, p);
    }
    setLoading(true);
    try {
      await p;
      setLoadError(null);
    } catch (err) {
      setLoadError(settingsErrorText(err));
    } finally {
      setLoading(false);
    }
  }, [client]);

  useEffect(() => {
    if (connected && supported) void reload();
  }, [connected, supported, reload]);

  return {
    settings: state.settings,
    restartRequired: state.restartRequired ?? [],
    configPath: state.settingsConfigPath,
    connected,
    supported,
    loading,
    loadError,
    reload,
  };
}

// ---------------------------------------------------------------- saving

export type SaveOutcome =
  | { ok: true; restart: string[] }
  | { ok: false; cancelled: true }
  | { ok: false; cancelled?: false; fieldError: SettingsFieldError | null; message: string };

export interface SaveOptions {
  /** Toast on success (default "Saved"). */
  success?: string;
}

/**
 * Save a patch: an in-app confirm first when it contains risky changes (listener on, remote control on, new start
 * folders, custom agent commands), then `update_settings`. Field errors come back for the form to show.
 */
export function useSaveSettings(): (patch: HubSettingsPatch, opts?: SaveOptions) => Promise<SaveOutcome> {
  const { client, toast } = usePanel();
  const confirm = useConfirm();
  return useCallback(
    async (patch, opts) => {
      const st = client.getState();
      const current = st.settings;
      if (!current) return { ok: false, fieldError: null, message: "The settings have not loaded yet." };
      if (isEmptyPatch(patch)) return { ok: true, restart: [] };
      const risky = riskyChanges(current, patch, {
        machineName: st.machine ? machineDisplayName(st.machine) : undefined,
        acceptIntents: patch.peer?.acceptIntents ?? current.peer.acceptIntents,
        homeDir: await homeFolder(),
      });
      if (risky.length > 0 && !(await confirm.risky(risky))) return { ok: false, cancelled: true };
      try {
        const res = await client.updateSettings(patch);
        const changed = patchKeys(patch);
        const restart = (res.restartRequired ?? []).filter((r) => changed.some((k) => fieldMatches(r, k) || fieldMatches(k, r)));
        toast(restart.length > 0 ? `Saved. ${restartText(restart)}` : (opts?.success ?? "Saved"));
        return { ok: true, restart };
      } catch (err) {
        return { ok: false, fieldError: parseSettingsError(err), message: settingsErrorText(err) };
      }
    },
    [client, toast, confirm],
  );
}

/** Why `restart_hub` failed (an older hub has no restart). */
export function restartErrorText(err: unknown): string {
  const code = err && typeof err === "object" && "code" in err ? String((err as { code: unknown }).code) : "";
  if (code === "unknown_type" || code === "unknown_message" || code === "unsupported" || code === "bad_request") {
    return "This hub can't restart itself. Update krypto, or restart it where it runs.";
  }
  return settingsErrorText(err);
}

/**
 * "Restart hub" with an in-app confirm: running sessions end, paired machines reconnect. While it restarts the
 * connection phase is "restarting" (banner + spinner) until the new hub says hello.
 */
export function useRestartHub(): { restart(pending?: readonly string[]): Promise<void>; restarting: boolean; available: boolean } {
  const { conn, client, meta, toast } = usePanel();
  const confirm = useConfirm();
  const restart = useCallback(
    async (pending: readonly string[] = []) => {
      const st = client.getState();
      const own = st.machine?.id;
      const running = Object.values(st.sessions).filter((s) => !s.endedAt && s.source === "owned" && (!own || s.machineId === own)).length;
      const applies = restartText(pending);
      const ok = await confirm.ask({
        title: "Restart the hub?",
        body: [
          "Running sessions end; paired machines reconnect.",
          running > 0
            ? `${running} session${running === 1 ? "" : "s"} Krypto started on this machine will stop. Sessions you started yourself in a terminal or editor keep running.`
            : "No Krypto session is running on this machine right now.",
          "The hub starts again by itself in a few seconds" + (applies ? ` and applies what was saved (${applies.replace(/^Restart the hub to apply: /, "").replace(/\.$/, "")}).` : "."),
        ].join(" "),
        confirmLabel: "Restart hub",
        tone: "danger",
      });
      if (!ok) return;
      try {
        await conn.restartHub();
        toast("Restarting the hub…");
      } catch (err) {
        toast(restartErrorText(err), "error");
      }
    },
    [client, conn, confirm, toast],
  );
  return { restart, restarting: meta.phase === "restarting", available: meta.phase === "connected" };
}

/** Per-section save state: busy flag, field errors (client + hub) and a form-level message. */
export function useSectionSave(fieldKeys: readonly string[]) {
  const save = useSaveSettings();
  const [saving, setSaving] = useState(false);
  const [errors, setErrors] = useState<FormErrors>({});
  const [message, setMessage] = useState<string | null>(null);
  const keys = useRef(fieldKeys);
  keys.current = fieldKeys;

  const run = useCallback(
    async (result: { patch: HubSettingsPatch; errors: FormErrors }, opts?: SaveOptions): Promise<boolean> => {
      setMessage(null);
      if (Object.keys(result.errors).length > 0) {
        setErrors(result.errors);
        return false;
      }
      setErrors({});
      if (isEmptyPatch(result.patch)) return true;
      setSaving(true);
      try {
        const out = await save(result.patch, opts);
        if (out.ok) return true;
        if (out.cancelled) return false;
        const fe = out.fieldError;
        if (fe && keys.current.some((k) => fieldMatches(fe.field, k))) setErrors({ [fe.field]: fe.reason });
        else setMessage(out.message);
        return false;
      } finally {
        setSaving(false);
      }
    },
    [save],
  );

  const clear = useCallback(() => {
    setErrors({});
    setMessage(null);
  }, []);

  return { run, saving, errors, message, clear, setErrors };
}

/** The error for a field (exact key or one of its items). */
export function errorFor(errors: FormErrors, key: string): string | null {
  if (errors[key]) return errors[key]!;
  for (const [k, v] of Object.entries(errors)) if (fieldMatches(k, key)) return v;
  return null;
}

// ---------------------------------------------------------------- drafts

function same(a: unknown, b: unknown): boolean {
  return JSON.stringify(a) === JSON.stringify(b);
}

/**
 * A form draft of `source`. While untouched it follows `source` (another client's save shows up at once); once
 * edited it keeps the user's values until saved or reset.
 */
export function useDraft<T>(source: T) {
  const [draft, setDraft] = useState<T>(source);
  const base = useRef(source);
  useEffect(() => {
    const prev = base.current;
    base.current = source;
    setDraft((d) => (same(d, prev) ? source : d));
  }, [source]);
  const dirty = !same(draft, source);
  const update = useCallback((patch: Partial<T>) => setDraft((d) => ({ ...d, ...patch })), []);
  const reset = useCallback(() => setDraft(base.current), []);
  return { draft, setDraft, update, dirty, reset };
}

// ---------------------------------------------------------------- components

export function SettingsSection(props: { id: string; title: string; description?: ReactNode; aside?: ReactNode; children: ReactNode }) {
  return (
    <section className="card settings-section" id={`settings-${props.id}`} aria-labelledby={`settings-${props.id}-title`}>
      <header className="settings-section-head">
        <div>
          <h2 id={`settings-${props.id}-title`}>{props.title}</h2>
          {props.description && <p className="hint">{props.description}</p>}
        </div>
        {props.aside}
      </header>
      {props.children}
    </section>
  );
}

/** A labelled field with a hint, or its error in place of the hint. */
export function Field(props: {
  label: ReactNode;
  hint?: ReactNode;
  error?: string | null;
  children: (a: { id: string; invalid: boolean; describedBy: string }) => ReactNode;
  wide?: boolean;
}) {
  const id = useId();
  const describedBy = `${id}-note`;
  return (
    <div className={`field settings-field${props.wide ? " is-wide" : ""}`}>
      <label className="field-label" htmlFor={id}>
        {props.label}
      </label>
      {props.children({ id, invalid: !!props.error, describedBy })}
      {props.error ? (
        <span className="field-error" id={describedBy} role="alert">
          {props.error}
        </span>
      ) : props.hint ? (
        <span className="hint" id={describedBy}>
          {props.hint}
        </span>
      ) : null}
    </div>
  );
}

export function Toggle(props: { label: string; hint?: ReactNode; checked: boolean; onChange(): void; disabled?: boolean }) {
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

/** "Set by --port when the hub started": the value is pinned until the hub restarts without it. */
export function OverrideNote({ settings, field }: { settings: HubSettings; field: string }) {
  const by = settings.overriddenBy?.[field];
  if (!by) return null;
  return (
    <span className="override-note">
      Set by <span className="mono">{by}</span> when the hub started, which wins over this setting. A saved value applies once
      the hub starts without it.
    </span>
  );
}

/** Save / Discard for a section, shown while it has changes (or an error). */
export function SaveBar(props: { dirty: boolean; saving: boolean; disabled?: boolean; message?: string | null; onSave(): void; onReset(): void; saveLabel?: string }) {
  if (!props.dirty && !props.message) return null;
  return (
    <div className="save-bar">
      {props.message ? (
        <span className="field-error" role="alert">
          {props.message}
        </span>
      ) : (
        <span className="hint">Unsaved changes</span>
      )}
      <span className="spacer" />
      {props.dirty && (
        <>
          <button className="pill pill-ghost" disabled={props.saving} onClick={props.onReset}>
            Discard
          </button>
          <button className="pill pill-primary" disabled={props.saving || props.disabled} onClick={props.onSave}>
            {props.saving ? "Saving…" : (props.saveLabel ?? "Save")}
          </button>
        </>
      )}
    </div>
  );
}

/**
 * A write-only secret: shows only whether it is set. Set / Replace opens an empty password field; Clear asks first.
 * The value is never read back, shown, logged or kept after saving.
 */
export function SecretField(props: {
  label: string;
  isSet: boolean;
  disabled?: boolean;
  hint?: ReactNode;
  placeholder?: string;
  /** What clearing stops (for the confirm). */
  clearEffect: string;
  overriddenBy?: string | undefined;
  onSave(value: string | null): Promise<{ ok: boolean; error?: string | null }>;
}) {
  const confirm = useConfirm();
  const [editing, setEditing] = useState(false);
  const [value, setValue] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const id = useId();

  const stop = () => {
    setEditing(false);
    setValue("");
    setError(null);
  };

  const submit = async () => {
    const v = value.trim();
    if (!v) {
      setError(`Paste the ${props.label.toLowerCase()} first.`);
      return;
    }
    if (/\s/.test(v)) {
      setError("A key has no spaces or line breaks.");
      return;
    }
    setBusy(true);
    try {
      const res = await props.onSave(v);
      if (res.ok) stop();
      else {
        setValue("");
        setError(res.error ?? null);
      }
    } finally {
      setBusy(false);
    }
  };

  const clear = async () => {
    const ok = await confirm.ask({
      title: `Clear the ${props.label.toLowerCase()}?`,
      body: `${props.clearEffect} You can set a new one at any time.`,
      confirmLabel: "Clear key",
      tone: "danger",
    });
    if (!ok) return;
    setBusy(true);
    try {
      const res = await props.onSave(null);
      setError(res.ok ? null : (res.error ?? null));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="field settings-field secret-field">
      <span className="field-label" id={`${id}-label`}>
        {props.label}
        <span className={`chip ${props.isSet ? "chip-good" : "chip-soft"}`}>{props.isSet ? "Set" : "Not set"}</span>
      </span>
      {editing ? (
        <form
          className="input-row"
          onSubmit={(e) => {
            e.preventDefault();
            void submit();
          }}
        >
          <input
            className={`input mono${error ? " is-invalid" : ""}`}
            type="password"
            aria-labelledby={`${id}-label`}
            autoComplete="off"
            spellCheck={false}
            autoFocus
            value={value}
            placeholder={props.placeholder}
            disabled={busy}
            onChange={(e) => {
              setValue(e.target.value);
              setError(null);
            }}
            onKeyDown={(e) => {
              if (e.key === "Escape") stop();
            }}
          />
          <button className="pill pill-ghost" type="button" disabled={busy} onClick={stop}>
            Cancel
          </button>
          <button className="pill pill-primary" type="submit" disabled={busy || !value.trim()}>
            {busy ? "Saving…" : "Save key"}
          </button>
        </form>
      ) : (
        <span className="input-row">
          <button className="pill" disabled={props.disabled || busy} onClick={() => setEditing(true)}>
            {props.isSet ? "Replace…" : "Set key…"}
          </button>
          {props.isSet && (
            <button className="pill pill-ghost" disabled={props.disabled || busy} onClick={() => void clear()}>
              Clear
            </button>
          )}
        </span>
      )}
      {error ? (
        <span className="field-error" role="alert">
          {error}
        </span>
      ) : (
        <span className="hint">
          {props.hint}
          {props.overriddenBy && (
            <>
              {" "}
              <span className="override-note">
                The hub also sees <span className="mono">{props.overriddenBy}</span>; a key saved here is used first.
              </span>
            </>
          )}
        </span>
      )}
    </div>
  );
}

/** This user's home folder once known (null outside Tauri). */
export function useHomeFolder(): string | null {
  const [home, setHome] = useState<string | null>(null);
  useEffect(() => {
    let gone = false;
    void homeFolder().then((h) => !gone && setHome(h));
    return () => {
      gone = true;
    };
  }, []);
  return home;
}

/**
 * A list of folders with add (typed or picked) and remove. Errors per item come as `<field>[i]`. Very broad folders
 * (a drive, a home folder) carry a warning chip; saving them asks first.
 */
export function FolderList(props: {
  field: string;
  label: string;
  folders: string[];
  errors: FormErrors;
  disabled?: boolean;
  empty: string;
  addLabel: string;
  pickTitle: string;
  onChange(next: string[]): void;
}) {
  const [typed, setTyped] = useState("");
  const inputId = useId();
  const home = useHomeFolder();
  const add = (path: string) => {
    const p = path.trim();
    if (!p) return;
    props.onChange([...props.folders, p]);
    setTyped("");
  };
  const pick = async () => {
    try {
      const picked = await open({ directory: true, multiple: false, title: props.pickTitle });
      if (typeof picked === "string") add(picked);
    } catch {
      /* dialog closed or unavailable */
    }
  };
  const listError = props.errors[props.field];
  const indexError = useMemo(() => {
    const out = new Map<number, string>();
    for (const [k, v] of Object.entries(props.errors)) {
      const m = /\[(\d+)\]$/.exec(k);
      if (m && k.startsWith(props.field)) out.set(Number(m[1]), v);
    }
    return out;
  }, [props.errors, props.field]);

  return (
    <div className="field settings-field is-wide">
      <span className="field-label">{props.label}</span>
      {props.folders.length === 0 ? (
        <span className="hint folder-empty">{props.empty}</span>
      ) : (
        <ul className="folder-list">
          {props.folders.map((f, i) => (
            <li key={`${i}:${f}`} className={indexError.has(i) ? "is-invalid" : undefined}>
              <span className="mono folder-path" title={f}>
                {f}
              </span>
              {broadFolderReason(f, home) && (
                <span className="chip chip-warn" title={`${f} is ${broadFolderReason(f, home)}: any paired machine could start agents anywhere under it`}>
                  very broad
                </span>
              )}
              <button className="icon-btn" title={`Remove ${f}`} aria-label={`Remove ${f}`} disabled={props.disabled} onClick={() => props.onChange(props.folders.filter((_, j) => j !== i))}>
                ×
              </button>
              {indexError.has(i) && (
                <span className="field-error folder-error" role="alert">
                  {indexError.get(i)}
                </span>
              )}
            </li>
          ))}
        </ul>
      )}
      <span className="input-row">
        <input
          id={inputId}
          className="input mono"
          value={typed}
          placeholder="E:\work\project"
          aria-label={props.addLabel}
          spellCheck={false}
          disabled={props.disabled}
          onChange={(e) => setTyped(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") {
              e.preventDefault();
              add(typed);
            }
          }}
        />
        <button className="pill" disabled={props.disabled || !typed.trim()} onClick={() => add(typed)}>
          Add
        </button>
        {isTauri && (
          <button className="pill" disabled={props.disabled} onClick={() => void pick()}>
            Browse…
          </button>
        )}
      </span>
      {listError && (
        <span className="field-error" role="alert">
          {listError}
        </span>
      )}
    </div>
  );
}

/** "Saved; restart the hub to apply" chip next to a field that waits for a restart. */
export function RestartChip({ restartRequired, field }: { restartRequired: string[]; field: string }) {
  if (!restartRequired.some((r) => fieldMatches(r, field))) return null;
  return (
    <span className="chip chip-warn" title={`${settingsFieldLabel(field)} is saved and applies after a hub restart`}>
      restart needed
    </span>
  );
}
