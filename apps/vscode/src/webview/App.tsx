import { useCallback, useEffect, useMemo, useRef, useState, type RefObject } from "react";
import { derivePetViews, lastSignificantEvents, type PetView } from "@krypto/pet-engine";
import { PetStage } from "@krypto/pet-engine/react";
import { DEFAULT_SPECIES, type Session } from "@krypto/protocol";
import type { ConnInfo, HostToWebview, SpritesInfo, WebviewHubState } from "../shared/messages";
import { ErrorBoundary } from "./ErrorBoundary";
import { ApprovalCard, ChatPanel, ConnBanner, SessionDetail, SessionList, Toasts, type Toast } from "./parts";
import { loadPersisted, post, savePersisted, type PersistedState } from "./vscodeApi";

const EMPTY_STATE: WebviewHubState = {
  status: "disconnected",
  agents: [],
  sessions: {},
  pets: {},
  pending: {},
  events: {},
  chat: [],
  notifications: [],
};

/** Shown while offline or before any pet exists: the default dog, asleep in its bed. */
const SLEEPY_DOG: PetView = {
  petId: "__krypto-placeholder",
  agentId: "",
  species: DEFAULT_SPECIES,
  name: "Pet",
  activity: "napping",
  since: 0,
  mood: 70,
};

const INITIAL_CONN: ConnInfo = { status: "connecting", offline: false, missingToken: false, url: "" };

function useNow(intervalMs: number): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const t = setInterval(() => setNow(Date.now()), intervalMs);
    return () => clearInterval(t);
  }, [intervalMs]);
  return now;
}

function useWidth<T extends HTMLElement>(): [RefObject<T | null>, number] {
  const ref = useRef<T | null>(null);
  const [width, setWidth] = useState(300);
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    const ro = new ResizeObserver((entries) => {
      const w = Math.floor(entries[0]?.contentRect.width ?? 0);
      if (w > 0) setWidth(w);
    });
    ro.observe(el);
    return () => ro.disconnect();
  }, []);
  return [ref, width];
}

export function App() {
  const [hub, setHub] = useState<WebviewHubState>(EMPTY_STATE);
  const [conn, setConn] = useState<ConnInfo>(INITIAL_CONN);
  const [sprites, setSprites] = useState<SpritesInfo | null>(null);
  const [mode, setMode] = useState<"view" | "panel">("view");
  const [workspaceFolders, setWorkspaceFolders] = useState<string[]>([]);
  const [persisted, setPersisted] = useState<PersistedState>(loadPersisted);
  const [toasts, setToasts] = useState<Toast[]>([]);
  const now = useNow(1000);
  const [stageRef, stageWidth] = useWidth<HTMLDivElement>();

  const update = useCallback((patch: Partial<PersistedState>) => {
    setPersisted((prev) => {
      const next = { ...prev, ...patch };
      savePersisted(next);
      return next;
    });
  }, []);

  const toast = useCallback((text: string) => {
    const id = Date.now() + Math.random();
    setToasts((t) => [...t.slice(-2), { id, text }]);
    setTimeout(() => setToasts((t) => t.filter((x) => x.id !== id)), 7000);
  }, []);

  useEffect(() => {
    const onMessage = (ev: MessageEvent<HostToWebview>) => {
      const msg = ev.data;
      if (!msg || typeof msg !== "object") return;
      switch (msg.type) {
        case "init":
          setMode(msg.mode);
          setSprites(msg.sprites);
          setWorkspaceFolders(msg.workspaceFolders);
          break;
        case "state":
          setConn(msg.conn);
          setHub(msg.state);
          break;
        case "focusSession":
          update({ tab: "sessions", selectedSessionId: msg.sessionId });
          break;
        case "error":
          toast(msg.message);
          break;
      }
    };
    window.addEventListener("message", onMessage);
    post({ type: "ready" });
    return () => window.removeEventListener("message", onMessage);
  }, [toast, update]);

  const connected = conn.status === "connected";
  const sessions = useMemo(() => Object.values(hub.sessions), [hub.sessions]);
  const pending = useMemo(() => Object.values(hub.pending).sort((a, b) => a.createdAt - b.createdAt), [hub.pending]);
  const pets = useMemo(() => Object.values(hub.pets), [hub.pets]);

  const lastEvents = useMemo(() => lastSignificantEvents(hub.events), [hub.events]);

  const views: PetView[] = useMemo(() => {
    if (!connected) return [SLEEPY_DOG];
    try {
      const derived = derivePetViews({ pets, sessions, pending, lastEvents, now });
      return derived.length > 0 ? derived : [SLEEPY_DOG];
    } catch (err) {
      console.error("[krypto] derivePetViews failed", err);
      return [SLEEPY_DOG];
    }
  }, [connected, pets, sessions, pending, lastEvents, now]);

  const selected: Session | undefined = persisted.selectedSessionId ? hub.sessions[persisted.selectedSessionId] : undefined;

  const onPetClick = useCallback(
    (petId: string) => {
      const pet = hub.pets[petId];
      if (!pet) {
        if (connected) post({ type: "command", command: "startSession" });
        return;
      }
      const view = views.find((v) => v.petId === petId);
      const target =
        (view?.sessionId && hub.sessions[view.sessionId]) ||
        sessions.filter((s) => s.agentId === pet.agentId).sort((a, b) => b.lastEventAt - a.lastEventAt)[0];
      if (target) update({ tab: "sessions", selectedSessionId: target.id });
      else update({ tab: "chat", chatPetId: petId });
    },
    [connected, hub.pets, hub.sessions, sessions, views, update],
  );

  return (
    <div className={`app mode-${mode}`}>
      <Toasts toasts={toasts} onDismiss={(id) => setToasts((t) => t.filter((x) => x.id !== id))} />
      <ConnBanner conn={conn} />

      <div className="stage" ref={stageRef}>
        <ErrorBoundary resetKey={sprites} fallback={<div className="stage-empty">The pet could not be drawn.</div>}>
          <PetStage
            views={views}
            width={stageWidth}
            sprites={sprites}
            showNames={pets.length > 1}
            petSize={mode === "panel" ? 112 : 84}
            onApprove={(requestId) => post({ type: "approve", requestId })}
            onDeny={(requestId) => post({ type: "deny", requestId })}
            onPetClick={onPetClick}
          />
        </ErrorBoundary>
        {connected && pets.length === 0 && <div className="stage-empty">No pets yet — start a session to wake one up.</div>}
      </div>

      {connected && pending.length > 0 && (
        <section className="approvals" aria-label="Waiting for approval">
          <h2>
            Needs you <span className="count">{pending.length}</span>
          </h2>
          {pending.map((req) => (
            <ApprovalCard
              key={req.requestId}
              request={req}
              session={hub.sessions[req.sessionId]}
              agents={hub.agents}
              now={now}
            />
          ))}
        </section>
      )}

      <nav className="tabs" role="tablist">
        <button
          role="tab"
          aria-selected={persisted.tab === "sessions"}
          className={persisted.tab === "sessions" ? "active" : ""}
          onClick={() => update({ tab: "sessions" })}
        >
          Sessions
        </button>
        <button
          role="tab"
          aria-selected={persisted.tab === "chat"}
          className={persisted.tab === "chat" ? "active" : ""}
          onClick={() => update({ tab: "chat" })}
        >
          Chat
        </button>
        <span className="spacer" />
        <button
          className="icon"
          title="Start a session"
          disabled={!connected}
          onClick={() => post({ type: "command", command: "startSession" })}
        >
          ＋
        </button>
        {mode === "view" && (
          <button className="icon" title="Open the pet in an editor tab" onClick={() => post({ type: "command", command: "openPanel" })}>
            ⧉
          </button>
        )}
      </nav>

      <main className="content">
        {persisted.tab === "sessions" ? (
          selected ? (
            <SessionDetail
              session={selected}
              events={hub.events[selected.id] ?? []}
              agents={hub.agents}
              pets={pets}
              pending={pending.filter((p) => p.sessionId === selected.id)}
              connected={connected}
              now={now}
              workspaceFolders={workspaceFolders}
              onBack={() => update({ selectedSessionId: null })}
            />
          ) : (
            <SessionList
              sessions={sessions}
              agents={hub.agents}
              pets={pets}
              connected={connected}
              showEnded={persisted.showEnded}
              now={now}
              onToggleEnded={() => update({ showEnded: !persisted.showEnded })}
              onSelect={(id) => update({ selectedSessionId: id })}
            />
          )
        ) : (
          <ChatPanel
            chat={hub.chat}
            pets={pets}
            agents={hub.agents}
            connected={connected}
            petId={persisted.chatPetId}
            onPetChange={(id) => update({ chatPetId: id })}
          />
        )}
      </main>

      <footer className="footer">
        <button className="link" disabled={!connected || pets.length === 0} onClick={() => post({ type: "command", command: "changeSpecies" })}>
          Change species
        </button>
        <button className="link" onClick={() => post({ type: "command", command: "installHooks" })}>
          Install hooks
        </button>
        {conn.hubVersion && <span className="muted">hub {conn.hubVersion}</span>}
      </footer>
    </div>
  );
}
