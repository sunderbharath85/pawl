import { useCallback, useEffect, useMemo, useRef, useState, type RefObject } from "react";
import { derivePetViews, lastSignificantEvents, type PetView } from "@krypto/pet-engine";
import { PetStage } from "@krypto/pet-engine/react";
import {
  activeMachineId,
  isLocal,
  machineCounts,
  machineDisplayName,
  machineSummary,
  machinesLine,
  type ProposalEntry,
} from "@krypto/hub-client";
import { DEFAULT_SPECIES, type Session } from "@krypto/protocol";
import type { ConnInfo, HostToWebview, SpritesInfo, WebviewHubState } from "../shared/messages";
import { ErrorBoundary } from "./ErrorBoundary";
import { ApprovalCard, ChatPanel, ConnBanner, SessionDetail, SessionList, Toasts, type Toast } from "./parts";
import { loadPersisted, post, savePersisted, type PersistedState } from "./vscodeApi";

const EMPTY_STATE: WebviewHubState = {
  status: "disconnected",
  machines: {},
  devices: [],
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
  machineId: "",
  agentId: "",
  species: DEFAULT_SPECIES,
  name: "Pet",
  activity: "napping",
  since: 0,
  mood: 70,
  visitor: false,
  remoteOffline: false,
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
  const [proposals, setProposals] = useState<Record<string, ProposalEntry>>({});
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
          setProposals(msg.proposals ?? {});
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
  /** This hub's machine and every known one: names, online state, read-only checks. */
  const place = useMemo(() => ({ machine: hub.machine, machines: hub.machines }), [hub.machine, hub.machines]);
  // Chat goes to this machine's hub, so only its own pets answer.
  const localPets = useMemo(() => pets.filter((p) => isLocal(place, p.machineId)), [pets, place]);
  // This machine's requests first (answerable here), then other machines' (read-only).
  const sortedPending = useMemo(
    () => [...pending].sort((a, b) => Number(isLocal(place, b.machineId)) - Number(isLocal(place, a.machineId)) || a.createdAt - b.createdAt),
    [pending, place],
  );
  const machineTotals = useMemo(() => machineCounts({ machines: hub.machines }), [hub.machines]);

  const lastEvents = useMemo(() => lastSignificantEvents(hub.events), [hub.events]);

  const views: PetView[] = useMemo(() => {
    if (!connected) return [SLEEPY_DOG];
    try {
      const derived = derivePetViews({
        pets,
        sessions,
        pending,
        lastEvents,
        machines: hub.machines,
        // Visitors and "runs to you" (PLAN.md migration step 3).
        localMachineId: hub.machine?.id,
        activeMachineId: activeMachineId(place, now),
        now,
      });
      // Keep the sleepy dog at home when this machine has no pet of its own (visitors only).
      return derived.some((v) => !v.visitor && !v.remoteOffline) ? derived : [SLEEPY_DOG, ...derived];
    } catch (err) {
      console.error("[krypto] derivePetViews failed", err);
      return [SLEEPY_DOG];
    }
  }, [connected, pets, sessions, pending, lastEvents, hub.machines, hub.machine, place, now]);

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
        sessions
          .filter((s) => s.machineId === pet.machineId && s.agentId === pet.agentId)
          .sort((a, b) => b.lastEventAt - a.lastEventAt)[0];
      if (target) update({ tab: "sessions", selectedSessionId: target.id });
      // Chat goes to this machine's hub: a visitor without a session just shows the list.
      else if (isLocal(place, pet.machineId)) update({ tab: "chat", chatPetId: petId });
      else update({ tab: "sessions", selectedSessionId: null });
    },
    [connected, hub.pets, hub.sessions, sessions, views, update, place],
  );

  return (
    <div className={`app mode-${mode}`}>
      <Toasts toasts={toasts} onDismiss={(id) => setToasts((t) => t.filter((x) => x.id !== id))} />
      <header className="machine-bar" title={machineSummary(hub.machine)}>
        <svg className="machine-icon" viewBox="0 0 16 16" width="14" height="14" aria-hidden="true">
          <rect x="1.5" y="2.5" width="13" height="8.5" rx="1.2" fill="none" stroke="currentColor" strokeWidth="1.3" />
          <path d="M5.5 14h5M8 11v3" stroke="currentColor" strokeWidth="1.3" strokeLinecap="round" />
        </svg>
        <span className="machine-name">{machineDisplayName(hub.machine)}</span>
        <span className="spacer" />
        {connected && machineTotals.total > 1 && (
          <button
            className="link small machine-count"
            title="Show machines"
            onClick={() => post({ type: "command", command: "showMachines" })}
          >
            {machinesLine(machineTotals.total, machineTotals.online)}
          </button>
        )}
        <button
          className="link small"
          disabled={!connected || !hub.machine}
          title="Rename this machine"
          onClick={() => post({ type: "command", command: "renameMachine" })}
        >
          Rename
        </button>
      </header>
      <ConnBanner conn={conn} />

      <div className="stage" ref={stageRef}>
        <ErrorBoundary resetKey={sprites} fallback={<div className="stage-empty">The pet could not be drawn.</div>}>
          <PetStage
            views={views}
            width={stageWidth}
            sprites={sprites}
            showNames
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
          {sortedPending.map((req) => (
            <ApprovalCard
              key={req.requestId}
              request={req}
              session={hub.sessions[req.sessionId]}
              agents={hub.agents}
              pets={pets}
              place={place}
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
              place={place}
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
              place={place}
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
            pets={localPets}
            agents={hub.agents}
            place={place}
            proposals={proposals}
            now={now}
            onOpenSession={(id) => update({ tab: "sessions", selectedSessionId: id })}
            connected={connected}
            petId={persisted.chatPetId}
            onPetChange={(id) => update({ chatPetId: id })}
          />
        )}
      </main>

      <footer className="footer">
        <button className="link" disabled={!connected || localPets.length === 0} onClick={() => post({ type: "command", command: "renamePet" })}>
          Rename pet
        </button>
        <button
          className="link"
          disabled={!connected || localPets.length === 0}
          onClick={() => post({ type: "command", command: "changeSpecies" })}
        >
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
