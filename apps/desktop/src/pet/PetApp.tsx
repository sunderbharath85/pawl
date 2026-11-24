import { useCallback, useEffect, useRef, useState, type ReactNode } from "react";
import { useHubState } from "@krypto/hub-client/react";
import { PetSprite, PetStage } from "@krypto/pet-engine/react";
import type { PetActivity, PetSpritesConfig } from "@krypto/pet-engine";
import type { ConnectionMeta } from "../lib/connection";
import { errorText } from "../lib/format";
import { native, onNative } from "../lib/native";
import { attachNotifications } from "../lib/notifications";
import { usePetViews } from "../lib/pets";
import { useSettings } from "../lib/settings";
import { useSprites } from "../lib/sprites";
import { useConnection } from "../lib/useConnection";
import { useNow } from "../lib/useNow";
import { ErrorBoundary } from "../lib/ErrorBoundary";
import { startHitRectReporter } from "./hitRects";

export function PetApp() {
  const { conn, meta, client } = useConnection();
  const state = useHubState(client);
  const now = useNow(1000);
  const sprites = useSprites();
  const [settings] = useSettings();
  const views = usePetViews(state, now);
  const stripRef = useRef<HTMLDivElement>(null);
  const [width, setWidth] = useState(() => window.innerWidth);
  const [hover, setHover] = useState(false);
  const [toast, setToast] = useState<string | null>(null);

  useEffect(() => {
    const onResize = () => setWidth(window.innerWidth);
    window.addEventListener("resize", onResize);
    return () => window.removeEventListener("resize", onResize);
  }, []);

  useEffect(() => startHitRectReporter(() => stripRef.current), []);
  useEffect(() => attachNotifications(client), [client]);
  useEffect(() => onNative("pet-hover", setHover), []);

  useEffect(() => {
    if (!toast) return;
    const t = setTimeout(() => setToast(null), 4500);
    return () => clearTimeout(t);
  }, [toast]);

  const guard = useCallback((p: Promise<unknown>) => {
    p.catch((err) => setToast(errorText(err)));
  }, []);

  const onPetClick = useCallback(
    (petId: string) => {
      const view = views.find((v) => v.petId === petId);
      const focus = view?.bubble?.requestId
        ? { petId, sessionId: view.sessionId, view: "approvals" as const, requestId: view.bubble.requestId }
        : { petId, sessionId: view?.sessionId, view: view?.sessionId ? ("session" as const) : ("home" as const) };
      guard(native.openPanel(focus));
    },
    [views, guard],
  );

  const connected = meta.phase === "connected";

  return (
    <div ref={stripRef} className={`strip${hover ? " is-hover" : ""}`}>
      {connected ? (
        <ErrorBoundary
          resetKey={views.map((v) => `${v.petId}:${v.species}:${v.activity}`).join("|")}
          fallback={<FallbackPets count={views.length} onOpen={() => guard(native.openPanel({ view: "home" }))} />}
        >
          <PetStage
            className="strip-stage"
            views={views}
            width={width}
            petSize={settings.petSize}
            sprites={sprites}
            onApprove={(requestId) => guard(client.approve(requestId))}
            onDeny={(requestId) => guard(client.deny(requestId))}
            onPetClick={onPetClick}
          />
        </ErrorBoundary>
      ) : (
        <SleepyHub
          meta={meta}
          sprites={sprites}
          size={settings.petSize}
          onWake={() => guard(conn.startHub())}
          onOpen={() => guard(native.openPanel({ view: "home" }))}
        />
      )}
      {toast && (
        <div className="strip-toast" role="status" onClick={() => setToast(null)}>
          {toast}
        </div>
      )}
    </div>
  );
}

interface SleepyHubProps {
  meta: ConnectionMeta;
  sprites: PetSpritesConfig | null;
  size: number;
  onWake(): void;
  onOpen(): void;
}

/** The lone dog shown while the hub is offline: naps and offers to wake the hub. */
function SleepyHub({ meta, sprites, size, onWake, onOpen }: SleepyHubProps) {
  const [dismissed, setDismissed] = useState(false);
  useEffect(() => setDismissed(false), [meta.phase]);

  let activity: PetActivity = "napping";
  let bubble: ReactNode = null;
  switch (meta.phase) {
    case "booting":
      break;
    case "hub-down":
      bubble = (
        <>
          <p className="sleepy-title">Zzz… the hub is asleep.</p>
          <p className="sleepy-sub">{meta.startError ?? "Wake it so I can watch your agents."}</p>
          <div className="sleepy-actions">
            <button className="pill pill-primary" onClick={onWake}>
              Wake the hub
            </button>
            <button className="pill" onClick={onOpen}>
              Panel
            </button>
          </div>
        </>
      );
      break;
    case "starting":
      activity = "thinking";
      bubble = <p className="sleepy-title">Waking the hub… *stretch*</p>;
      break;
    case "connecting":
      activity = "idle";
      bubble = <p className="sleepy-title">Sniffing for the hub…</p>;
      break;
    case "unauthorized":
      activity = "sad";
      bubble = (
        <>
          <p className="sleepy-title">The hub doesn't recognise me.</p>
          <p className="sleepy-sub">Token mismatch — restart the hub or check KRYPTO_HUB_TOKEN.</p>
          <div className="sleepy-actions">
            <button className="pill" onClick={onOpen}>
              Open panel
            </button>
          </div>
        </>
      );
      break;
    case "connected":
      break;
  }

  return (
    <div className="sleepy" data-pet-hit>
      {bubble && !dismissed && (
        <div className="sleepy-bubble" data-pet-hit>
          <button className="sleepy-close" aria-label="Dismiss" onClick={() => setDismissed(true)}>
            ×
          </button>
          {bubble}
        </div>
      )}
      <button className="sleepy-pet" onClick={dismissed ? () => setDismissed(false) : onOpen} title="Krypto">
        <PetSprite species="dog" activity={activity} size={size} facing="left" sprites={sprites} />
      </button>
    </div>
  );
}

/** Shown only if the engine's stage fails to render: a plain dog button so the panel stays reachable. */
function FallbackPets({ count, onOpen }: { count: number; onOpen(): void }) {
  return (
    <div className="sleepy" data-pet-hit>
      <button className="sleepy-pet fallback-dog" onClick={onOpen} title={`${count} pet(s)`}>
        🐕
      </button>
    </div>
  );
}
