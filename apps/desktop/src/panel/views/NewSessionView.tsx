import { useEffect, useMemo, useState } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import type { Agent, MachineId, RepoRef } from "@krypto/protocol";
import { errorText, shortPath } from "../../lib/format";
import {
  OS_LABEL,
  intentErrorText,
  isLocal,
  machineDisplayName,
  petFor,
  sortedMachines,
  startNeedsConfirmation,
} from "@krypto/hub-client";
import { isTauri } from "../../lib/native";
import { useSettings } from "../../lib/settings";
import { usePanel } from "../context";
import * as I from "../icons";
import { PetAvatar } from "../PetAvatar";

/** A machine's repo index (`list_repos{machineId}`): the folders it runs agents in, newest first. */
interface MachineRepos {
  machineId: MachineId;
  repos: RepoRef[];
  loading: boolean;
  error: string | null;
}

/** Repos shown under the folder field at once. */
const REPO_PICKS = 12;

/** Another machine's agents, as last reported in its presence (`list_agents{machineId}`). */
interface RemoteAgents {
  machineId: MachineId;
  agents: Agent[];
  loading: boolean;
  error: string | null;
}

export function NewSessionView() {
  const { client, state, navigate, toast, meta } = usePanel();
  const [settings, setSettings] = useSettings();
  const own = state.machine?.id ?? "";
  // This machine first, then the paired machines that are online right now.
  const machines = useMemo(() => sortedMachines(state).filter((m) => m.id === own || m.online), [state, own]);
  const [machineId, setMachineId] = useState<MachineId>(own);
  const target = machineId || own;
  const remote = startNeedsConfirmation(state, target);
  const targetName = machineDisplayName(state.machines[target] ?? state.machine);

  const [remoteAgents, setRemoteAgents] = useState<RemoteAgents | null>(null);
  const localAgents = useMemo(() => state.agents.filter((a) => isLocal(state, a.machineId)), [state]);
  const agents = useMemo(() => {
    const list = remote ? (remoteAgents?.machineId === target ? remoteAgents.agents : []) : localAgents;
    return [...list].sort((a, b) => Number(b.installed) - Number(a.installed) || a.name.localeCompare(b.name));
  }, [remote, remoteAgents, target, localAgents]);

  const [agentId, setAgentId] = useState(() => settings.lastAgentId);
  const [cwd, setCwd] = useState(() => settings.lastCwd);
  const [prompt, setPrompt] = useState("");
  const [busy, setBusy] = useState(false);
  const [refreshing, setRefreshing] = useState(false);
  /** Remote start: the confirm step is showing. */
  const [confirming, setConfirming] = useState(false);
  const [repoIndex, setRepoIndex] = useState<MachineRepos | null>(null);
  const repos = repoIndex?.machineId === target ? repoIndex.repos : [];
  // Repos matching what is typed (by name or path), newest first.
  const repoPicks = useMemo(() => {
    const q = cwd.trim().toLowerCase();
    const hit = (r: RepoRef) => !q || r.name.toLowerCase().includes(q) || r.path.toLowerCase().includes(q);
    const exact = repos.find((r) => r.path === cwd.trim());
    return exact ? [] : repos.filter(hit).slice(0, REPO_PICKS);
  }, [repos, cwd]);

  // The chosen machine's repo index (this machine's too): the folder picker.
  useEffect(() => {
    if (meta.phase !== "connected") return;
    let alive = true;
    setRepoIndex({ machineId: target, repos: [], loading: true, error: null });
    client
      .listRepos({ machineId: target })
      .then((list) => {
        if (!alive) return;
        setRepoIndex({ machineId: target, repos: list, loading: false, error: null });
        // A remote machine starts only in folders it already uses: preselect its newest.
        if (remote) setCwd((c) => c || list[0]?.path || "");
      })
      .catch((err) => alive && setRepoIndex({ machineId: target, repos: [], loading: false, error: remote ? intentErrorText(err, targetName, "start") : errorText(err) }));
    return () => {
      alive = false;
    };
    // targetName / remote follow target.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [client, target, meta.phase]);

  // The machine went offline or was unpaired: back to this one.
  useEffect(() => {
    if (machineId && machineId !== own && !machines.some((m) => m.id === machineId)) setMachineId(own);
  }, [machines, machineId, own]);

  // Another machine's agents come from its latest presence.
  useEffect(() => {
    setConfirming(false);
    if (!remote || meta.phase !== "connected") return;
    let alive = true;
    setRemoteAgents({ machineId: target, agents: [], loading: true, error: null });
    client
      .listAgents({ machineId: target })
      .then((list) => alive && setRemoteAgents({ machineId: target, agents: list, loading: false, error: null }))
      .catch((err) => alive && setRemoteAgents({ machineId: target, agents: [], loading: false, error: intentErrorText(err, targetName, "start") }));
    return () => {
      alive = false;
    };
    // targetName is display only; re-fetch on machine change / reconnect.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [client, target, remote, meta.phase]);

  // Switching machines: suggest that machine's most recent folder (this machine keeps the last one used).
  const pickMachine = (id: MachineId) => {
    setMachineId(id);
    setConfirming(false);
    // This machine keeps the last folder used; another machine gets its newest repo once its index loads.
    setCwd(id === own ? settings.lastCwd : "");
  };

  const selected: Agent | undefined =
    agents.find((a) => a.id === agentId && a.installed) ?? agents.find((a) => a.installed);
  const pet = selected ? petFor(Object.values(state.pets), target, selected.id) : undefined;

  const browse = async () => {
    if (!isTauri) {
      toast("Folder picker needs the desktop app; type a path instead.");
      return;
    }
    try {
      const picked = await open({ directory: true, multiple: false, defaultPath: cwd || undefined, title: "Where should the agent work?" });
      if (typeof picked === "string") setCwd(picked);
    } catch (err) {
      toast(errorText(err), "error");
    }
  };

  const refresh = async () => {
    setRefreshing(true);
    try {
      if (remote) {
        const list = await client.listAgents({ machineId: target });
        setRemoteAgents({ machineId: target, agents: list, loading: false, error: null });
      } else {
        await client.listAgents(true);
      }
    } catch (err) {
      toast(remote ? intentErrorText(err, targetName, "start") : errorText(err), "error");
    } finally {
      setRefreshing(false);
    }
  };

  const start = async (confirmed: boolean) => {
    if (!selected || !cwd.trim() || busy) return;
    // Starting on another machine runs code there: confirm on this device first.
    if (remote && !confirmed) {
      setConfirming(true);
      return;
    }
    setBusy(true);
    try {
      const session = await client.startSession({
        agentId: selected.id,
        cwd: cwd.trim(),
        prompt: prompt.trim() || undefined,
        machineId: remote ? target : undefined,
        confirmed: remote ? true : undefined,
      });
      if (!remote) setSettings({ lastAgentId: selected.id, lastCwd: cwd.trim() });
      toast(remote ? `${pet?.name ?? "The pet"} is on it on ${targetName}!` : `${pet?.name ?? "Your pet"} is on it!`);
      navigate({ view: "session", sessionId: session.id });
    } catch (err) {
      toast(remote ? intentErrorText(err, targetName, "start") : `Could not start: ${errorText(err)}`, "error");
      setConfirming(false);
    } finally {
      setBusy(false);
    }
  };

  const disabled = meta.phase !== "connected";
  const loadingAgents = remote && (remoteAgents?.machineId !== target || remoteAgents.loading);

  return (
    <section className="view">
      <header className="view-head">
        <h1>New session</h1>
        <p>Pick a machine and an agent, point it at a folder and give it a job. Its pet will walk the strip while it works.</p>
      </header>

      <form
        className="new-form"
        onSubmit={(e) => {
          e.preventDefault();
          void start(false);
        }}
      >
        {machines.length > 1 && (
          <div className="field">
            <div className="field-label">Machine</div>
            <div className="machine-picker" role="radiogroup" aria-label="Machine">
              {machines.map((m) => {
                const isSel = m.id === target;
                return (
                  <button
                    type="button"
                    key={m.id}
                    role="radio"
                    aria-checked={isSel}
                    className={`machine-choice${isSel ? " is-selected" : ""}`}
                    onClick={() => pickMachine(m.id)}
                  >
                    <I.Monitor size={14} />
                    <span>
                      <strong>{machineDisplayName(m)}</strong>
                      <span className="muted">
                        {" "}
                        {m.id === own ? "this machine" : (OS_LABEL[m.os] ?? m.os)}
                      </span>
                    </span>
                  </button>
                );
              })}
            </div>
          </div>
        )}

        <div className="field">
          <div className="field-label">
            Agent{remote ? ` on ${targetName}` : ""}
            <button type="button" className="link" onClick={() => void refresh()} disabled={refreshing || disabled}>
              {refreshing ? "Sniffing around…" : remote ? "Reload" : "Rescan agents"}
            </button>
          </div>
          {loadingAgents ? (
            <div className="hint">Asking {targetName} for its agents…</div>
          ) : remote && remoteAgents?.error ? (
            <div className="field-error">{remoteAgents.error}</div>
          ) : agents.length === 0 ? (
            <div className="hint">
              No agents {remote ? `reported by ${targetName}` : "discovered yet"}
              {disabled ? " (hub offline)" : ""}.
            </div>
          ) : (
            <div className="agent-grid">
              {agents.map((a) => {
                const p = petFor(Object.values(state.pets), target, a.id);
                const isSel = selected?.id === a.id;
                return (
                  <button
                    type="button"
                    key={a.id}
                    className={`agent-card${isSel ? " is-selected" : ""}${a.installed ? "" : " is-missing"}`}
                    disabled={!a.installed}
                    onClick={() => {
                      setAgentId(a.id);
                      setConfirming(false);
                    }}
                    title={a.installed ? a.command.join(" ") : "Not installed"}
                  >
                    <PetAvatar species={p?.species ?? "dog"} activity={isSel ? "happy" : "idle"} size={44} />
                    <span className="agent-card-text">
                      <strong>{a.name}</strong>
                      <span>
                        {p ? `${p.name} · ` : ""}
                        {a.installed ? (a.description ?? a.version ?? a.id) : "Not installed"}
                      </span>
                    </span>
                  </button>
                );
              })}
            </div>
          )}
        </div>

        <div className="field">
          <label className="field-label" htmlFor="new-session-cwd">
            Working folder{remote ? ` on ${targetName}` : ""}
          </label>
          <div className="input-row">
            <input
              id="new-session-cwd"
              className="input mono"
              value={cwd}
              list="new-session-cwds"
              placeholder={
                remote
                  ? `Only folders ${targetName} already uses`
                  : navigator.userAgent.includes("Windows")
                    ? "C:\\code\\my-project"
                    : "~/code/my-project"
              }
              onChange={(e) => {
                setCwd(e.target.value);
                setConfirming(false);
              }}
              spellCheck={false}
            />
            {!remote && (
              <button type="button" className="pill" onClick={() => void browse()}>
                <I.Folder size={14} /> Browse…
              </button>
            )}
          </div>
          <datalist id="new-session-cwds">
            {repos.map((r) => (
              <option key={r.path} value={r.path} label={r.name} />
            ))}
          </datalist>
          {repoPicks.length > 0 && (
            <div className="repo-picker" role="listbox" aria-label={`Repos on ${targetName}`}>
              {repoPicks.map((r) => (
                <button
                  type="button"
                  key={r.path}
                  role="option"
                  aria-selected={r.path === cwd.trim()}
                  className="repo-choice"
                  title={r.path}
                  onClick={() => {
                    setCwd(r.path);
                    setConfirming(false);
                  }}
                >
                  <strong>{r.name}</strong>
                  <span className="mono">{shortPath(r.path, 3)}</span>
                </button>
              ))}
            </div>
          )}
          {repoIndex?.machineId === target && repoIndex.loading && <span className="hint">Loading the repos {targetName} uses…</span>}
          {repoIndex?.machineId === target && repoIndex.error && <span className="field-error">{repoIndex.error}</span>}
          {cwd && (
            <span className="hint">
              The agent can read and write files inside {shortPath(cwd, 3)}
              {remote ? ` on ${targetName}` : ""}.
            </span>
          )}
          {remote && (
            <span className="hint">
              Only folders {targetName} already uses
              {repos.length > 0 ? " (pick one of its repos)" : `: start a session on ${targetName} first to add one`}.
            </span>
          )}
        </div>

        <label className="field">
          <span className="field-label">First prompt (optional)</span>
          <textarea
            className="input"
            rows={5}
            value={prompt}
            placeholder="Run the tests and fix whatever is failing."
            onChange={(e) => setPrompt(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) {
                e.preventDefault();
                void start(false);
              }
            }}
          />
        </label>

        {confirming && selected ? (
          <div className="start-confirm" role="group" aria-label={`Start on ${targetName}?`}>
            <span className="start-confirm-text">
              Start <strong>{selected.name}</strong> on <strong className="start-confirm-machine">{targetName}</strong> in{" "}
              <span className="mono">{cwd.trim()}</span>? It runs on {targetName}, with access to that folder there.
            </span>
            <span className="form-actions">
              <button type="button" className="pill pill-primary" autoFocus disabled={busy} onClick={() => void start(true)}>
                {busy ? "Starting…" : `Start on ${targetName}`}
              </button>
              <button type="button" className="pill pill-ghost" disabled={busy} onClick={() => setConfirming(false)}>
                Back
              </button>
            </span>
          </div>
        ) : (
          <div className="form-actions">
            <button className="pill pill-primary big" type="submit" disabled={disabled || busy || !selected || !cwd.trim()}>
              {busy ? "Starting…" : `Start with ${selected?.name ?? "agent"}${remote ? "…" : ""}`}
            </button>
            {remote ? <span className="chip chip-machine">Runs on {targetName}</span> : <span className="hint">Ctrl+Enter to start</span>}
          </div>
        )}
      </form>
    </section>
  );
}
