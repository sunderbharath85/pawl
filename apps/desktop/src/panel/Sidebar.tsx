import { useMemo, useState, type ReactNode } from "react";
import {
  OS_LABEL,
  groupByMachine,
  machineCounts,
  machineDisplayName,
  machineSummary,
  machinesLine,
  type MachineGroup,
  isAssistantSession,
  isBrowserSession,
} from "@pawl/hub-client";
import type { Session } from "@pawl/protocol";
import { STATUS_META, SOURCE_LABEL, agentName, isActive, sessionTitle, timeAgo } from "../lib/format";
import { usePanel, type Route } from "./context";
import * as I from "./icons";
import { ACTIVITY_LABEL, PetAvatar } from "./PetAvatar";
import { useDefaultPet } from "./useDefaultPet";

export function Sidebar() {
  const { state, route, navigate, now, meta } = usePanel();
  const [showEnded, setShowEnded] = useState(false);

  const pendingCount = Object.keys(state.pending).length;

  // One pet: the default pet, which orchestrates every session (agents' pets are names on their sessions).
  const pet = useDefaultPet();

  const sessions = useMemo(() => {
    const all = Object.values(state.sessions);
    const sorted = all.sort((a, b) => Number(isActive(b)) - Number(isActive(a)) || b.lastEventAt - a.lastEventAt);
    const open = sorted.filter((s) => !s.endedAt);
    const ended = sorted.filter((s) => !!s.endedAt);
    return {
      open,
      ended,
      openGroups: groupByMachine(state, open),
      endedGroups: groupByMachine(state, ended),
    };
  }, [state]);
  // Machine headings only once another machine's sessions are mirrored here.
  const grouped = sessions.openGroups.length + sessions.endedGroups.length > 0 &&
    new Set([...sessions.openGroups, ...sessions.endedGroups].map((g) => g.machineId)).size > 1;

  const nav: Array<{ route: Route; label: string; icon: ReactNode; badge?: number }> = [
    { route: { view: "home" }, label: "Home", icon: <I.Home /> },
    { route: { view: "new" }, label: "New session", icon: <I.Plus /> },
    { route: { view: "chat" }, label: "Chat", icon: <I.Chat /> },
    { route: { view: "approvals" }, label: "Approvals", icon: <I.Bell />, badge: pendingCount },
    { route: { view: "machines" }, label: "Machines", icon: <I.Machines /> },
    { route: { view: "rules" }, label: "Rules", icon: <I.Shield /> },
    { route: { view: "settings" }, label: "Settings", icon: <I.Gear /> },
  ];

  const machine = state.machine;
  const counts = machineCounts(state);
  const connected = meta.phase === "connected";

  return (
    <aside className="sidebar">
      <button
        className={`machine-tag${route.view === "settings" ? " is-active" : ""}`}
        onClick={() => navigate({ view: "settings" })}
        title={`${machineSummary(machine)}
Rename it in Settings`}
      >
        <I.Monitor size={15} />
        <span className="machine-tag-text">
          <span className="machine-tag-label">This machine</span>
          <span className="machine-tag-name">{machineDisplayName(machine)}</span>
        </span>
        {machine && <span className="machine-tag-os">{OS_LABEL[machine.os] ?? machine.os}</span>}
      </button>
      {counts.total > 0 && (
        <button
          className={`machines-line${route.view === "machines" ? " is-active" : ""}`}
          onClick={() => navigate({ view: "machines" })}
          title="Paired machines"
        >
          <span className={`dot${connected && counts.online > 1 ? " tone-done" : ""}`} aria-hidden />
          <span>{machinesLine(counts.total, connected ? counts.online : undefined)}</span>
          {counts.total === 1 && <span className="machines-line-cta">Pair</span>}
        </button>
      )}

      <button className={`hero tone-${pet.activity}`} onClick={() => navigate({ view: "chat" })} title={`Chat with ${pet.name}`}>
        <div className="hero-bed">
          <PetAvatar species={pet.species} activity={pet.activity} size={92} />
        </div>
        <div className="hero-text">
          <div className="hero-name">{pet.name}</div>
          <div className="hero-activity">{ACTIVITY_LABEL[pet.activity]}</div>
          <div className="hero-line">{pet.line}</div>
        </div>
      </button>

      <nav className="nav">
        {nav.map((n) => (
          <button
            key={n.route.view}
            className={`nav-item${route.view === n.route.view ? " is-active" : ""}`}
            onClick={() => navigate(n.route)}
          >
            <span className="nav-icon">{n.icon}</span>
            <span>{n.label}</span>
            {!!n.badge && <span className="badge badge-alert">{n.badge}</span>}
          </button>
        ))}
      </nav>

      <div className="side-section">
        <div className="side-heading">
          <I.Paw size={13} /> Sessions
          <span className="side-count">{sessions.open.length}</span>
        </div>
        <div className="session-list">
          {sessions.open.length === 0 && <div className="side-empty">No sessions yet. Start one and I'll keep watch.</div>}
          {sessions.openGroups.map((g) => renderGroup(g, "open"))}
          {sessions.ended.length > 0 && (
            <button className="side-toggle" onClick={() => setShowEnded((v) => !v)}>
              {showEnded ? "Hide" : "Show"} finished ({sessions.ended.length})
            </button>
          )}
          {showEnded && sessions.endedGroups.map((g) => renderGroup(g, "ended"))}
        </div>
      </div>
    </aside>
  );

  function renderGroup(g: MachineGroup<Session>, part: "open" | "ended") {
    const offline = !g.online;
    return (
      <div key={`${part}:${g.machineId}`} className={`session-group${offline ? " is-offline" : ""}`}>
        {grouped && (
          <div className="session-group-head" title={offline ? "Offline: showing what it last sent" : undefined}>
            <I.Monitor size={12} />
            <span className="session-group-name">{g.local ? `${machineDisplayName(g.machine)} (this machine)` : machineDisplayName(g.machine)}</span>
            {offline && <span className="session-group-seen">last seen {timeAgo(g.machine?.lastSeen ?? 0, now)}</span>}
          </div>
        )}
        {g.items.map((s) => renderRow(s))}
      </div>
    );
  }

  function renderRow(s: Session) {
    const active = route.view === "session" && route.sessionId === s.id;
    const meta = STATUS_META[s.status];
    const waiting = Object.values(state.pending).filter((p) => p.sessionId === s.id).length;
    // The assistant's session (Assistant mode) is the default pet itself at work: shown with its name.
    const assistant = isAssistantSession(s);
    const title = assistant ? pet.name : sessionTitle(s, state.agents);
    return (
      <button
        key={s.id}
        title={title}
        className={`session-row${active ? " is-active" : ""}${s.endedAt ? " is-ended" : ""}`}
        onClick={() => navigate({ view: "session", sessionId: s.id })}
      >
        <span className={`dot tone-${meta.tone}`} title={meta.label} />
        <span className="session-row-main">
          <span className="session-row-title">
            {assistant ? (
              <span className="chip chip-browser chip-assistant" title={`${pet.name}'s own work session (Settings > Assistant)`}>
                <I.Paw size={11} /> Assistant
              </span>
            ) : (
              isBrowserSession(s) && (
                <span className="chip chip-browser" title="A browser task: the pet drives your Chrome">
                  <I.Globe size={11} /> Browser
                </span>
              )
            )}
            {title}
          </span>
          <span className="session-row-sub">
            {agentName(state.agents, s.agentId, s.machineId)} · {SOURCE_LABEL[s.source]} · {timeAgo(s.lastEventAt, now)}
          </span>
        </span>
        {waiting > 0 && <span className="badge badge-alert">{waiting}</span>}
      </button>
    );
  }
}
