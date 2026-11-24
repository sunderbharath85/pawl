import { useMemo, useState, type ReactNode } from "react";
import type { Session } from "@krypto/protocol";
import { STATUS_META, SOURCE_LABEL, agentName, isActive, sessionTitle, timeAgo } from "../lib/format";
import { usePanel, type Route } from "./context";
import * as I from "./icons";
import { ACTIVITY_LABEL, PetAvatar, activityUrgency, safePetLine } from "./PetAvatar";

export function Sidebar() {
  const { state, views, route, navigate, now } = usePanel();
  const [showEnded, setShowEnded] = useState(false);
  const [heroIdx, setHeroIdx] = useState(0);

  const pendingCount = Object.keys(state.pending).length;

  // The most "urgent" pet leads; clicking the hero cycles through the pack.
  const ordered = useMemo(
    () => [...views].sort((a, b) => activityUrgency(b.activity) - activityUrgency(a.activity) || a.name.localeCompare(b.name)),
    [views],
  );
  const hero = ordered.length > 0 ? ordered[heroIdx % ordered.length] : undefined;

  const sessions = useMemo(() => {
    const all = Object.values(state.sessions);
    const sorted = all.sort((a, b) => Number(isActive(b)) - Number(isActive(a)) || b.lastEventAt - a.lastEventAt);
    return {
      open: sorted.filter((s) => !s.endedAt),
      ended: sorted.filter((s) => !!s.endedAt),
    };
  }, [state.sessions]);

  const nav: Array<{ route: Route; label: string; icon: ReactNode; badge?: number }> = [
    { route: { view: "home" }, label: "Home", icon: <I.Home /> },
    { route: { view: "new" }, label: "New session", icon: <I.Plus /> },
    { route: { view: "chat" }, label: "Chat", icon: <I.Chat /> },
    { route: { view: "approvals" }, label: "Approvals", icon: <I.Bell />, badge: pendingCount },
    { route: { view: "pets" }, label: "Pets", icon: <I.Bone /> },
    { route: { view: "rules" }, label: "Rules", icon: <I.Shield /> },
    { route: { view: "settings" }, label: "Settings", icon: <I.Gear /> },
  ];

  return (
    <aside className="sidebar">
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
          {sessions.open.map((s) => (
            renderRow(s)
          ))}
          {sessions.ended.length > 0 && (
            <button className="side-toggle" onClick={() => setShowEnded((v) => !v)}>
              {showEnded ? "Hide" : "Show"} finished ({sessions.ended.length})
            </button>
          )}
          {showEnded && sessions.ended.map((s) => renderRow(s))}
        </div>
      </div>
    </aside>
  );

  function renderRow(s: Session) {
    const active = route.view === "session" && route.sessionId === s.id;
    const meta = STATUS_META[s.status];
    const waiting = Object.values(state.pending).filter((p) => p.sessionId === s.id).length;
    return (
      <button
        key={s.id}
        className={`session-row${active ? " is-active" : ""}${s.endedAt ? " is-ended" : ""}`}
        onClick={() => navigate({ view: "session", sessionId: s.id })}
      >
        <span className={`dot tone-${meta.tone}`} title={meta.label} />
        <span className="session-row-main">
          <span className="session-row-title">{sessionTitle(s, state.agents)}</span>
          <span className="session-row-sub">
            {agentName(state.agents, s.agentId)} · {SOURCE_LABEL[s.source]} · {timeAgo(s.lastEventAt, now)}
          </span>
        </span>
        {waiting > 0 && <span className="badge badge-alert">{waiting}</span>}
      </button>
    );
  }
}
