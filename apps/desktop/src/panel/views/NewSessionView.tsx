import { useMemo, useState } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import type { Agent } from "@krypto/protocol";
import { errorText, shortPath } from "../../lib/format";
import { isLocal, petFor } from "@krypto/hub-client";
import { isTauri } from "../../lib/native";
import { useSettings } from "../../lib/settings";
import { usePanel } from "../context";
import * as I from "../icons";
import { PetAvatar } from "../PetAvatar";

export function NewSessionView() {
  const { client, state, navigate, toast, meta } = usePanel();
  const [settings, setSettings] = useSettings();
  const agents = useMemo(
    // Sessions start on this machine's hub, so only its own agents are offered.
    () =>
      state.agents
        .filter((a) => isLocal(state, a.machineId))
        .sort((a, b) => Number(b.installed) - Number(a.installed) || a.name.localeCompare(b.name)),
    [state],
  );
  const [agentId, setAgentId] = useState(() => settings.lastAgentId);
  const [cwd, setCwd] = useState(() => settings.lastCwd);
  const [prompt, setPrompt] = useState("");
  const [busy, setBusy] = useState(false);
  const [refreshing, setRefreshing] = useState(false);

  const selected: Agent | undefined =
    agents.find((a) => a.id === agentId && a.installed) ?? agents.find((a) => a.installed);
  const pet = selected ? petFor(Object.values(state.pets), selected.machineId, selected.id) : undefined;

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
      await client.listAgents(true);
    } catch (err) {
      toast(errorText(err), "error");
    } finally {
      setRefreshing(false);
    }
  };

  const start = async () => {
    if (!selected || !cwd.trim() || busy) return;
    setBusy(true);
    try {
      const session = await client.startSession(selected.id, cwd.trim(), prompt.trim() || undefined);
      setSettings({ lastAgentId: selected.id, lastCwd: cwd.trim() });
      toast(`${pet?.name ?? "Your pet"} is on it!`);
      navigate({ view: "session", sessionId: session.id });
    } catch (err) {
      toast(`Could not start: ${errorText(err)}`, "error");
    } finally {
      setBusy(false);
    }
  };

  const disabled = meta.phase !== "connected";

  return (
    <section className="view">
      <header className="view-head">
        <h1>New session</h1>
        <p>Pick an agent, point it at a folder and give it a job. Its pet will walk the strip while it works.</p>
      </header>

      <form
        className="new-form"
        onSubmit={(e) => {
          e.preventDefault();
          void start();
        }}
      >
        <div className="field">
          <div className="field-label">
            Agent
            <button type="button" className="link" onClick={() => void refresh()} disabled={refreshing || disabled}>
              {refreshing ? "Sniffing around…" : "Rescan agents"}
            </button>
          </div>
          {agents.length === 0 ? (
            <div className="hint">No agents discovered yet{disabled ? " (hub offline)" : ""}.</div>
          ) : (
            <div className="agent-grid">
              {agents.map((a) => {
                const p = petFor(Object.values(state.pets), a.machineId, a.id);
                const isSel = selected?.id === a.id;
                return (
                  <button
                    type="button"
                    key={a.id}
                    className={`agent-card${isSel ? " is-selected" : ""}${a.installed ? "" : " is-missing"}`}
                    disabled={!a.installed}
                    onClick={() => setAgentId(a.id)}
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

        <label className="field">
          <span className="field-label">Working folder</span>
          <div className="input-row">
            <input
              className="input mono"
              value={cwd}
              placeholder={navigator.userAgent.includes("Windows") ? "C:\\code\\my-project" : "~/code/my-project"}
              onChange={(e) => setCwd(e.target.value)}
              spellCheck={false}
            />
            <button type="button" className="pill" onClick={() => void browse()}>
              <I.Folder size={14} /> Browse…
            </button>
          </div>
          {cwd && <span className="hint">The agent can read and write files inside {shortPath(cwd, 3)}.</span>}
        </label>

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
                void start();
              }
            }}
          />
        </label>

        <div className="form-actions">
          <button className="pill pill-primary big" type="submit" disabled={disabled || busy || !selected || !cwd.trim()}>
            {busy ? "Starting…" : `Start with ${selected?.name ?? "agent"}`}
          </button>
          <span className="hint">Ctrl+Enter to start</span>
        </div>
      </form>
    </section>
  );
}
