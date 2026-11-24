// Agents & hooks: per-agent hook status with preview → apply / remove (the same code as `pawl install-hooks`),
// the agents found on this machine, and the custom agents list. `HooksPanel` is also the first-run setup's step.
import { useCallback, useEffect, useState } from "react";
import {
  CUSTOM_AGENTS_MAX,
  formatCommandLine,
  HOOK_STATE_LABEL,
  hookAction,
  hookKey,
  hookLabel,
  hookRemovable,
  hookRemoveConfirm,
  settingsErrorText,
} from "@pawl/hub-client";
import type { CustomAgentSetting, HookStatus, HubSettings } from "@pawl/protocol";
import { agentDraft, applyAgentDraft, customAgentsPatch, suggestAgentId, type AgentDraft, type FormErrors } from "../../../lib/settingsForms";
import { useConfirm, withCode } from "../../Confirm";
import { usePanel } from "../../context";
import { errorFor, Field, SettingsSection, useSaveSettings } from "./shared";
import type { SectionProps } from "./sections";

const STATE_TONE: Record<HookStatus["state"], string> = {
  installed: "done",
  outdated: "waiting",
  missing: "idle",
  unsupported: "idle",
};

/** Hook status rows. `foundOnly` lists only agents found on this machine (first-run setup). */
export function HooksPanel({ live, foundOnly }: { live: boolean; foundOnly?: boolean }) {
  const { client, toast } = usePanel();
  const confirm = useConfirm();
  const [hooks, setHooks] = useState<HookStatus[] | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [plans, setPlans] = useState<Record<string, string>>({});
  const [busy, setBusy] = useState<string | null>(null);
  const [rowError, setRowError] = useState<Record<string, string>>({});

  const load = useCallback(async () => {
    try {
      setHooks(await client.hookStatus());
      setLoadError(null);
    } catch (err) {
      setLoadError(settingsErrorText(err));
    }
  }, [client]);

  useEffect(() => {
    if (live) void load();
  }, [live, load]);

  const replace = (status: HookStatus) =>
    setHooks((list) => (list ?? []).map((h) => (hookKey(h) === hookKey(status) ? status : h)));

  const act = async (h: HookStatus, action: "preview" | "apply" | "remove") => {
    const key = hookKey(h);
    if (action === "remove") {
      const c = hookRemoveConfirm(h);
      if (!(await confirm.ask({ title: c.title, body: withCode(c.body), confirmLabel: c.confirm, tone: "danger" }))) return;
    }
    setBusy(key);
    setRowError(({ [key]: _gone, ...rest }) => rest);
    try {
      const res = await client.installHooks(h.target, action, { repo: h.repo });
      if (action === "preview") {
        setPlans((p) => ({ ...p, [key]: res.plan }));
      } else {
        setPlans(({ [key]: _gone, ...rest }) => rest);
        replace(res.status);
        toast(action === "remove" ? `Removed the hooks for ${hookLabel(h)}` : `Hooks ready for ${hookLabel(h)}`);
      }
    } catch (err) {
      setRowError((e) => ({ ...e, [key]: settingsErrorText(err) }));
    } finally {
      setBusy(null);
    }
  };

  if (!live) return <p className="hint">Connect the hub to see and install agent hooks.</p>;
  if (loadError)
    return (
      <p className="field-error" role="alert">
        {loadError}{" "}
        <button className="link" onClick={() => void load()}>
          Try again
        </button>
      </p>
    );
  if (!hooks) return <p className="hint">Checking agent hooks…</p>;

  const shown = foundOnly ? hooks.filter((h) => h.agentInstalled) : hooks;
  const missing = foundOnly ? hooks.filter((h) => !h.agentInstalled) : [];

  return (
    <div className="hooks">
      {shown.length === 0 && <p className="hint">No supported agent was found on this machine. Install Claude Code, Codex or Copilot, then check again.</p>}
      {shown.map((h) => {
        const key = hookKey(h);
        const plan = plans[key];
        const next = hookAction(h);
        const rowBusy = busy === key;
        return (
          <div key={key} className={`hook-row${h.agentInstalled ? "" : " is-absent"}`}>
            <div className="hook-row-main">
              <div className="hook-row-title">
                <strong>{hookLabel(h)}</strong>
                <span className={`status-pill tone-${STATE_TONE[h.state]}`}>{HOOK_STATE_LABEL[h.state]}</span>
                {!h.agentInstalled && <span className="chip chip-soft">agent not found</span>}
              </div>
              <span className="mono muted hook-path" title={h.configPath}>
                {h.configPath}
              </span>
              {h.detail && <span className="hint">{h.detail}</span>}
              {rowError[key] && (
                <span className="field-error" role="alert">
                  {rowError[key]}
                </span>
              )}
            </div>
            <div className="hook-row-actions">
              {next && !plan && (
                <button className="pill" disabled={rowBusy || busy !== null} onClick={() => void act(h, "preview")}>
                  {rowBusy ? "Checking…" : next === "install" ? "Preview install" : "Preview update"}
                </button>
              )}
              {hookRemovable(h) && !plan && (
                <button className="pill pill-ghost" disabled={busy !== null} onClick={() => void act(h, "remove")}>
                  Remove
                </button>
              )}
            </div>
            {plan !== undefined && (
              <div className="hook-plan">
                <span className="hint">This is the exact change to {h.configPath}. Nothing is written until you apply it.</span>
                <pre className="plan-preview">{plan || "(no change needed)"}</pre>
                <div className="form-actions">
                  <button className="pill pill-ghost" disabled={rowBusy} onClick={() => setPlans(({ [key]: _gone, ...rest }) => rest)}>
                    Cancel
                  </button>
                  <button className="pill pill-primary" disabled={rowBusy} onClick={() => void act(h, "apply")}>
                    {rowBusy ? "Applying…" : next === "update" ? "Apply update" : "Install hooks"}
                  </button>
                </div>
              </div>
            )}
          </div>
        );
      })}
      {missing.length > 0 && <p className="hint">Not found on this machine: {missing.map(hookLabel).join(", ")}.</p>}
      <button className="link" onClick={() => void load()}>
        Check again
      </button>
    </div>
  );
}

/** Custom ACP agents: list, add, edit (id, name, command line), remove. */
function CustomAgents({ settings, live }: { settings: HubSettings; live: boolean }) {
  const { state } = usePanel();
  const save = useSaveSettings();
  const [editing, setEditing] = useState<number | "new" | null>(null);
  const [draft, setDraft] = useState<AgentDraft>(agentDraft());
  const [errors, setErrors] = useState<FormErrors>({});
  const [message, setMessage] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const own = state.machine?.id;
  const builtinIds = state.agents.filter((a) => a.source !== "custom" && (!own || a.machineId === own)).map((a) => a.id);
  const list = settings.customAgents;
  const at = editing === "new" ? list.length : (editing ?? 0);

  const start = (index: number | "new") => {
    setEditing(index);
    setDraft(agentDraft(index === "new" ? undefined : list[index]));
    setErrors({});
    setMessage(null);
  };
  const stop = () => {
    setEditing(null);
    setErrors({});
    setMessage(null);
  };

  const persist = async (next: CustomAgentSetting[], success: string): Promise<boolean> => {
    setSaving(true);
    setMessage(null);
    try {
      const out = await save(customAgentsPatch(next, settings), { success });
      if (out.ok) return true;
      if (!out.cancelled) {
        if (out.fieldError?.field.startsWith("customAgents")) setErrors({ [out.fieldError.field]: out.fieldError.reason });
        else setMessage(out.message);
      }
      return false;
    } finally {
      setSaving(false);
    }
  };

  const submit = async () => {
    const res = applyAgentDraft(list, editing === "new" ? null : editing, draft, builtinIds);
    setErrors(res.errors);
    if (!res.list) return;
    if (await persist(res.list, editing === "new" ? `Added ${draft.name.trim()}` : `Saved ${draft.name.trim()}`)) stop();
  };

  const remove = async (i: number) => {
    const a = list[i]!;
    const next = list.filter((_, j) => j !== i);
    // Errors show under the list (persist sets them).
    await persist(next, `Removed ${a.name}`);
  };

  const err = (k: string) => errorFor(errors, `customAgents[${at}].${k}`);

  return (
    <div className="custom-agents">
      <div className="subhead">
        <h3>Custom agents</h3>
        <span className="muted">
          {list.length}/{CUSTOM_AGENTS_MAX}
        </span>
      </div>
      <p className="hint">Any ACP agent you run yourself: the hub starts its command when a session starts.</p>
      {list.length === 0 && editing !== "new" && <p className="hint">None yet.</p>}
      <ul className="agent-list">
        {list.map((a, i) =>
          editing === i ? null : (
            <li key={a.id}>
              <div className="agent-list-main">
                <strong>{a.name}</strong> <span className="chip chip-soft mono">{a.id}</span>
                <code className="mono agent-cmd" title={formatCommandLine(a.command)}>
                  {formatCommandLine(a.command)}
                </code>
              </div>
              <button className="pill pill-ghost" disabled={!live || saving || editing !== null} onClick={() => start(i)}>
                Edit
              </button>
              <button className="pill pill-ghost" disabled={!live || saving || editing !== null} onClick={() => void remove(i)}>
                Remove
              </button>
            </li>
          ),
        )}
      </ul>
      {editing !== null && (
        <form
          className="agent-editor"
          onSubmit={(e) => {
            e.preventDefault();
            void submit();
          }}
        >
          <h4>{editing === "new" ? "Add a custom agent" : `Edit ${list[editing]?.name ?? ""}`}</h4>
          <div className="settings-fields">
            <Field label="Name" error={err("name")} hint="Shown on pets and in pickers.">
              {(a) => (
                <input
                  id={a.id}
                  className={`input${a.invalid ? " is-invalid" : ""}`}
                  value={draft.name}
                  autoFocus
                  disabled={saving}
                  aria-invalid={a.invalid || undefined}
                  aria-describedby={a.describedBy}
                  onChange={(e) => {
                    const name = e.target.value;
                    // Suggest an id from the name for a new agent until the id is typed by hand.
                    const auto = editing === "new" && (draft.id === "" || draft.id === suggestAgentId(draft.name));
                    setDraft({ ...draft, name, id: auto ? suggestAgentId(name) : draft.id });
                  }}
                />
              )}
            </Field>
            <Field label="Id" error={err("id")} hint="Lowercase letters, digits and dashes; unique.">
              {(a) => (
                <input
                  id={a.id}
                  className={`input mono${a.invalid ? " is-invalid" : ""}`}
                  value={draft.id}
                  spellCheck={false}
                  disabled={saving}
                  aria-invalid={a.invalid || undefined}
                  aria-describedby={a.describedBy}
                  onChange={(e) => setDraft({ ...draft, id: e.target.value })}
                />
              )}
            </Field>
            <Field wide label="Command" error={err("command")} hint='The command that starts the agent as an ACP server over stdio. Quote parts with spaces: "C:\Program Files\agent\agent.exe" --acp'>
              {(a) => (
                <input
                  id={a.id}
                  className={`input mono${a.invalid ? " is-invalid" : ""}`}
                  value={draft.commandLine}
                  placeholder="npx -y my-agent --acp"
                  spellCheck={false}
                  disabled={saving}
                  aria-invalid={a.invalid || undefined}
                  aria-describedby={a.describedBy}
                  onChange={(e) => setDraft({ ...draft, commandLine: e.target.value })}
                />
              )}
            </Field>
          </div>
          {(message || errors["customAgents"]) && (
            <span className="field-error" role="alert">
              {message ?? errors["customAgents"]}
            </span>
          )}
          <div className="form-actions">
            <button className="pill pill-ghost" type="button" disabled={saving} onClick={stop}>
              Cancel
            </button>
            <button className="pill pill-primary" type="submit" disabled={!live || saving}>
              {saving ? "Saving…" : editing === "new" ? "Add agent" : "Save agent"}
            </button>
          </div>
        </form>
      )}
      {editing === null && (
        <>
          {message && (
            <span className="field-error" role="alert">
              {message}
            </span>
          )}
          <button className="pill" disabled={!live || saving || list.length >= CUSTOM_AGENTS_MAX} onClick={() => start("new")}>
            Add custom agent
          </button>
        </>
      )}
    </div>
  );
}

export function AgentsSection({ settings, live }: SectionProps) {
  const { state } = usePanel();
  const own = state.machine?.id;
  const found = state.agents.filter((a) => a.installed && (!own || a.machineId === own));
  return (
    <SettingsSection
      id="agents"
      title="Agents & hooks"
      description="Hooks let Pawl follow agent sessions you start yourself (in a terminal or editor). Pawl's own sessions need no hooks."
    >
      <p className="hint">
        Agents on this machine: {found.length > 0 ? found.map((a) => a.name).join(", ") : live ? "none found yet" : "…"}.
      </p>
      <HooksPanel live={live} />
      <CustomAgents settings={settings} live={live} />
    </SettingsSection>
  );
}
