// The pet window: ONE pet, the default pet (it orchestrates every agent and mirrors the most important state across
// all sessions and machines), and its chat bubble. Sessions never get pets of their own on screen (not this
// machine's, not a paired machine's): the bubble's switcher talks to one session at a time.
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
import { HOLD_MS, layoutPetWindow, petFrame, placeBubble, PressTracker, type PetSpace, type Rect } from "../lib/petLayout";
import { autoOpenBubble, clickBubble, DEFAULT_TARGET as DEFAULT, setupLooksDone, type OpenBubble } from "../lib/bubble";
import { chatPhase, petPose, sessionPhase, type ChatPhase } from "../lib/petMotion";
import { defaultPetState, newRequestIds, switcherSessions } from "../lib/petScreen";
import { usePetViews } from "../lib/pets";
import { useSettings } from "../lib/settings";
import { useSprites } from "../lib/sprites";
import { useConnection } from "../lib/useConnection";
import { hubPetName, renameDefaultPet, useDefaultPetName } from "../lib/usePetName";
import type { MicMarker as MicMark } from "../lib/voice";
import { useVoiceTalk } from "./useVoiceTalk";
import { MicMarker } from "./VoiceBits";
import { useNow } from "../lib/useNow";
import { notifiesHere } from "../lib/where";
import { DefaultBubble, SayBubble, SleepyBubble, type BubbleEnv } from "./Bubble";
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
  // The session the bubble talks to (null: Pawl-wide, the orchestrator chat), and the sessions it can switch to.
  const [focusId, setFocusId] = useState<string | null>(null);
  const switcher = useMemo(() => (connected ? switcherSessions(state, now, { focus: focusId }) : { chips: [], more: [] }), [connected, state, now, focusId]);
  const focus = focusId ? (switcher.chips.find((c) => c.sessionId === focusId) ?? null) : null;
  // A session that ended or went out of reach: back to Pawl-wide.
  useEffect(() => {
    if (focusId && connected && !focus) setFocusId(null);
  }, [focusId, connected, focus]);
  const layout = useMemo(
    () => layoutPetWindow({ petSize: settings.petSize, showNames: settings.showPetNames, space }),
    [settings.petSize, settings.showPetNames, space],
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

  // A new approval opens the bubble by itself (where the user is, like notifications), Pawl-wide (it lists every
  // approval) unless the approval is the focused session's. It never replaces an open bubble: the user may be typing
  // there; the badge on the pet and the switcher's chip show the approval instead.
  const seen = useRef<Set<string>>(new Set());
  const openNow = useRef(open);
  openNow.current = open;
  useEffect(() => {
    const fresh = newRequestIds(seen.current, pending);
    seen.current = new Set(pending.map((p) => p.requestId));
    if (fresh.length === 0 || !notifiesHere(client.getState())) return;
    const req = pending.find((p) => p.requestId === fresh[0]);
    if (!openNow.current && req?.sessionId !== focusId) setFocusId(null);
    setOpen((o) => autoOpenBubble(o, DEFAULT));
  }, [pending, client, focusId]);

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

  // Esc or clicking outside (the window loses focus) closes the bubble.
  useEffect(() => {
    const onKey = (e: globalThis.KeyboardEvent) => {
      if (e.key !== "Escape") return;
      // While talking, Esc drops what was said (nothing is sent); otherwise it closes the bubble.
      if (voiceNow.current.phase === "listening") voiceNow.current.cancel();
      else setOpen(null);
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

  // Voice (Settings > Voice): wake word, push-to-talk, transcripts to the chat, replies read aloud.
  const voice = useVoiceTalk({
    client,
    state,
    now,
    hubName: hubPetName(state),
    bubbleOpen: open?.target === DEFAULT,
    // What is said goes to the orchestrator chat: the bubble shows it (Pawl-wide), whatever session was picked.
    openBubble: () => {
      setFocusId(null);
      setOpen((o) => (o?.target === DEFAULT ? o : { target: DEFAULT, by: "auto" }));
    },
    say,
    chatSent,
    speech: { voiceURI: settings.speechVoice, rate: settings.speechRate },
  });
  const voiceNow = useRef(voice);
  voiceNow.current = voice;
  const voiceOn = voice.enabled;
  const env: BubbleEnv = useMemo(
    () => ({ client, state, now, connected, say, dismiss, chatSent, promptSent, voiceOn }),
    [client, state, now, connected, say, dismiss, chatSent, promptSent, voiceOn],
  );

  const toggle = useCallback((target: string) => setOpen((o) => clickBubble(o, target)), []);
  const menuMuted = voice.menuMuted;
  const menu = useCallback(() => {
    native.petContextMenu(name, canRename, menuMuted).catch((err) => setNote(errorText(err)));
  }, [name, canRename, menuMuted]);
  const rename = useCallback((raw: string) => renameDefaultPet(client, state, raw), [client, state]);

  // Press: a click toggles the bubble, moving 4 px drags the pet, and (voice on) holding the default pet still for
  // 400 ms talks until the release.
  const press = useRef(new PressTracker());
  const holdTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const holding = useRef(false);
  const endHold = useCallback(() => {
    if (holdTimer.current) clearTimeout(holdTimer.current);
    holdTimer.current = null;
    if (!holding.current) return;
    holding.current = false;
    voiceNow.current.stop();
  }, []);
  const petHandlers = (): PetHandlers => ({
    onPointerDown: (e: PointerEvent<HTMLDivElement>) => {
      if (e.button !== 0) return;
      press.current.down(e.screenX, e.screenY, Date.now());
      if (!voiceNow.current.ready) return;
      holdTimer.current = setTimeout(() => {
        holdTimer.current = null;
        if (!press.current.hold(Date.now())) return;
        holding.current = true;
        voiceNow.current.start();
        // The release may happen off the pet.
        window.addEventListener("pointerup", endHold, { once: true });
      }, HOLD_MS);
    },
    onPointerMove: (e: PointerEvent<HTMLDivElement>) => {
      if ((e.buttons & 1) === 0) return;
      if (press.current.move(e.screenX, e.screenY)) {
        if (holdTimer.current) clearTimeout(holdTimer.current);
        holdTimer.current = null;
        native.petStartDrag().catch((err) => console.warn("[desktop] drag failed", err));
      }
    },
    onPointerUp: () => {
      press.current.up();
      endHold();
    },
    onClick: (e: MouseEvent<HTMLDivElement>) => {
      // A drag is not a click; the second click of a double-click does nothing (double-click opens nothing).
      if (!press.current.click() || e.detail > 1) return;
      toggle(DEFAULT);
    },
    onContextMenu: (e: MouseEvent<HTMLDivElement>) => {
      e.preventDefault();
      menu();
    },
    onKeyDown: (e: KeyboardEvent<HTMLDivElement>) => {
      if (e.key === "Enter" || e.key === " ") {
        e.preventDefault();
        toggle(DEFAULT);
      } else if (e.key === "ContextMenu" || (e.key === "F10" && e.shiftKey)) {
        e.preventDefault();
        menu();
      }
    },
  });

  const facing = space && space.right < space.left ? "left" : "right";
  // Still by default: a pet moves only while the person chats with it in its bubble (Settings > Pet animation).
  const defaultChat = useMemo(() => state.chat.filter((m) => !m.petId), [state.chat]);
  // The pet moves while a conversation is going on: the orchestrator chat, or a session prompted from the bubble.
  let phase: ChatPhase = chatPhase(defaultChat, sentChat, now);
  for (const [sessionId, at] of Object.entries(sentPrompts)) {
    if (phase !== "idle") break;
    phase = sessionPhase(state.events[sessionId] ?? [], at, now);
  }
  const mainPose = petPose({
    mode: settings.petAnimation,
    reducedMotion,
    agentActivity: connected ? main.activity : sleepyActivity(meta.phase),
    // Talking to it counts as chatting: it thinks while it listens and transcribes, then still again.
    phase: voice.phase ? "waiting" : phase,
  });
  const mainActivity = mainPose.activity;
  const needsYou = connected && main.mood === "needs_you";
  const place = placeBubble(layout, layout.main);

  let bubble = null;
  if (open && !connected && open.target === DEFAULT) {
    bubble = <SleepyBubble meta={meta} place={place} windowHeight={layout.height} name={name} onWake={() => void conn.startHub().catch((err) => setNote(errorText(err)))} onClose={() => setOpen(null)} />;
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
        switcher={switcher}
        focus={focus}
        onFocus={setFocusId}
        renaming={renaming}
        onRename={rename}
        onRenameDone={() => setRenaming(false)}
        voice={{
          marker: voice.marker,
          phase: voice.phase,
          level: voice.level,
          enabled: voice.enabled,
          unavailable: voice.unavailable,
          onMic: () => (voice.phase === "listening" ? voice.stop() : voice.start()),
          onCancel: voice.cancel,
        }}
        onClose={() => setOpen(null)}
      />
    );
  } else if (note) {
    bubble = <SayBubble place={place} windowHeight={layout.height} text={note} onClick={() => setNote(null)} />;
  }

  const badge = open?.target !== DEFAULT ? pending.length : 0;
  const mainLabel = `${name}: ${connected ? main.line : "the hub is not connected"}`;

  return (
    <div ref={rootRef} className={`pet-root${hover ? " is-hover" : ""}`} style={{ width: layout.width, height: layout.height }}>
      <ErrorBoundary resetKey={`${name}:${mainActivity}`} fallback={<FallbackDog rect={layout.main} name={name} onMenu={menu} />}>
        <PetBox
          rect={layout.main}
          label={mainLabel}
          name={settings.showPetNames ? name : null}
          badge={badge}
          mark={needsYou && badge === 0}
          still={!mainPose.animate}
          pressed={open?.target === DEFAULT}
          handlers={petHandlers()}
          mic={voice.marker}
        >
          <PetSprite species={settings.defaultPetSpecies} activity={mainActivity} size={layout.main.w} facing={facing} sprites={sprites} title={mainLabel} />
        </PetBox>
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
  /** Voice: a static mic marker while the mic is live, a crossed one while muted. */
  mic?: MicMark;
  /** Drawn without any animation. */
  still?: boolean;
  pressed: boolean;
  handlers: PetHandlers;
  children: ReactNode;
}

function PetBox({ rect, label, name, badge, mark, mic, still, pressed, handlers, children }: PetBoxProps) {
  return (
    <div
      className={`pet-box${still ? " is-still" : ""}`}
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
      <MicMarker marker={mic ?? null} where="pet" />
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
