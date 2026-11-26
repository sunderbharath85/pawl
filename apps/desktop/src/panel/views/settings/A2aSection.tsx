// Settings > A2A (PLAN.md "A2A orchestration"): coding agents connected to Pawl in one click (a "pawl" MCP entry in
// their own config, docs/CONTRACTS.md "v0.2 A2A: connect coding agents"), Pawl as an A2A agent for programs on this
// PC (off by default, on 127.0.0.1 only, each caller with its own token, shown once), and outside A2A agents Pawl
// hands work to. Everything goes through the hub (update_settings `a2a`, `a2a_rotate_token`, `a2a_connect`); risky
// changes ask first.
import { useCallback, useEffect, useState } from "react";
import type { A2aAgentSetting, A2aCallerSetting, A2aConnectStatus, A2aSettings } from "@pawl/protocol";
import {
  A2A_AGENTS_MAX,
  A2A_CONNECT_STATE_LABEL,
  A2A_CONNECT_STATE_TONE,
  a2aConnectAction,
  a2aConnectConfirm,
  a2aConnectDoneText,
  a2aConnectName,
  a2aConnectPreviewable,
  a2aConnectRemovable,
  a2aConnectRemoveConfirm,
  a2aConnectServerProblem,
  callerConnectTarget,
  ASSISTANT_CALLER_HINT,
  isAssistantCaller,
  connectCallerHint,
  connectFacts,
  hubLacksMessage,
  a2aCardUrl,
  a2aServerPatch,
  a2aServerStatus,
  addAgentPatch,
  addCallerPatch,
  agentTokenPatch,
  callerApprovalsPatch,
  checkA2aAgentUrl,
  checkA2aName,
  checkA2aToken,
  checkPort,
  isLoopbackUrl,
  removeAgentPatch,
  removeCallerPatch,
  settingsErrorText,
  urlHost,
} from "@pawl/hub-client";
import { timeAgo } from "../../../lib/format";
import { useConfirm, withCode } from "../../Confirm";
import { usePanel } from "../../context";
import { CopyField } from "../../CopyField";
import { SettingsSection, Toggle, useSaveSettings, type SaveOutcome } from "./shared";
import type { SectionProps } from "./sections";

const SKILLS_SHOWN = 8;

export function A2aSection({ settings, live }: SectionProps) {
  const a2a = settings.a2a;
  return (
    <SettingsSection
      id="a2a"
      title="A2A"
      description="Agent2Agent (A2A): other programs on this PC can talk to Pawl, and Pawl can hand work to outside A2A agents. Both are off until you set them up."
    >
      {!a2a ? (
        <p className="hint">This hub does not speak A2A yet. Update pawl on this machine and restart the hub.</p>
      ) : (
        <A2aParts a2a={a2a} live={live} />
      )}
    </SettingsSection>
  );
}

function A2aParts({ a2a, live }: { a2a: A2aSettings; live: boolean }) {
  const connect = useConnectAgents(a2a, live);
  return (
    <>
      <ConnectPart connect={connect} a2a={a2a} live={live} />
      <ServerPart a2a={a2a} live={live} />
      <CallersPart a2a={a2a} live={live} connect={connect} />
      <AgentsPart a2a={a2a} live={live} />
    </>
  );
}

function outcomeError(out: SaveOutcome): string | null {
  if (out.ok || out.cancelled) return null;
  return out.fieldError ? out.fieldError.reason : out.message;
}

// ---------------------------------------------------------------- connect coding agents (`pawl mcp`)

/**
 * The coding agents' rows and what can be done with them, shared by the Connect list and the Callers list (a caller
 * Connect made is removed the same way: entry and caller together). Connect gives the agent its own caller and token
 * and may turn the A2A server on, so the settings (server part, callers) are fetched again afterwards.
 */
function useConnectAgents(a2a: A2aSettings, live: boolean) {
  const { client, toast } = usePanel();
  const confirm = useConfirm();
  const [rows, setRows] = useState<A2aConnectStatus[] | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  /** The hub is older than this feature. */
  const [old, setOld] = useState(false);
  const [plans, setPlans] = useState<Record<string, string>>({});
  const [busy, setBusy] = useState<string | null>(null);
  const [rowError, setRowError] = useState<Record<string, string>>({});
  /** Remove revoked the token but could not edit the agent's file: a warning, not an error. */
  const [rowWarning, setRowWarning] = useState<Record<string, string>>({});

  const load = useCallback(async () => {
    try {
      setRows(await client.a2aConnectStatus());
      setOld(false);
      setLoadError(null);
    } catch (err) {
      if (hubLacksMessage(err, "a2a_connect_status")) setOld(true);
      else setLoadError(settingsErrorText(err));
    }
  }, [client]);

  // Again when the server's port or the callers change: either can make a row outdated.
  const signature = `${a2a.port}|${a2a.callers.map((c) => `${c.id}:${c.tokenSet}`).join(",")}`;
  useEffect(() => {
    if (live) void load();
  }, [live, load, signature]);

  const replace = (status: A2aConnectStatus) => setRows((list) => (list ?? []).map((r) => (r.target === status.target ? status : r)));
  const failed = (key: string, err: unknown, type: string) => {
    if (hubLacksMessage(err, type)) setOld(true);
    else setRowError((e) => ({ ...e, [key]: settingsErrorText(err) }));
  };
  const begin = (key: string) => {
    setBusy(key);
    setRowError(({ [key]: _gone, ...rest }) => rest);
    setRowWarning(({ [key]: _gone, ...rest }) => rest);
  };
  /** The A2A server and the callers changed on the hub: fetch the settings again. */
  const refreshSettings = () => client.getSettings().catch(() => {});

  const preview = async (r: A2aConnectStatus) => {
    begin(r.target);
    try {
      const res = await client.a2aConnect(r.target, "preview");
      setPlans((p) => ({ ...p, [r.target]: res.plan }));
    } catch (err) {
      failed(r.target, err, "a2a_connect");
    } finally {
      setBusy(null);
    }
  };

  const connect = async (r: A2aConnectStatus) => {
    begin(r.target);
    try {
      // The confirm says whether Connect turns the A2A server on: a fresh preview tells (nothing is written).
      const plan = await client.a2aConnect(r.target, "preview");
      const c = a2aConnectConfirm(r, connectFacts(plan, a2a.callers));
      if (!(await confirm.ask({ title: c.title, body: withCode(c.body), confirmLabel: c.confirm, tone: "primary" }))) return;
      const res = await client.a2aConnect(r.target, "apply");
      replace(res.status);
      setPlans(({ [r.target]: _gone, ...rest }) => rest);
      toast(a2aConnectDoneText(r, "apply", res.applied));
      await refreshSettings();
    } catch (err) {
      failed(r.target, err, "a2a_connect");
    } finally {
      setBusy(null);
    }
  };

  /** Remove (asks first, danger): the entry and the caller. From a row, or from a Connect caller in Callers. */
  const remove = async (
    r: Pick<A2aConnectStatus, "target"> & { name?: string; configPath?: string; state?: A2aConnectStatus["state"]; callerExists?: boolean; callerMarked?: boolean },
  ) => {
    const c = a2aConnectRemoveConfirm(r);
    if (!(await confirm.ask({ title: c.title, body: withCode(c.body), confirmLabel: c.confirm, tone: "danger" }))) return;
    begin(r.target);
    try {
      const res = await client.a2aConnect(r.target, "remove");
      replace(res.status);
      setPlans(({ [r.target]: _gone, ...rest }) => rest);
      if (res.warning) setRowWarning((w) => ({ ...w, [r.target]: res.warning! }));
      toast(a2aConnectDoneText(r, "remove", res.applied, res.warning));
      await refreshSettings();
    } catch (err) {
      failed(r.target, err, "a2a_connect");
    } finally {
      setBusy(null);
    }
  };

  const hidePlan = (target: string) => setPlans(({ [target]: _gone, ...rest }) => rest);
  return { rows, loadError, old, plans, busy, rowError, rowWarning, load, preview, connect, remove, hidePlan };
}

type ConnectAgents = ReturnType<typeof useConnectAgents>;

/**
 * One row per coding agent (Claude Code, Codex, Copilot CLI, Copilot in VS Code), like the hook rows in Agents & hooks:
 * Preview shows the exact plan (nothing written); Connect / Update and Remove ask first.
 */
function ConnectPart({ connect: agents, a2a, live }: { connect: ConnectAgents; a2a: A2aSettings; live: boolean }) {
  const { rows, loadError, old, plans, busy, rowError, rowWarning, load, preview, connect, remove, hidePlan } = agents;
  let body;
  if (!live) body = <p className="hint">Connect the hub to see the coding agents on this PC.</p>;
  else if (old) body = <p className="hint a2a-connect-old">Update Pawl to connect coding agents.</p>;
  else if (loadError)
    body = (
      <p className="field-error" role="alert">
        {loadError}{" "}
        <button className="link" onClick={() => void load()}>
          Try again
        </button>
      </p>
    );
  else if (!rows) body = <p className="hint">Checking the coding agents…</p>;
  else
    body = (
      <div className="hooks a2a-connect-rows">
        {rows.map((r) => {
          const key = r.target;
          const plan = plans[key];
          const next = a2aConnectAction(r);
          const rowBusy = busy === key;
          const serverProblem = a2aConnectServerProblem(r, a2a);
          return (
            <div key={key} className={`hook-row a2a-connect-row${r.agentInstalled ? "" : " is-absent"}`} data-target={key}>
              <div className="hook-row-main">
                <div className="hook-row-title">
                  <strong>{a2aConnectName(r)}</strong>
                  <span className={`status-pill tone-${A2A_CONNECT_STATE_TONE[r.state]}`}>{A2A_CONNECT_STATE_LABEL[r.state]}</span>
                  {!r.agentInstalled && <span className="chip chip-soft">not found</span>}
                </div>
                <span className="mono muted hook-path" title={r.configPath}>
                  {r.configPath}
                </span>
                {r.detail && !serverProblem && <span className="hint">{r.detail}</span>}
                {serverProblem && (
                  <span className="field-error a2a-connect-server" role="alert">
                    {serverProblem}
                  </span>
                )}
                {rowWarning[key] && <span className="field-warning a2a-connect-warning">{rowWarning[key]}</span>}
                {rowError[key] && (
                  <span className="field-error" role="alert">
                    {rowError[key]}
                  </span>
                )}
              </div>
              <div className="hook-row-actions">
                {a2aConnectPreviewable(r) &&
                  (plan === undefined ? (
                    <button className="pill" disabled={busy !== null} onClick={() => void preview(r)}>
                      {rowBusy ? "Checking…" : "Preview"}
                    </button>
                  ) : (
                    <button className="pill pill-ghost" disabled={rowBusy} onClick={() => hidePlan(key)}>
                      Hide preview
                    </button>
                  ))}
                {next && (
                  <button className="pill pill-primary" disabled={busy !== null} onClick={() => void connect(r)}>
                    {rowBusy ? "Working…" : next === "update" ? "Update" : "Connect"}
                  </button>
                )}
                {a2aConnectRemovable(r) && (
                  <button className="pill pill-ghost" disabled={busy !== null} onClick={() => void remove(r)}>
                    Remove
                  </button>
                )}
              </div>
              {plan !== undefined && (
                <div className="hook-plan">
                  <span className="hint">What Connect would do. Nothing is written until you connect.</span>
                  <pre className="plan-preview">{plan || "(no change needed)"}</pre>
                </div>
              )}
            </div>
          );
        })}
        <button className="link" onClick={() => void load()}>
          Check again
        </button>
      </div>
    );

  return (
    <div className="a2a-part a2a-connect">
      <h3 className="a2a-heading">Connect coding agents</h3>
      <p className="hint">
        Let Claude Code, Codex or Copilot talk to Pawl: they can start work in folders Pawl knows and check on it. Everything they do still asks you
        here.
      </p>
      {body}
    </div>
  );
}

// ---------------------------------------------------------------- the localhost server

function ServerPart({ a2a, live }: { a2a: A2aSettings; live: boolean }) {
  const save = useSaveSettings();
  const [busy, setBusy] = useState(false);
  const [port, setPort] = useState(String(a2a.port));
  const [portError, setPortError] = useState<string | null>(null);
  useEffect(() => setPort(String(a2a.port)), [a2a.port]);
  const status = a2aServerStatus(a2a);

  const run = async (patch: Parameters<typeof save>[0], onError?: (e: string) => void) => {
    setBusy(true);
    try {
      const out = await save(patch);
      const err = outcomeError(out);
      if (err) onError?.(err);
      return out.ok;
    } finally {
      setBusy(false);
    }
  };

  const portDirty = port.trim() !== String(a2a.port);
  return (
    <div className="a2a-part">
      <Toggle
        label="Let other tools talk to Pawl (A2A)"
        hint="Off by default. Programs on this PC that have a token can ask Pawl what is going on and ask it to start work in folders it already knows. It listens on 127.0.0.1 only."
        checked={a2a.server}
        disabled={!live || busy}
        onChange={() => void run(a2aServerPatch(a2a, { server: !a2a.server }), setPortError)}
      />
      <div className="field">
        <label className="field-label" htmlFor="a2a-port">
          Port
        </label>
        <span className="a2a-row">
          <input
            id="a2a-port"
            className={`input a2a-port${portError ? " is-invalid" : ""}`}
            inputMode="numeric"
            value={port}
            disabled={!live || busy}
            aria-invalid={portError ? true : undefined}
            onChange={(e) => {
              setPort(e.target.value);
              setPortError(null);
            }}
          />
          {portDirty && (
            <>
              <button
                type="button"
                className="pill pill-primary"
                disabled={!live || busy}
                onClick={() => {
                  const problem = checkPort(port);
                  if (problem) {
                    setPortError(problem);
                    return;
                  }
                  void run(a2aServerPatch(a2a, { port: Number(port.trim()) }), setPortError);
                }}
              >
                Save port
              </button>
              <button type="button" className="pill pill-ghost" onClick={() => setPort(String(a2a.port))}>
                Undo
              </button>
            </>
          )}
        </span>
        {portError ? <span className="field-error">{portError}</span> : <span className="hint">127.0.0.1 only; the default is 47802.</span>}
      </div>
      <p className={`a2a-status is-${status.tone}`} role="status">
        <span className="a2a-dot" aria-hidden /> {status.text}
      </p>
      {a2a.running && a2a.url && (
        <div className="field">
          <span className="field-label">Agent card (give this address to the tool)</span>
          <CopyField label="Agent card URL" text={a2aCardUrl(a2a.url)} />
        </div>
      )}
    </div>
  );
}

// ---------------------------------------------------------------- callers (programs that talk to Pawl)

function CallersPart({ a2a, live, connect }: { a2a: A2aSettings; live: boolean; connect: ConnectAgents }) {
  const { client } = usePanel();
  const save = useSaveSettings();
  const confirm = useConfirm();
  const [name, setName] = useState("");
  const [addError, setAddError] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  /** A token just made: shown once, here, then forgotten. */
  const [fresh, setFresh] = useState<{ callerId: string; token: string } | null>(null);
  const [rowError, setRowError] = useState<{ callerId: string; text: string } | null>(null);

  const makeToken = async (c: Pick<A2aCallerSetting, "id" | "name">, replacing: boolean) => {
    if (replacing) {
      const ok = await confirm.ask({
        title: `Make a new token for “${c.name}”?`,
        body: `The token it uses now stops working at once. Paste the new one into ${c.name}.`,
        confirmLabel: "Make new token",
        tone: "danger",
      });
      if (!ok) return;
    }
    setBusy(c.id);
    setRowError(null);
    try {
      setFresh({ callerId: c.id, token: await client.rotateA2aToken(c.id) });
    } catch (err) {
      setRowError({ callerId: c.id, text: settingsErrorText(err) });
    } finally {
      setBusy(null);
    }
  };

  const add = async () => {
    const problem = checkA2aName(name);
    if (problem) {
      setAddError(problem);
      return;
    }
    setBusy("+");
    try {
      const { patch, id } = addCallerPatch(a2a, name);
      const out = await save(patch, { success: `Added ${name.trim()}` });
      const err = outcomeError(out);
      if (err) {
        setAddError(err);
        return;
      }
      if (!out.ok) return;
      setName("");
      await makeToken({ id, name: name.trim() }, false);
    } finally {
      setBusy(null);
    }
  };

  const remove = async (c: A2aCallerSetting) => {
    const ok = await confirm.ask({
      title: `Remove “${c.name}”?`,
      body: "Its token stops working at once. Tasks it already started keep running until they end.",
      confirmLabel: "Remove caller",
      tone: "danger",
    });
    if (!ok) return;
    setBusy(c.id);
    try {
      const out = await save(removeCallerPatch(a2a, c.id), { success: `Removed ${c.name}` });
      const err = outcomeError(out);
      if (err) setRowError({ callerId: c.id, text: err });
      if (fresh?.callerId === c.id) setFresh(null);
    } finally {
      setBusy(null);
    }
  };

  const setApprovals = async (c: A2aCallerSetting) => {
    setBusy(c.id);
    try {
      const out = await save(callerApprovalsPatch(a2a, c.id, !c.allowApprovals));
      const err = outcomeError(out);
      if (err) setRowError({ callerId: c.id, text: err });
    } finally {
      setBusy(null);
    }
  };

  return (
    <div className="a2a-part">
      <h3 className="a2a-heading">Callers</h3>
      <p className="hint">Each program that talks to Pawl gets its own token, so you can see what it did and stop it on its own.</p>
      {a2a.callers.length === 0 ? (
        <p className="hint">No callers yet.</p>
      ) : (
        <ul className="a2a-list">
          {a2a.callers.map((c) => {
            // The assistant's own caller (Assistant mode): made and kept by the hub, shown read-only.
            if (isAssistantCaller(c)) {
              return (
                <li key={c.id} className="a2a-item is-connect is-assistant">
                  <div className="a2a-item-head">
                    <span className="a2a-item-title">
                      <strong>{c.name}</strong> <span className="mono muted">{c.id}</span>
                    </span>
                    <span className="chip">Used by the assistant</span>
                  </div>
                  <p className="hint a2a-connect-note">{ASSISTANT_CALLER_HINT}</p>
                </li>
              );
            }
            // A caller Connect made for a coding agent: never answers approvals; its token lives in the agent's config,
            // so it is renewed with Connect's Update and removed with the entry (Connect's Remove).
            const target = callerConnectTarget(c);
            const row = target ? connect.rows?.find((r) => r.target === target) : undefined;
            const anyBusy = busy !== null || connect.busy !== null;
            return (
              <li key={c.id} className={`a2a-item${target ? " is-connect" : ""}`}>
                <div className="a2a-item-head">
                  <span className="a2a-item-title">
                    <strong>{c.name}</strong> <span className="mono muted">{c.id}</span>
                  </span>
                  <span className={`chip ${c.tokenSet ? "chip-good" : "chip-warn"}`}>{c.tokenSet ? "Token set" : "No token yet"}</span>
                </div>
                {target ? (
                  <p className="hint a2a-connect-note">{connectCallerHint(target, row?.name)}</p>
                ) : (
                  <Toggle
                    label="Can answer approvals"
                    hint="Allow once or deny, only in sessions it started. Never “always”, never running commands, deleting files or network access."
                    checked={c.allowApprovals}
                    disabled={!live || anyBusy}
                    onChange={() => void setApprovals(c)}
                  />
                )}
                {fresh?.callerId === c.id && (
                  <div className="a2a-token" role="alert">
                    <strong>Copy this token now: Pawl never shows it again.</strong>
                    <span className="hint">Paste it into {c.name}'s A2A settings as a bearer token. Anyone who has it can use Pawl as {c.name}.</span>
                    <CopyField label={`Token for ${c.name}`} text={fresh.token} primary />
                    <div className="form-actions">
                      <button type="button" className="pill" onClick={() => setFresh(null)}>
                        Done, I copied it
                      </button>
                    </div>
                  </div>
                )}
                {rowError?.callerId === c.id && <span className="field-error">{rowError.text}</span>}
                {target && connect.rowError[target] && <span className="field-error">{connect.rowError[target]}</span>}
                <div className="form-actions">
                  {!target && (
                    <button type="button" className="pill" disabled={!live || anyBusy} onClick={() => void makeToken(c, c.tokenSet)}>
                      {busy === c.id ? "…" : c.tokenSet ? "Make new token" : "Create token"}
                    </button>
                  )}
                  <button
                    type="button"
                    className="pill pill-ghost"
                    disabled={!live || anyBusy}
                    onClick={() => void (target ? connect.remove({ ...(row ?? { target, name: c.name }), callerExists: true, callerMarked: true }) : remove(c))}
                  >
                    {target && connect.busy === target ? "…" : "Remove"}
                  </button>
                </div>
              </li>
            );
          })}
        </ul>
      )}
      <form
        className="a2a-add"
        onSubmit={(e) => {
          e.preventDefault();
          void add();
        }}
      >
        <label className="field-label" htmlFor="a2a-caller-name">
          Add a caller
        </label>
        <span className="a2a-row">
          <input
            id="a2a-caller-name"
            className={`input${addError ? " is-invalid" : ""}`}
            value={name}
            maxLength={80}
            placeholder="Program name, e.g. Claude Desktop"
            disabled={!live}
            onChange={(e) => {
              setName(e.target.value);
              setAddError(null);
            }}
          />
          <button type="submit" className="pill pill-primary" disabled={!live || busy !== null || !name.trim()}>
            {busy === "+" ? "Adding…" : "Add and create token"}
          </button>
        </span>
        {addError && <span className="field-error">{addError}</span>}
      </form>
    </div>
  );
}

// ---------------------------------------------------------------- outside agents (Pawl hands work to them)

function AgentsPart({ a2a, live }: { a2a: A2aSettings; live: boolean }) {
  const { client, state } = usePanel();
  const save = useSaveSettings();
  const confirm = useConfirm();
  const agents = a2a.agents ?? [];

  // The hub reads an agent's card after the save and announces the agent list, not the settings: read the settings
  // again when an outside agent appears or becomes usable, and every 1.5 s (up to 30 s) while a card is still on its way.
  const agentsKey = state.agents
    .filter((a) => a.source === "a2a")
    .map((a) => `${a.id}:${a.installed}`)
    .join(",");
  const waiting = agents
    .filter((a) => !a.card && !a.error)
    .map((a) => a.id)
    .join(",");
  useEffect(() => {
    if (live) client.getSettings().catch(() => {});
  }, [agentsKey, live, client]);
  useEffect(() => {
    if (!waiting || !live) return;
    let n = 0;
    const t = setInterval(() => {
      if (++n > 20) clearInterval(t);
      else client.getSettings().catch(() => {});
    }, 1500);
    return () => clearInterval(t);
  }, [waiting, live, client]);
  const [url, setUrl] = useState("");
  const [name, setName] = useState("");
  const [token, setToken] = useState("");
  const [errors, setErrors] = useState<{ url?: string; name?: string; token?: string; form?: string }>({});
  const [busy, setBusy] = useState(false);

  const add = async () => {
    const e: typeof errors = {};
    const u = checkA2aAgentUrl(url);
    if (u) e.url = u;
    if (name.trim()) {
      const n = checkA2aName(name);
      if (n) e.name = n;
    }
    if (token.trim()) {
      const t = checkA2aToken(token);
      if (t) e.token = t;
    }
    if (agents.length >= A2A_AGENTS_MAX) e.form = `At most ${A2A_AGENTS_MAX} outside agents.`;
    setErrors(e);
    if (Object.keys(e).length) return;
    setBusy(true);
    try {
      const { patch } = addAgentPatch(a2a, { url, name, token });
      const out = await save(patch, { success: "Added. Pawl is reading its agent card…" });
      const err = outcomeError(out);
      if (err) setErrors({ form: err });
      else if (out.ok) {
        setUrl("");
        setName("");
        setToken("");
      }
    } finally {
      setBusy(false);
    }
  };

  const remove = async (a: A2aAgentSetting) => {
    const ok = await confirm.ask({
      title: `Remove ${a.card?.name || a.name || a.id}?`,
      body: "It disappears from the agent list and its token is forgotten. Its sessions stay in the history.",
      confirmLabel: "Remove agent",
      tone: "danger",
    });
    if (!ok) return;
    const out = await save(removeAgentPatch(a2a, a.id), { success: "Removed" });
    const err = outcomeError(out);
    if (err) setErrors({ form: err });
  };

  return (
    <div className="a2a-part">
      <h3 className="a2a-heading">Outside agents</h3>
      <p className="hint">
        A2A agents that run elsewhere (Gemini CLI's A2A server, a cloud agent). They show up next to your other agents in New session and in the pet's
        switcher. Their questions become approvals that Pawl never answers for you.
      </p>
      {agents.length === 0 ? (
        <p className="hint">No outside agents yet.</p>
      ) : (
        <ul className="a2a-list">
          {agents.map((a) => (
            <AgentRow key={a.id} a2a={a2a} agent={a} live={live} onRemove={() => void remove(a)} />
          ))}
        </ul>
      )}
      <form
        className="a2a-add a2a-add-agent"
        onSubmit={(e) => {
          e.preventDefault();
          void add();
        }}
      >
        <span className="field-label">Add an outside agent</span>
        <label className="field">
          <span className="field-label-sub">URL</span>
          <input
            className={`input mono${errors.url ? " is-invalid" : ""}`}
            value={url}
            placeholder="https://agents.example.com/a2a"
            spellCheck={false}
            disabled={!live}
            onChange={(e) => {
              setUrl(e.target.value);
              setErrors({});
            }}
          />
          {errors.url ? (
            <span className="field-error">{errors.url}</span>
          ) : url.trim() && !checkA2aAgentUrl(url) ? (
            <span className="hint">{isLoopbackUrl(url) ? "On this PC: prompts stay here." : `Prompts you give it go to ${urlHost(url)}.`}</span>
          ) : null}
        </label>
        <span className="a2a-row">
          <label className="field">
            <span className="field-label-sub">Name (optional)</span>
            <input className={`input${errors.name ? " is-invalid" : ""}`} value={name} maxLength={80} placeholder="From its card" disabled={!live} onChange={(e) => setName(e.target.value)} />
            {errors.name && <span className="field-error">{errors.name}</span>}
          </label>
          <label className="field">
            <span className="field-label-sub">Token (optional, never shown again)</span>
            <input
              className={`input mono${errors.token ? " is-invalid" : ""}`}
              type="password"
              autoComplete="off"
              value={token}
              disabled={!live}
              onChange={(e) => setToken(e.target.value)}
            />
            {errors.token && <span className="field-error">{errors.token}</span>}
          </label>
        </span>
        {errors.form && <span className="field-error">{errors.form}</span>}
        <div className="form-actions">
          <button type="submit" className="pill pill-primary" disabled={!live || busy || !url.trim()}>
            {busy ? "Adding…" : "Add agent"}
          </button>
        </div>
      </form>
    </div>
  );
}

function AgentRow({ a2a, agent, live, onRemove }: { a2a: A2aSettings; agent: A2aAgentSetting; live: boolean; onRemove(): void }) {
  const { now } = usePanel();
  const save = useSaveSettings();
  const confirm = useConfirm();
  const [editing, setEditing] = useState(false);
  const [token, setToken] = useState("");
  const [error, setError] = useState<string | null>(null);
  const card = agent.card;
  const title = agent.name || card?.name || agent.id;

  const saveToken = async (value: string | null) => {
    if (value !== null) {
      const t = checkA2aToken(value);
      if (t) {
        setError(t);
        return;
      }
    } else {
      const ok = await confirm.ask({
        title: `Clear ${title}'s token?`,
        body: "Pawl calls it without a token from now on; an agent that needs one refuses.",
        confirmLabel: "Clear token",
        tone: "danger",
      });
      if (!ok) return;
    }
    const out = await save(agentTokenPatch(a2a, agent.id, value), { success: value === null ? "Token cleared" : "Token saved" });
    const err = outcomeError(out);
    setError(err);
    if (out.ok) {
      setEditing(false);
      setToken("");
    }
  };

  const skills = card?.skills ?? [];
  return (
    <li className="a2a-item">
      <div className="a2a-item-head">
        <span className="a2a-item-title">
          <strong>{title}</strong>
          {card && agent.name && card.name && card.name !== agent.name && <span className="muted"> ({card.name})</span>}
        </span>
        <span className={`chip ${agent.tokenSet ? "chip-good" : "chip-soft"}`}>{agent.tokenSet ? "Token set" : "No token"}</span>
      </div>
      <span className="mono a2a-url" title={agent.url}>
        {agent.url}
      </span>
      {card ? (
        <div className="a2a-card">
          {card.description && <p className="a2a-card-desc">{card.description}</p>}
          <div className="a2a-chips">
            <span className="chip chip-soft">A2A {card.protocolVersion}</span>
            <span className="chip chip-soft">{card.streaming ? "Streaming" : "No streaming"}</span>
            {card.version && <span className="chip chip-soft">v{card.version}</span>}
            <span className={`chip ${card.signature === "unverified" ? "chip-warn" : "chip-soft"}`} title="Pawl does not check agent card signatures yet.">
              {card.signature === "unverified" ? "Signature unverified" : "Unsigned card"}
            </span>
          </div>
          {skills.length > 0 && (
            <div className="a2a-skills" aria-label="Skills">
              {skills.slice(0, SKILLS_SHOWN).map((s) => (
                <span key={s.id} className="a2a-skill" title={s.description || undefined}>
                  {s.name || s.id}
                </span>
              ))}
              {skills.length > SKILLS_SHOWN && <span className="muted">+{skills.length - SKILLS_SHOWN} more</span>}
            </div>
          )}
          <span className="hint">Card read {timeAgo(card.fetchedAt, now)}.</span>
        </div>
      ) : !agent.error ? (
        <span className="hint">Reading its agent card…</span>
      ) : null}
      {agent.error && <span className="field-error">Could not use it: {agent.error}</span>}
      {editing && (
        <span className="a2a-row">
          <input
            className={`input mono${error ? " is-invalid" : ""}`}
            type="password"
            autoComplete="off"
            autoFocus
            value={token}
            placeholder="Paste the token"
            onChange={(e) => {
              setToken(e.target.value);
              setError(null);
            }}
          />
          <button type="button" className="pill pill-primary" disabled={!token.trim()} onClick={() => void saveToken(token)}>
            Save token
          </button>
          <button
            type="button"
            className="pill pill-ghost"
            onClick={() => {
              setEditing(false);
              setToken("");
              setError(null);
            }}
          >
            Cancel
          </button>
        </span>
      )}
      {error && <span className="field-error">{error}</span>}
      <div className="form-actions">
        {!editing && (
          <button type="button" className="pill" disabled={!live} onClick={() => setEditing(true)}>
            {agent.tokenSet ? "Replace token" : "Add token"}
          </button>
        )}
        {agent.tokenSet && !editing && (
          <button type="button" className="pill pill-ghost" disabled={!live} onClick={() => void saveToken(null)}>
            Clear token
          </button>
        )}
        <button type="button" className="pill pill-ghost" disabled={!live} onClick={onRemove}>
          Remove
        </button>
      </div>
    </li>
  );
}
