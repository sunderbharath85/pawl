import { useMemo } from "react";
import { STATUS_META, SOURCE_LABEL, agentName, isActive, sessionTitle, timeAgo } from "../../lib/format";
import { usePanel } from "../context";
import * as I from "../icons";
import { ACTIVITY_LABEL, PetAvatar, safePetLine } from "../PetAvatar";

function greeting(): string {
  const h = new Date().getHours();
  if (h < 5) return "Up late";
  if (h < 12) return "Good morning";
  if (h < 18) return "Good afternoon";
  return "Good evening";
}

export function HomeView() {
  const { state, views, navigate, now, meta } = usePanel();
  const sessions = Object.values(state.sessions);
  const active = useMemo(() => sessions.filter(isActive).sort((a, b) => b.lastEventAt - a.lastEventAt), [sessions]);
  const recent = useMemo(
    () => sessions.filter((s) => !isActive(s)).sort((a, b) => b.lastEventAt - a.lastEventAt).slice(0, 5),
    [sessions],
  );
  const pendingCount = Object.keys(state.pending).length;
  const notes = state.notifications.slice(-5).reverse();
  const lead = views[0];

  return (
    <section className="view home">
      <div className="home-hero">
        <div className="home-hero-text">
          <span className="eyebrow">
            <I.Paw size={12} /> Krypto
          </span>
          <h1>
            {greeting()}! {active.length > 0 ? `${active.length} agent${active.length > 1 ? "s are" : " is"} busy.` : "The pack is resting."}
          </h1>
          <p>
            {pendingCount > 0
              ? `${pendingCount} request${pendingCount > 1 ? "s need" : " needs"} your OK.`
              : meta.phase === "connected"
                ? "Nothing needs you right now."
                : "Connect the hub to start watching agents."}
          </p>
          <div className="form-actions">
            <button className="pill pill-primary big" onClick={() => navigate({ view: "new" })}>
              <I.Plus size={15} /> New session
            </button>
            <button className="pill big" onClick={() => navigate({ view: "chat" })}>
              <I.Chat size={15} /> Chat
            </button>
            {pendingCount > 0 && (
              <button className="pill pill-bad big" onClick={() => navigate({ view: "approvals" })}>
                <I.Bell size={15} /> Review {pendingCount}
              </button>
            )}
          </div>
        </div>
        <div className="home-hero-pets">
          <div className="home-rug" />
          {(views.length > 0 ? views.slice(0, 4) : []).map((v, i) => (
            <button
              key={v.petId}
              className="home-pet"
              style={{ zIndex: 10 - i }}
              onClick={() => navigate(v.sessionId ? { view: "session", sessionId: v.sessionId } : { view: "pets", petId: v.petId })}
              title={`${v.name}: ${ACTIVITY_LABEL[v.activity]}`}
            >
              <PetAvatar species={v.species} activity={v.activity} size={i === 0 ? 150 : 92} facing={i % 2 ? "left" : "right"} />
              <span className="home-pet-name">{v.name}</span>
            </button>
          ))}
          {lead && <div className="home-speech">“{safePetLine(lead.species, lead.activity, lead.since)}”</div>}
        </div>
      </div>

      <div className="home-grid">
        <div className="card">
          <h3>Working now</h3>
          {active.length === 0 ? (
            <p className="muted">No active turns. Your pets are napping in their beds.</p>
          ) : (
            <ul className="mini-list">
              {active.map((s) => (
                <li key={s.id}>
                  <button className="mini-row" onClick={() => navigate({ view: "session", sessionId: s.id })}>
                    <span className={`dot tone-${STATUS_META[s.status].tone}`} />
                    <span className="mini-title">{sessionTitle(s, state.agents)}</span>
                    <span className="muted">{STATUS_META[s.status].label}</span>
                  </button>
                </li>
              ))}
            </ul>
          )}
        </div>

        <div className="card">
          <h3>Recently</h3>
          {recent.length === 0 ? (
            <p className="muted">Finished sessions show up here.</p>
          ) : (
            <ul className="mini-list">
              {recent.map((s) => (
                <li key={s.id}>
                  <button className="mini-row" onClick={() => navigate({ view: "session", sessionId: s.id })}>
                    <span className={`dot tone-${STATUS_META[s.status].tone}`} />
                    <span className="mini-title">{sessionTitle(s, state.agents)}</span>
                    <span className="muted">
                      {agentName(state.agents, s.agentId)} · {SOURCE_LABEL[s.source]} · {timeAgo(s.lastEventAt, now)}
                    </span>
                  </button>
                </li>
              ))}
            </ul>
          )}
        </div>

        <div className="card">
          <h3>Pet mail</h3>
          {notes.length === 0 ? (
            <p className="muted">Hub notifications (watchdog nudges, hook alerts, reports) land here.</p>
          ) : (
            <ul className="notes">
              {notes.map((n) => (
                <li key={n.id} className={`note note-${n.level}`}>
                  <strong>{n.title}</strong>
                  <span>{n.body}</span>
                  <time className="muted">{timeAgo(n.ts, now)}</time>
                </li>
              ))}
            </ul>
          )}
        </div>
      </div>
    </section>
  );
}
