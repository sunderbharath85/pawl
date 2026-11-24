// A headless Pawl phone for smoke tests (bun, no device). State persists in a JSON file so the same identity
// can be reused across runs. Never prints the device secret, invite codes or the relay access key.
//
//   bun scripts/headless-phone.ts --state /tmp/phone.json pair "<pawl://peer?... invite>"
//   bun scripts/headless-phone.ts --state /tmp/phone.json status            # hubs, sessions, pending approvals
//   bun scripts/headless-phone.ts --state /tmp/phone.json watch [secs]      # print changes for a while
//   bun scripts/headless-phone.ts --state /tmp/phone.json approve <requestId>   # sends confirmed: true (no biometrics here)
//   bun scripts/headless-phone.ts --state /tmp/phone.json deny <requestId>
//   bun scripts/headless-phone.ts --state /tmp/phone.json prompt <sessionId> <text>
//   bun scripts/headless-phone.ts --state /tmp/phone.json cancel <sessionId>
//   bun scripts/headless-phone.ts --state /tmp/phone.json start <machineId> <agentId> <cwd> [prompt]
//   bun scripts/headless-phone.ts --state /tmp/phone.json unpair
// Options: --name "Smoke phone", --os ios|android (default android), --wait <secs> (link wait, default 20).
import { existsSync, readFileSync, renameSync, writeFileSync } from "node:fs";
import { PhoneNode, phoneIntentErrorText, type KeyValueStore, type PhoneSnapshot } from "../src/lib/peer";

class JsonFileStore implements KeyValueStore {
  #data: Record<string, string>;
  constructor(readonly path: string) {
    this.#data = existsSync(path) ? (JSON.parse(readFileSync(path, "utf8")) as Record<string, string>) : {};
  }
  #save(): void {
    const tmp = `${this.path}.tmp`;
    writeFileSync(tmp, JSON.stringify(this.#data, null, 1), { mode: 0o600 });
    renameSync(tmp, this.path);
  }
  async get(key: string): Promise<string | null> {
    return this.#data[key] ?? null;
  }
  async set(key: string, value: string): Promise<void> {
    this.#data[key] = value;
    this.#save();
  }
  async delete(key: string): Promise<void> {
    delete this.#data[key];
    this.#save();
  }
}

function arg(name: string): string | undefined {
  const i = process.argv.indexOf(name);
  if (i < 0) return undefined;
  const v = process.argv[i + 1];
  process.argv.splice(i, 2);
  return v;
}

const statePath = arg("--state") ?? "headless-phone.json";
const name = arg("--name") ?? "Smoke phone";
const os = (arg("--os") ?? "android") as "ios" | "android";
const waitSecs = Number(arg("--wait") ?? "20");
const [cmd, ...rest] = process.argv.slice(2);

function summary(s: PhoneSnapshot): string {
  const lines: string[] = [];
  lines.push(`phone ${s.deviceId} "${s.name}" (${s.os}) relay: ${s.relay.url ?? "off"} ${s.relay.connected ? "connected" : s.relay.lastError ?? "connecting"}`);
  for (const h of s.hubs) {
    lines.push(`  hub ${h.id} "${h.name}" ${h.online ? "online (relay)" : "offline"} sync=${h.sync}${h.introducedBy ? ` introducedBy=${h.introducedBy}` : ""}`);
  }
  for (const x of Object.values(s.sessions)) lines.push(`  session ${x.id} ${x.agentId} ${x.status} ${x.cwd}`);
  for (const r of Object.values(s.pending)) lines.push(`  pending ${r.requestId} ${r.toolCall.title ?? r.toolCall.kind ?? "tool"}`);
  for (const r of s.resolutions.slice(0, 5)) lines.push(`  decided ${r.requestId} ${r.verdict} by=${r.by}${r.byMachineId ? ` from=${r.byMachineId}` : ""}`);
  for (const n of s.notices.slice(0, 5)) lines.push(`  notice [${n.level}] ${n.text}`);
  return lines.join("\n");
}

async function main(): Promise<void> {
  const phone = await PhoneNode.open({ store: new JsonFileStore(statePath), name, os });
  phone.start();
  const linked = () => phone.waitFor((s) => s.hubs.length > 0 && s.hubs.every((h) => h.online && h.sync === "synced"), waitSecs * 1000).catch(() => phone.getSnapshot());
  try {
    switch (cmd) {
      case "pair": {
        const hub = await phone.pair(rest[0] ?? "");
        console.log(`paired with ${hub.name} (${hub.id})`);
        await linked();
        console.log(summary(phone.getSnapshot()));
        break;
      }
      case "status":
        await linked();
        console.log(summary(phone.getSnapshot()));
        break;
      case "watch": {
        const secs = Number(rest[0] ?? "60");
        let last = "";
        phone.subscribe((s) => {
          const text = summary(s);
          if (text !== last) console.log(`--- ${new Date().toISOString()}\n${text}`);
          last = text;
        });
        phone.onEvent((e) => {
          if (e.type === "permission_request") console.log(`>> new approval ${e.request.requestId}`);
          if (e.type === "link") console.log(`>> link ${e.hubId} ${e.up ? "up" : "down"}`);
        });
        await new Promise((r) => setTimeout(r, secs * 1000));
        break;
      }
      case "approve":
      case "deny":
      case "prompt":
      case "cancel":
      case "start": {
        await linked();
        const id = rest[0] ?? "";
        const res =
          cmd === "approve"
            ? await phone.approve(id, { confirmed: true })
            : cmd === "deny"
              ? await phone.deny(id)
              : cmd === "prompt"
                ? await phone.prompt(id, rest.slice(1).join(" "))
                : cmd === "cancel"
                  ? await phone.cancel(id)
                  : await phone.startSession({ machineId: id, agentId: rest[1] ?? "", cwd: rest[2] ?? "", prompt: rest.slice(3).join(" "), confirmed: true });
        console.log(`${cmd}: applied`, JSON.stringify(res.result ?? {}));
        break;
      }
      case "unpair":
        await linked();
        await phone.unpairPhone();
        console.log("unpaired; this state file now holds a fresh identity");
        break;
      default:
        console.log("usage: bun scripts/headless-phone.ts --state <file> pair|status|watch|approve|deny|prompt|cancel|start|unpair ...");
    }
  } catch (e) {
    const code = (e as { code?: string }).code ?? "";
    const intentCmd = ["approve", "deny", "prompt", "cancel", "start"].includes(cmd ?? "");
    const machine = rest[0]?.split(":")[0] ?? "the hub";
    const text = intentCmd && code ? ` — ${phoneIntentErrorText(e, machine, cmd as "approve")}` : "";
    console.error(`${cmd} failed: ${(e as Error).message || code}${text}`);
    process.exitCode = 1;
  } finally {
    await phone.stop();
  }
}

await main();
