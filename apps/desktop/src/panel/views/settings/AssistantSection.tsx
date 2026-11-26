// Settings > Assistant (docs/CONTRACTS.md "v0.2 Assistant mode"): the default pet does the work with a real coding
// agent (Claude Code, Codex, Copilot CLI or any installed ACP agent) in its own workspace folder. Its changes, commands,
// web use and new sessions are approval cards under the user's rules (it may read files in its folder without asking),
// and it can never answer approvals. On/off, the agent, the workspace (pick, open, reset), Pawl's session tools, the
// browser, the status and "New chat".
import { useState } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import {
  assistantActivity,
  assistantAgentChoices,
  assistantAgentName,
  assistantAgentNote,
  assistantAgentProblem,
  assistantIntro,
  assistantOf,
  assistantPatch,
  assistantStatus,
  browserOf,
  isDefaultWorkspace,
  settingsErrorText,
  tildePath,
} from "@pawl/hub-client";
import type { HubSettingsAssistantPatch } from "@pawl/protocol";
import { isTauri } from "../../../lib/native";
import { openFolder } from "../../../lib/openFolder";
import { usePanel } from "../../context";
import { Field, SettingsSection, Toggle, useHomeFolder, useSaveSettings, type SaveOutcome } from "./shared";
import type { SectionProps } from "./sections";

const EMPTY: never[] = [];

function scrollToBrowser(): void {
  document.getElementById("settings-browser")?.scrollIntoView({ block: "start", behavior: "smooth" });
}

export function AssistantSection({ settings, live }: SectionProps) {
  const { client, state, toast } = usePanel();
  const save = useSaveSettings();
  const home = useHomeFolder();
  const a = assistantOf(settings);
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<{ field: string | null; text: string } | null>(null);
  const [typed, setTyped] = useState<string | null>(null);

  if (!a) {
    return (
      <SettingsSection id="assistant" title="Assistant" description="Let your pet do the work with a coding agent, in a folder of its own.">
        <p className="hint">This hub does not have the assistant yet. Update pawl on this machine and restart the hub.</p>
      </SettingsSection>
    );
  }

  const pet = settings.bot.petName?.trim() || "Your pet";
  const agentName = assistantAgentName(a, state.agents);
  const where = tildePath(a.workspace, home);
  const status = assistantStatus(a);
  const choices = assistantAgentChoices(a, state.agents, state.machine?.id);
  const agentProblem = assistantAgentProblem(a, state.agents, state.machine?.id);
  const agentNote = assistantAgentNote(a);
  const browser = browserOf(settings);
  const browserUsable = !!browser?.enabled;
  const session = a.sessionId ? state.sessions[a.sessionId] : undefined;
  const pendingHere = a.sessionId ? Object.values(state.pending).filter((p) => p.sessionId === a.sessionId).length : 0;
  const activity = a.state === "working" && a.sessionId ? assistantActivity(state.events[a.sessionId] ?? EMPTY, { pending: pendingHere, session }) : null;
  const workspaceDraft = typed ?? a.workspace;
  const workspaceDirty = typed !== null && typed.trim() !== a.workspace;

  const run = async (key: string, next: HubSettingsAssistantPatch, opts?: { success?: string }): Promise<boolean> => {
    setBusy(key);
    setError(null);
    try {
      const out: SaveOutcome = await save(assistantPatch(a, next), opts);
      if (out.ok) return true;
      if (!out.cancelled) setError({ field: out.fieldError?.field ?? null, text: out.fieldError ? out.fieldError.reason : out.message });
      return false;
    } finally {
      setBusy(null);
    }
  };

  const saveWorkspace = async (path: string) => {
    const p = path.trim();
    if (!p) return;
    if (await run("workspace", { workspace: p })) setTyped(null);
  };

  const pick = async () => {
    try {
      const picked = await open({ directory: true, multiple: false, title: `A folder for ${pet} to work in`, defaultPath: a.workspace || undefined });
      if (typeof picked === "string") await saveWorkspace(picked);
    } catch {
      /* dialog closed or unavailable */
    }
  };

  const newChat = async () => {
    setBusy("reset");
    setError(null);
    try {
      await client.assistantReset();
      toast(`New chat: ${pet} starts fresh`);
    } catch (err) {
      setError({ field: null, text: settingsErrorText(err) });
    } finally {
      setBusy(null);
    }
  };

  const anyBusy = busy !== null;
  const fieldError = (f: string) => (error && error.field === f ? error.text : null);
  const workspaceError = fieldError("assistant.workspace");
  const agentError = fieldError("assistant.agent") ?? agentProblem;

  return (
    <SettingsSection
      id="assistant"
      title="Assistant"
      description={assistantIntro(pet, agentName, where)}
      aside={
        <span className={`status-pill tone-${status.tone} assistant-status-pill`} aria-label={`Assistant: ${status.label}`}>
          <span className={`dot tone-${status.tone}`} /> {status.label}
        </span>
      }
    >
      <Toggle
        label={`Let ${pet} do the work`}
        hint={
          a.enabled
            ? `${pet}'s chat goes to ${agentName ?? "a coding agent"}. It may read files in its workspace without asking.`
            : `Off: ${pet} only answers and suggests what to do, as before.`
        }
        checked={a.enabled}
        disabled={!live || anyBusy}
        onChange={() => void run("enabled", { enabled: !a.enabled })}
      />

      <div className="assistant-status" role="status">
        <span className="assistant-status-text">
          {status.error ? (
            <span className="field-error">{status.error}</span>
          ) : a.enabled ? (
            <>
              <strong>{status.label}</strong>
              {activity ? <span className="muted"> · {activity}</span> : status.label === "Ready" ? <span className="muted"> · waiting for your next message</span> : null}
            </>
          ) : (
            <span className="muted">The assistant is off.</span>
          )}
        </span>
        <button
          type="button"
          className="pill"
          disabled={!live || anyBusy || !a.enabled}
          title={`End ${pet}'s current work session: your next message starts a fresh one. The chat history stays.`}
          onClick={() => void newChat()}
        >
          {busy === "reset" ? "…" : "New chat"}
        </button>
      </div>

      <Field
        label="Agent"
        hint="The coding agent that does the work. Automatic uses Claude Code, else Codex, else Copilot CLI, else the first one installed."
        error={agentError}
      >
        {(f) => (
          <select
            id={f.id}
            className={`input select${f.invalid ? " is-invalid" : ""}`}
            aria-describedby={f.describedBy}
            value={a.agent}
            disabled={!live || anyBusy}
            onChange={(e) => void run("agent", { agent: e.target.value })}
          >
            {choices.map((c) => (
              <option key={c.value} value={c.value}>
                {c.label}
              </option>
            ))}
          </select>
        )}
      </Field>
      {agentNote && (
        // The hub's caution for agents other than Claude Code (Pawl can't lock their own approval settings).
        <span className="field-warning assistant-agent-note" role="note">
          {agentNote}
        </span>
      )}

      <Field
        wide
        label="Workspace"
        hint={`${pet}'s own folder: it makes a subfolder for each job. Pawl creates it the first time and never deletes it.`}
        error={workspaceError}
      >
        {(f) => (
          <span className="input-row assistant-workspace">
            <input
              id={f.id}
              className={`input mono${f.invalid ? " is-invalid" : ""}`}
              aria-describedby={f.describedBy}
              value={workspaceDraft}
              spellCheck={false}
              disabled={!live}
              onChange={(e) => {
                setTyped(e.target.value);
                if (error?.field === "assistant.workspace") setError(null);
              }}
              onKeyDown={(e) => {
                if (e.key === "Enter") {
                  e.preventDefault();
                  void saveWorkspace(workspaceDraft);
                } else if (e.key === "Escape" && typed !== null) {
                  e.preventDefault();
                  setTyped(null);
                }
              }}
            />
            {workspaceDirty && (
              <button type="button" className="pill pill-primary" disabled={!live || anyBusy || !workspaceDraft.trim()} onClick={() => void saveWorkspace(workspaceDraft)}>
                {busy === "workspace" ? "Saving…" : "Save"}
              </button>
            )}
            {isTauri && (
              <button type="button" className="pill" disabled={!live || anyBusy} onClick={() => void pick()}>
                Browse…
              </button>
            )}
            <button
              type="button"
              className="pill"
              disabled={!a.workspace}
              onClick={() =>
                void openFolder(a.workspace, { missing: `${where} doesn't exist yet: Pawl creates it the first time ${pet} works.` }).then((msg) => msg && toast(msg))
              }
            >
              Open folder
            </button>
            <button
              type="button"
              className="pill pill-ghost"
              disabled={!live || anyBusy || (typed === null && isDefaultWorkspace(a.workspace, home))}
              title="Back to the default folder, Pawl in your home folder"
              onClick={() => void run("workspace", { workspace: "" }, { success: "Workspace reset to ~/Pawl" }).then((ok) => ok && setTyped(null))}
            >
              Reset to default
            </button>
          </span>
        )}
      </Field>

      <Toggle
        label="Can start and steer other sessions"
        hint="Pawl's own tools: see your agents, start coding agents in your repos and in its workspace, prompt them, check on them, stop them. Each start asks you first."
        checked={a.sessions}
        disabled={!live || anyBusy}
        onChange={() => void run("sessions", { sessions: !a.sessions })}
      />
      <Toggle
        label="Can use the browser"
        hint={
          browserUsable
            ? "Your own Chrome, through browser use. Every click and every typing asks you first."
            : "Browser use is off, so this stays off until you turn it on."
        }
        checked={a.browser && browserUsable}
        disabled={!live || anyBusy || !browserUsable}
        onChange={() => void run("browser", { browser: !a.browser })}
      />
      {!browserUsable && (
        // Outside the switch's label: a click here must not reach the switch.
        <p className="hint assistant-browser-link">
          <button type="button" className="link" onClick={scrollToBrowser}>
            Turn on browser use in Settings &gt; Browser
          </button>
        </p>
      )}
      {error && !error.field && (
        <span className="field-error" role="alert">
          {error.text}
        </span>
      )}
      {error && error.field && error.field !== "assistant.workspace" && error.field !== "assistant.agent" && (
        <span className="field-error" role="alert">
          {error.text}
        </span>
      )}
    </SettingsSection>
  );
}
