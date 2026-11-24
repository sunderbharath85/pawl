// VS Code-only machine text (the shared machine helpers live in @pawl/hub-client).
import type { Machine } from "@pawl/protocol";

/**
 * Online state of a paired machine: "online", or "offline · last seen 14:05" (a clock time, or a date
 * when older than today, so a tooltip rendered once never goes stale).
 */
export function presenceLabel(machine: Pick<Machine, "online" | "lastSeen">, now = Date.now()): string {
  if (machine.online) return "online";
  if (!machine.lastSeen) return "offline · not seen yet";
  const seen = new Date(machine.lastSeen);
  const today = new Date(now).toDateString() === seen.toDateString();
  const when = today
    ? seen.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })
    : seen.toLocaleDateString([], { month: "short", day: "numeric" });
  return `offline · last seen ${when}`;
}
