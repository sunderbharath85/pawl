import { useMemo, useState, type ReactNode } from "react";
import {
  OS_LABEL,
  groupByMachine,
  machineCounts,
  machineDisplayName,
  machineSummary,
  machinesLine,
  type MachineGroup,
} from "@pawl/hub-client";
import type { Session } from "@pawl/protocol";
import { STATUS_META, SOURCE_LABEL, agentName, isActive, sessionTitle, timeAgo } from "../lib/format";
import { usePanel, type Route } from "./context";
import * as I from "./icons";
import { ACTIVITY_LABEL, PetAvatar, activityUrgency, safePetLine } from "./PetAvatar";

export function Sidebar() {
  const { state, views, route, navigate, now, meta } = usePanel();
  const [showEnded, setShowEnded] = useState(false);
  const [heroIdx, setHeroIdx] = useState(0);

  const pendingCount = Object.keys(state.pending).length;

  // The most "urgent" home pet leads (visitors live on other machines); clicking the hero cycles through the pack.
  const ordered = useMemo(() => {
    const home = views.filter((v) => !v.visitor && !v.remoteOffline);
    return [...(home.length > 0 ? home : views)].sort(
      (a, b) => activityUrgency(b.activity) - activityUrgency(a.activity) || a.name.localeCompare(b.name),
    );
  }, [views]);
  const hero = ordered.length > 0 ? ordered[heroIdx % ordered.length] : undefined;

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
    { route: { view: "pets" }, label: "Pets", icon: <I.Bone /> },
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

      <button
        className={`hero tone-${hero?.activity ?? "napping"}`}
        onClick={() => setHeroIdx((i) => i + 1)}
        title={ordered.length > 1 ? "Next pet" : undefined}
      >
        <div className="hero-bed">
          <PetAvatar species={hero?.species ?? "dog"} activity={hero?.activity ?? "napping"} size={92} />
        </div>
        <div className="hero-text">
          <div className="hero-name">
            {hero?.name ?? "Biscuit"}
            {ordered.length > 1 && <span className="hero-count">+{ordered.length - 1}</span>}
          </div>
          <div className="hero-activity">{ACTIVITY_LABEL[hero?.activity ?? "napping"]}</div>
          <div className="hero-line">“{safePetLine(hero?.species ?? "dog", hero?.activity ?? "napping", hero ? hero.since : 0)}”</div>
          {hero && (
            <div className="mood" title={`Mood ${Math.round(hero.mood)}/100`}>
              <span style={{ width: `${Math.max(4, Math.min(100, hero.mood))}%` }} />
            </div>
          )}
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
    const title = sessionTitle(s, state.agents);
    return (
      <button
        key={s.id}
        title={title}
        className={`session-row${active ? " is-active" : ""}${s.endedAt ? " is-ended" : ""}`}
        onClick={() => navigate({ view: "session", sessionId: s.id })}
      >
        <span className={`dot tone-${meta.tone}`} title={meta.label} />
        <span className="session-row-main">
          <span className="session-row-title">{title}</span>
          <span className="session-row-sub">
            {agentName(state.agents, s.agentId, s.machineId)} · {SOURCE_LABEL[s.source]} · {timeAgo(s.lastEventAt, now)}
          </span>
        </span>
        {waiting > 0 && <span className="badge badge-alert">{waiting}</span>}
      </button>
    );
  }
}
