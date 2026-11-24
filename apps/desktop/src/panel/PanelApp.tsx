import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useHubState } from "@krypto/hub-client/react";
import { onNative, type PanelFocus } from "../lib/native";
import { usePetViews } from "../lib/pets";
import { useSprites } from "../lib/sprites";
import { useConnection } from "../lib/useConnection";
import { useNow } from "../lib/useNow";
import { PanelContext, type PanelCtx, type Route } from "./context";
import { Sidebar } from "./Sidebar";
import { HubBanner } from "./HubBanner";
import { HomeView } from "./views/HomeView";
import { SessionView } from "./views/SessionView";
import { NewSessionView } from "./views/NewSessionView";
import { ChatView } from "./views/ChatView";
import { ApprovalsView } from "./views/ApprovalsView";
import { PetsView } from "./views/PetsView";
import { RulesView } from "./views/RulesView";
import { SettingsView } from "./views/SettingsView";
import { MachinesView } from "./views/MachinesView";
import { PendingStrip } from "./views/PendingStrip";

interface Toast {
  id: number;
  text: string;
  tone: "info" | "error";
}

function routeFromFocus(focus: PanelFocus): Route {
  switch (focus.view) {
    case "approvals":
      return { view: "approvals", requestId: focus.requestId };
    case "chat":
      return { view: "chat", petId: focus.petId };
    case "pets":
      return { view: "pets", petId: focus.petId };
    case "new":
      return { view: "new" };
    case "rules":
      return { view: "rules" };
    case "home":
      return { view: "home" };
    case "session":
    case undefined:
      return focus.sessionId ? { view: "session", sessionId: focus.sessionId } : { view: "home" };
  }
}

export function PanelApp() {
  const { conn, meta, client } = useConnection();
  const state = useHubState(client);
  const now = useNow(1000);
  const sprites = useSprites();
  const views = usePetViews(state, now);
  const [route, setRoute] = useState<Route>({ view: "home" });
  const [toasts, setToasts] = useState<Toast[]>([]);
  const toastSeq = useRef(0);

  const navigate = useCallback((r: Route) => setRoute(r), []);
  const toast = useCallback((text: string, tone: "info" | "error" = "info") => {
    const id = ++toastSeq.current;
    setToasts((t) => [...t.slice(-3), { id, text, tone }]);
    setTimeout(() => setToasts((t) => t.filter((x) => x.id !== id)), tone === "error" ? 6000 : 3500);
  }, []);

  useEffect(() => onNative("panel-focus", (focus) => setRoute(routeFromFocus(focus))), []);

  // Input in the panel means the user is on this machine (hub-client throttles the reports).
  useEffect(() => {
    const report = () => void client.reportUserActivity();
    const events = ["pointerdown", "pointermove", "keydown", "wheel", "focus"] as const;
    for (const e of events) window.addEventListener(e, report, { passive: true });
    return () => {
      for (const e of events) window.removeEventListener(e, report);
    };
  }, [client]);

  // If the open session disappears (hub restarted with a fresh db), fall back home.
  useEffect(() => {
    if (route.view === "session" && meta.phase === "connected" && !state.sessions[route.sessionId]) {
      const t = setTimeout(() => {
        if (!client.getState().sessions[route.sessionId]) setRoute({ view: "home" });
      }, 1500);
      return () => clearTimeout(t);
    }
  }, [route, meta.phase, state.sessions, client]);

  const ctx: PanelCtx = useMemo(
    () => ({ conn, client, meta, state, views, sprites, now, route, navigate, toast }),
    [conn, client, meta, state, views, sprites, now, route, navigate, toast],
  );

  return (
    <PanelContext.Provider value={ctx}>
      <div className="app">
        <Sidebar />
        <main className="main">
          <HubBanner />
          {route.view !== "approvals" && <PendingStrip />}
          <div className="main-scroll" key={route.view === "session" ? `s:${route.sessionId}` : route.view}>
            {renderRoute(route)}
          </div>
        </main>
        <div className="toasts" aria-live="polite">
          {toasts.map((t) => (
            <div key={t.id} className={`toast toast-${t.tone}`}>
              {t.text}
            </div>
          ))}
        </div>
      </div>
    </PanelContext.Provider>
  );
}

function renderRoute(route: Route) {
  switch (route.view) {
    case "home":
      return <HomeView />;
    case "session":
      return <SessionView sessionId={route.sessionId} />;
    case "new":
      return <NewSessionView />;
    case "chat":
      return <ChatView initialPetId={route.petId} />;
    case "approvals":
      return <ApprovalsView focusRequestId={route.requestId} />;
    case "pets":
      return <PetsView focusPetId={route.petId} />;
    case "rules":
      return <RulesView />;
    case "settings":
      return <SettingsView />;
    case "machines":
      return <MachinesView />;
  }
}
