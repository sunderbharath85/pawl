// OS notifications (tauri-plugin-notification) for permission requests, turn
// completions and hub `notification` messages. Attached once, in the pet window.
import { isPermissionGranted, requestPermission, sendNotification } from "@tauri-apps/plugin-notification";
import type { HubClient } from "@krypto/hub-client";
import { SPECIES_META } from "@krypto/pet-engine";
import type { StopPayload } from "@krypto/protocol";
import { isTauri } from "./native";
import { readSettings } from "./settings";
import { agentName, permissionSummary, sessionTitle } from "./format";
import { petFor } from "./machine";

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

/** Wire hub messages → OS notifications. Returns an unsubscribe function. */
export function attachNotifications(client: HubClient): () => void {
  void ensurePermission();

  const offPermission = client.onMessage("permission_request", ({ request }) => {
    if (!readSettings().notifyPermissions || !once(`perm:${request.requestId}`)) return;
    const state = client.getState();
    const pet = petFor(Object.values(state.pets), request.machineId, request.agentId);
    const sound = SPECIES_META[pet?.species ?? "dog"]?.sound ?? "Woof!";
    const who = pet?.name ?? "Your pet";
    void notify(`${sound} ${who} needs you`, `${agentName(state.agents, request.agentId, request.machineId)} wants to: ${permissionSummary(request)}`);
  });

  const offEvent = client.onMessage("event", ({ event }) => {
    if (!readSettings().notifyTurns) return;
    const state = client.getState();
    const session = state.sessions[event.sessionId];
    // Hook-fed sessions announce themselves through hub `notification` messages instead.
    if (!session || session.source !== "owned") return;
    const agent = agentName(state.agents, session.agentId, session.machineId);
    const pet = petFor(Object.values(state.pets), session.machineId, session.agentId);
    const name = pet ? `${pet.name} (${agent})` : agent;
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
    if (!readSettings().notifyHub || !once(`hub:${notification.id}`)) return;
    void notify(notification.title, notification.body);
  });

  return () => {
    offPermission();
    offEvent();
    offNotification();
  };
}
