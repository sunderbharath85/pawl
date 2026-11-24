// OS notifications (tauri-plugin-notification) for permission requests, turn
// completions and hub `notification` messages of every machine. Attached once, in the pet
// window. Only the machine the user is active on speaks up, so they are told once, where they are.
import { isPermissionGranted, requestPermission, sendNotification } from "@tauri-apps/plugin-notification";
import { petFor, type HubClient } from "@pawl/hub-client";
import { SPECIES_META } from "@pawl/pet-engine";
import type { StopPayload } from "@pawl/protocol";
import { isTauri } from "./native";
import { readSettings } from "./settings";
import { agentName, permissionSummary, sessionTitle } from "./format";
import { notifiesHere, onMachine } from "./where";

let permission: Promise<boolean> | null = null;

function ensurePermission(): Promise<boolean> {
  if (!isTauri) return Promise.resolve(false);
  if (!permission) {
    permission = (async () => {
      try {
        if (await isPermissionGranted()) return true;
        return (await requestPermission()) === "granted";
      } catch (err) {
        console.warn("[desktop] notification permission check failed", err);
        return false;
      }
    })();
  }
  return permission;
}

const seen = new Set<string>();
function once(key: string): boolean {
  if (seen.has(key)) return false;
  seen.add(key);
  if (seen.size > 500) {
    const first = seen.values().next().value;
    if (first !== undefined) seen.delete(first);
  }
  return true;
}

export async function notify(title: string, body: string): Promise<void> {
  if (!(await ensurePermission())) return;
  try {
    sendNotification({ title, body });
  } catch (err) {
    console.warn("[desktop] notification failed", err);
  }
}

/** Events older than this are history (a mirror sync after reconnecting), not news. */
const FRESH_MS = 2 * 60_000;


/** Wire hub messages → OS notifications. Returns an unsubscribe function. */
export function attachNotifications(client: HubClient): () => void {
  void ensurePermission();

  const offPermission = client.onMessage("permission_request", ({ request }) => {
    const state = client.getState();
    if (!readSettings().notifyPermissions || !notifiesHere(state) || !once(`perm:${request.requestId}`)) return;
    const pet = petFor(Object.values(state.pets), request.machineId, request.agentId);
    const sound = SPECIES_META[pet?.species ?? "dog"]?.sound ?? "Woof!";
    const who = pet?.name ?? "Your pet";
    const where = onMachine(state, request.machineId);
    void notify(
      `${sound} ${who} needs you${where}`,
      `${agentName(state.agents, request.agentId, request.machineId)} wants to: ${permissionSummary(request)}.`,
    );
  });

  const offEvent = client.onMessage("event", ({ event }) => {
    if (!readSettings().notifyTurns) return;
    if (Date.now() - event.ts > FRESH_MS) return;
    const state = client.getState();
    const session = state.sessions[event.sessionId];
    // Hook-fed sessions announce themselves through hub `notification` messages instead.
    if (!session || session.source !== "owned" || !notifiesHere(state)) return;
    const agent = agentName(state.agents, session.agentId, session.machineId);
    const pet = petFor(Object.values(state.pets), session.machineId, session.agentId);
    const name = `${pet ? `${pet.name} (${agent})` : agent}${onMachine(state, session.machineId)}`;
    if (event.type === "stop") {
      const reason = (event.payload as Partial<StopPayload> | null)?.stopReason;
      if (reason === "cancelled" || !once(`stop:${event.id}`)) return;
      if (reason === "end_turn") void notify(`${name} finished`, sessionTitle(session, state.agents));
      else void notify(`${name} stopped (${reason ?? "unknown"})`, sessionTitle(session, state.agents));
    } else if (event.type === "error" && once(`err:${event.id}`)) {
      const msg = (event.payload as { message?: string } | null)?.message ?? "Something went wrong";
      void notify(`${name} hit a snag`, msg);
    }
  });

  const offNotification = client.onMessage("notification", ({ notification }) => {
    const state = client.getState();
    if (Date.now() - notification.ts > FRESH_MS || !notifiesHere(state)) return;
    if (!readSettings().notifyHub || !once(`hub:${notification.id}`)) return;
    const where = onMachine(state, notification.machineId);
    void notify(where ? `${notification.title} (${where.slice(4)})` : notification.title, notification.body);
  });

  return () => {
    offPermission();
    offEvent();
    offNotification();
  };
}
