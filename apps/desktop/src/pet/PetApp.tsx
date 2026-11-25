// The pet window: one default pet (it orchestrates every agent and mirrors the most important state across all
// sessions and machines), optionally one pet per active session beside it, and one chat bubble.
// - Press and drag a pet to move the window anywhere (Rust saves the spot and keeps it on screen).
// - Click a pet: toggle its bubble. Esc or clicking elsewhere closes it. A new approval opens it by itself.
// - Right-click: Open hub, Reset position, Settings, Quit (a native menu). The panel never opens on its own.
import { useCallback, useEffect, useMemo, useRef, useState, type HTMLAttributes, type KeyboardEvent, type MouseEvent, type PointerEvent, type ReactNode } from "react";
import { useHubState } from "@pawl/hub-client/react";
import { PetSprite } from "@pawl/pet-engine/react";
import type { PetActivity } from "@pawl/pet-engine";
import type { ConnectionMeta } from "../lib/connection";
import { ErrorBoundary } from "../lib/ErrorBoundary";
import { errorText } from "../lib/format";
import { isTauri, native, onNative } from "../lib/native";
import { attachNotifications } from "../lib/notifications";
import { layoutPetWindow, petFrame, placeBubble, PressTracker, type PetSpace, type Rect } from "../lib/petLayout";
import { autoOpenBubble, clickBubble, DEFAULT_TARGET as DEFAULT, setupLooksDone, type OpenBubble } from "../lib/bubble";
import { chatPhase, petPose, sessionPhase } from "../lib/petMotion";
import { activeSessionPets, defaultPetState, newRequestIds, type SessionPet } from "../lib/petScreen";
import { usePetViews } from "../lib/pets";
import { useSettings } from "../lib/settings";
import { useSprites } from "../lib/sprites";
import { useConnection } from "../lib/useConnection";
import { renameDefaultPet, useDefaultPetName } from "../lib/usePetName";
import { useNow } from "../lib/useNow";
import { notifiesHere } from "../lib/where";
import { DefaultBubble, SayBubble, SessionBubble, SleepyBubble, type BubbleEnv } from "./Bubble";
import { startHitRectReporter } from "./hitRects";

const NOTE_MS = 4500;

function sameSpace(a: PetSpace | null, b: PetSpace): boolean {
  return !!a && a.left === b.left && a.right === b.right && a.above === b.above && a.below === b.below;
}

function sleepyActivity(phase: ConnectionMeta["phase"]): PetActivity {
  switch (phase) {
    case "starting":
      return "thinking";
    case "connecting":
      return "idle";
    case "unauthorized":
      return "sad";
    default:
      return "napping";
  }
}

export function PetApp() {
  const { conn, meta, client } = useConnection();
  const state = useHubState(client);
  const now = useNow(1000);
  const sprites = useSprites();
  const [settings, setSettings] = useSettings();
  const views = usePetViews(state, now);
  const connected = meta.phase === "connected";
  const rootRef = useRef<HTMLDivElement>(null);
  const [space, setSpace] = useState<PetSpace | null>(null);
  const [hover, setHover] = useState(false);
  const [open, setOpen] = useState<OpenBubble | null>(null);
  const [note, setNote] = useState<string | null>(null);
  /** Requests someone already answered: hidden at once (the mirror removes them soon after). */
  const [dismissed, setDismissed] = useState<ReadonlySet<string>>(() => new Set());
  /** Chat messages the person sent from the default pet's bubble, and when each session was prompted from its bubble. */
  const [sentChat, setSentChat] = useState<ReadonlySet<string>>(() => new Set());
  const [sentPrompts, setSentPrompts] = useState<Readonly<Record<string, number>>>({});
  const reducedMotion = usePrefersReducedMotion();
  // The default pet's own name: the hub's (bot.petName), the last one seen while the hub is down.
  const { name, canRename } = useDefaultPetName(client, state, connected, { migrate: true });
  /** "Rename…" was picked in the pet's menu: the bubble shows the rename field. */
  const [renaming, setRenaming] = useState(false);

  useEffect(() => {
    setDismissed((d) => {
      const next = new Set([...d].filter((id) => id in state.pending));
      return next.size === d.size ? d : next;
    });
  }, [state.pending]);

  const main = useMemo(() => defaultPetState(state, views), [state, views]);
  const pending = useMemo(() => main.pending.filter((p) => !dismissed.has(p.requestId)), [main.pending, dismissed]);
  const sessionMode = connected && settings.petsOnScreen === "sessions";
  const sessions = useMemo(() => (sessionMode ? activeSessionPets(state, views) : { pets: [] as SessionPet[], hidden: 0 }), [sessionMode, state, views]);
  const layout = useMemo(
    () =>
      layoutPetWindow({
        petSize: settings.petSize,
        showNames: settings.showPetNames,
        sessionCount: sessions.pets.length,
        more: sessions.hidden > 0,
        space,
      }),
    [settings.petSize, settings.showPetNames, sessions.pets.length, sessions.hidden, space],
  );
  const frameKey = JSON.stringify(petFrame(layout));

  const acceptSpace = useCallback((s: PetSpace | null) => {
    if (s) setSpace((prev) => (sameSpace(prev, s) ? prev : s));
  }, []);

  // Window plumbing: click-through hit-rects, frame, room around the pet, hover, activity, notifications.
  useEffect(() => startHitRectReporter(() => rootRef.current), []);
  useEffect(() => onNative("pet-space", acceptSpace), [acceptSpace]);
  useEffect(() => {
    if (!isTauri) return;
    native
      .setPetFrame(JSON.parse(frameKey))
      .then(acceptSpace)
      .catch((err) => console.warn("[desktop] set_pet_frame failed", err));
  }, [frameKey, acceptSpace]);
  useEffect(() => attachNotifications(client), [client]);
  useEffect(() => onNative("pet-hover", setHover), []);
  // Cursor movement anywhere on this machine counts as the user being here (hub-client throttles).
  useEffect(() => onNative("user-activity", () => void client.reportUserActivity()), [client]);
  useEffect(() => onNative("pet-attention", () => setOpen((o) => autoOpenBubble(o, DEFAULT))), []);
  useEffect(
    () =>
      onNative("pet-rename", () => {
        setOpen({ target: DEFAULT, by: "click" });
        setRenaming(true);
      }),
    [],
  );
  useEffect(() => {
    if (open?.target !== DEFAULT || !connected) setRenaming(false);
  }, [open?.target, connected]);

  useEffect(() => {
    if (!note) return;
    const t = setTimeout(() => setNote(null), NOTE_MS);
    return () => clearTimeout(t);
  }, [note]);

  // A new approval opens a bubble by itself (where the user is, like notifications): the session pet's when it is
  // on screen, else the default pet's (which lists every approval). It never replaces an open bubble: the user may
  // be typing there; the badge on the pet shows the approval instead.
  const seen = useRef<Set<string>>(new Set());
  useEffect(() => {
    const fresh = newRequestIds(seen.current, pending);
    seen.current = new Set(pending.map((p) => p.requestId));
    if (fresh.length === 0 || !notifiesHere(client.getState())) return;
    const req = pending.find((p) => p.requestId === fresh[0]);
    const target = req && sessions.pets.some((p) => p.sessionId === req.sessionId) ? req.sessionId : DEFAULT;
    setOpen((o) => autoOpenBubble(o, target));
  }, [pending, sessions.pets, client]);

  // The hub went away (or rejects us): the default pet says so. Back online: that bubble closes.
  useEffect(() => {
    if (meta.phase === "hub-down" || meta.phase === "unauthorized") setOpen({ target: DEFAULT, by: "hub" });
    else if (meta.phase === "connected") setOpen((o) => (o?.by === "hub" ? null : o));
  }, [meta.phase]);

  // A hub that is already set up (a named machine with sessions or installed hooks) needs no setup hint: mark the
  // setup done (both windows share the setting), checking the hooks once only when there is no session yet.
  const hooksChecked = useRef(false);
  const machineName = state.machine?.name;
  const sessionCount = Object.keys(state.sessions).length;
  useEffect(() => {
    if (!connected || settings.setupDone) return;
    if (setupLooksDone({ machineName, sessionCount, hooksInstalled: false })) {
      setSettings({ setupDone: true, setupOffered: true });
      return;
    }
    if (!machineName?.trim() || hooksChecked.current) return;
    hooksChecked.current = true;
    client
      .hookStatus()
      .then((list) => {
        if (setupLooksDone({ machineName, sessionCount: 0, hooksInstalled: list.some((h) => h.state === "installed") })) {
          setSettings({ setupDone: true, setupOffered: true });
        }
      })
      .catch(() => {});
  }, [connected, settings.setupDone, machineName, sessionCount, client, setSettings]);

  // A session pet's bubble closes when its session is no longer on screen.
  useEffect(() => {
    if (open && open.target !== DEFAULT && !sessions.pets.some((p) => p.sessionId === open.target)) setOpen(null);
  }, [open, sessions.pets]);

  // Esc or clicking outside (the window loses focus) closes the bubble.
  useEffect(() => {
    const onKey = (e: globalThis.KeyboardEvent) => {
      if (e.key === "Escape") setOpen(null);
    };
    const onBlur = () => setOpen(null);
    // No webview context menu here: right-click is the pet's own menu.
    const onContext = (e: globalThis.MouseEvent) => e.preventDefault();
    window.addEventListener("keydown", onKey);
    window.addEventListener("blur", onBlur);
    window.addEventListener("contextmenu", onContext);
    return () => {
      window.removeEventListener("keydown", onKey);
      window.removeEventListener("blur", onBlur);
      window.removeEventListener("contextmenu", onContext);
    };
  }, []);

  const say = useCallback((text: string) => setNote(text), []);
  const dismiss = useCallback((id: string) => setDismissed((d) => new Set(d).add(id)), []);
  const chatSent = useCallback((id: string) => setSentChat((s) => new Set([...s].slice(-20)).add(id)), []);
  const promptSent = useCallback((sessionId: string, at: number) => setSentPrompts((p) => ({ ...p, [sessionId]: at })), []);
  const env: BubbleEnv = useMemo(
    () => ({ client, state, now, connected, say, dismiss, chatSent, promptSent }),
    [client, state, now, connected, say, dismiss, chatSent, promptSent],
  );

  const toggle = useCallback((target: string) => setOpen((o) => clickBubble(o, target)), []);
  const menu = useCallback(() => {
    native.petContextMenu(name, canRename).catch((err) => setNote(errorText(err)));
  }, [name, canRename]);
  const rename = useCallback((raw: string) => renameDefaultPet(client, state, raw), [client, state]);

  const press = useRef(new PressTracker());
  const petHandlers = (target: string): PetHandlers => ({
    onPointerDown: (e: PointerEvent<HTMLDivElement>) => {
      if (e.button !== 0) return;
      press.current.down(e.screenX, e.screenY);
    },
    onPointerMove: (e: PointerEvent<HTMLDivElement>) => {
      if ((e.buttons & 1) === 0) return;
      if (press.current.move(e.screenX, e.screenY)) native.petStartDrag().catch((err) => console.warn("[desktop] drag failed", err));
    },
    onPointerUp: () => press.current.up(),
    onClick: (e: MouseEvent<HTMLDivElement>) => {
      // A drag is not a click; the second click of a double-click does nothing (double-click opens nothing).
      if (!press.current.click() || e.detail > 1) return;
      toggle(target);
    },
    onContextMenu: (e: MouseEvent<HTMLDivElement>) => {
      e.preventDefault();
      menu();
    },
    onKeyDown: (e: KeyboardEvent<HTMLDivElement>) => {
      if (e.key === "Enter" || e.key === " ") {
        e.preventDefault();
        toggle(target);
      } else if (e.key === "ContextMenu" || (e.key === "F10" && e.shiftKey)) {
        e.preventDefault();
        menu();
      }
    },
  });

  const facing = space && space.right < space.left ? "left" : "right";
  // Still by default: a pet moves only while the person chats with it in its bubble (Settings > Pet animation).
  const defaultChat = useMemo(() => state.chat.filter((m) => !m.petId), [state.chat]);
  const mainPose = petPose({
    mode: settings.petAnimation,
    reducedMotion,
    agentActivity: connected ? main.activity : sleepyActivity(meta.phase),
    phase: chatPhase(defaultChat, sentChat, now),
  });
  const mainActivity = mainPose.activity;
  const needsYou = connected && main.mood === "needs_you";
  const openPet = open && open.target !== DEFAULT ? sessions.pets.find((p) => p.sessionId === open.target) : undefined;
  const openIndex = openPet ? sessions.pets.indexOf(openPet) : -1;
  const targetRect: Rect = openIndex >= 0 ? layout.sessions[openIndex]! : layout.main;
  const place = placeBubble(layout, targetRect);

  let bubble = null;
  if (open && !connected && open.target === DEFAULT) {
    bubble = <SleepyBubble meta={meta} place={place} windowHeight={layout.height} name={name} onWake={() => void conn.startHub().catch((err) => setNote(errorText(err)))} onClose={() => setOpen(null)} />;
  } else if (open && connected && openPet) {
    bubble = (
      <SessionBubble
        key={openPet.sessionId}
        env={env}
        place={place}
        windowHeight={layout.height}
        pet={openPet}
        pending={openPet.pending.filter((p) => !dismissed.has(p.requestId))}
        focusInput={open.by === "click"}
        note={note}
        onClose={() => setOpen(null)}
      />
    );
  } else if (open && connected && open.target === DEFAULT) {
    bubble = (
      <DefaultBubble
        key={DEFAULT}
        env={env}
        place={place}
        windowHeight={layout.height}
        name={name}
        main={main}
        pending={pending}
        setupDone={settings.setupDone}
        focusInput={open.by === "click"}
        note={note}
        renaming={renaming}
        onRename={rename}
        onRenameDone={() => setRenaming(false)}
        onClose={() => setOpen(null)}
      />
    );
  } else if (note) {
    bubble = <SayBubble place={placeBubble(layout, layout.main)} windowHeight={layout.height} text={note} onClick={() => setNote(null)} />;
  }

  const badge = open?.target !== DEFAULT ? pending.length : 0;
  const mainLabel = `${name}: ${connected ? main.line : "the hub is not connected"}`;

  return (
    <div ref={rootRef} className={`pet-root${hover ? " is-hover" : ""}`} style={{ width: layout.width, height: layout.height }}>
      <ErrorBoundary resetKey={`${name}:${mainActivity}:${sessions.pets.length}`} fallback={<FallbackDog rect={layout.main} name={name} onMenu={menu} />}>
        <PetBox
          rect={layout.main}
          label={mainLabel}
          name={settings.showPetNames ? name : null}
          badge={badge}
          mark={needsYou && badge === 0}
          still={!mainPose.animate}
          pressed={open?.target === DEFAULT}
          handlers={petHandlers(DEFAULT)}
        >
          <PetSprite species={settings.defaultPetSpecies} activity={mainActivity} size={layout.main.w} facing={facing} sprites={sprites} title={mainLabel} />
        </PetBox>
        {sessions.pets.map((p, i) => {
          const rect = layout.sessions[i]!;
          const label = `${p.label}: ${p.pending.length > 0 ? "needs you" : p.activity}`;
          const pose = petPose({
            mode: settings.petAnimation,
            reducedMotion,
            agentActivity: p.activity,
            phase: sessionPhase(state.events[p.sessionId] ?? [], sentPrompts[p.sessionId], now),
          });
          const petBadge = open?.target === p.sessionId ? 0 : p.pending.filter((r) => !dismissed.has(r.requestId)).length;
          return (
            <PetBox
              key={p.sessionId}
              rect={rect}
              label={label}
              name={settings.showPetNames ? p.label : null}
              badge={petBadge}
              mark={p.activity === "alert" && petBadge === 0}
              still={!pose.animate}
              pressed={open?.target === p.sessionId}
              session
              handlers={petHandlers(p.sessionId)}
            >
              <PetSprite species={p.species} activity={pose.activity} size={rect.w} facing={facing} skin={p.skin} sprites={sprites} title={label} />
            </PetBox>
          );
        })}
        {layout.more && (
          <button
            type="button"
            className="pet-more"
            style={{ left: layout.more.x, top: layout.more.y, width: layout.more.w, height: layout.more.h }}
            data-pet-hit
            title={`${sessions.hidden} more active session${sessions.hidden === 1 ? "" : "s"}: ask ${name}`}
            onClick={() => toggle(DEFAULT)}
            onContextMenu={(e) => {
              e.preventDefault();
              menu();
            }}
          >
            +{sessions.hidden}
          </button>
        )}
      </ErrorBoundary>
      <ErrorBoundary resetKey={`${open?.target ?? ""}:${meta.phase}`} fallback={null}>
        {bubble}
      </ErrorBoundary>
    </div>
  );
}

type PetHandlers = Pick<HTMLAttributes<HTMLDivElement>, "onPointerDown" | "onPointerMove" | "onPointerUp" | "onClick" | "onContextMenu" | "onKeyDown">;

interface PetBoxProps {
  rect: Rect;
  label: string;
  name: string | null;
  badge: number;
  /** A small static "!" (it needs you, with no count to show). */
  mark?: boolean;
  /** Drawn without any animation. */
  still?: boolean;
  pressed: boolean;
  session?: boolean;
  handlers: PetHandlers;
  children: ReactNode;
}

function PetBox({ rect, label, name, badge, mark, still, pressed, session, handlers, children }: PetBoxProps) {
  return (
    <div
      className={`pet-box${session ? " is-session" : ""}${still ? " is-still" : ""}`}
      style={{ left: rect.x, top: rect.y, width: rect.w, height: rect.h }}
      role="button"
      tabIndex={0}
      aria-label={label}
      aria-expanded={pressed}
      aria-haspopup="dialog"
      data-pet-hit
      data-pet-hit-leaf
      {...handlers}
    >
      <div className="pet-sprite">{children}</div>
      {name !== null && <div className="pet-name">{name}</div>}
      {badge > 0 && (
        <span className="pet-badge" aria-label={`${badge} waiting`}>
          {badge}
        </span>
      )}
      {mark && (
        <span className="pet-mark" aria-label="needs you">
          !
        </span>
      )}
    </div>
  );
}

/** The system's "reduce motion" wish (pets then stay still, even while chatting). */
function usePrefersReducedMotion(): boolean {
  const query = "(prefers-reduced-motion: reduce)";
  const [reduced, setReduced] = useState(() => typeof window !== "undefined" && !!window.matchMedia?.(query).matches);
  useEffect(() => {
    const mq = window.matchMedia?.(query);
    if (!mq) return;
    const on = () => setReduced(mq.matches);
    mq.addEventListener("change", on);
    return () => mq.removeEventListener("change", on);
  }, []);
  return reduced;
}

/** Shown only if a pet fails to render: a plain dog so the menu stays reachable. */
function FallbackDog({ rect, name, onMenu }: { rect: Rect; name: string; onMenu(): void }) {
  return (
    <div
      className="pet-box fallback-dog"
      style={{ left: rect.x, top: rect.y, width: rect.w, height: rect.h }}
      data-pet-hit
      title={`${name} (right-click for the menu)`}
      onContextMenu={(e) => {
        e.preventDefault();
        onMenu();
      }}
    >
      🐕
    </div>
  );
}
