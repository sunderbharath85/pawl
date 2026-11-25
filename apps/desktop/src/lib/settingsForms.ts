// Settings view forms: hub settings ⇄ editable form values (strings while typing), with the client-side checks
// that mirror the hub's validation. Errors are keyed by the same camelCase field paths the hub uses in
// `invalid:<field>: <reason>`, so the hub's field errors land on the same inputs. Pure: no React, no DOM.
import {
  checkCustomAgent,
  checkFolder,
  checkListenAddr,
  checkPort,
  checkReportTime,
  checkUrl,
  checkWholeNumber,
  CUSTOM_AGENTS_MAX,
  editableSettings,
  formatCommandLine,
  ACP_BOT_AGENTS,
  isBotProvider,
  isBotStyle,
  normalizeReportTime,
  parseCommandLine,
  parsePositiveDecimal,
  parseWholeNumber,
  settingsPatch,
  type EditableSettings,
} from "@pawl/hub-client";
import type { BotProvider, BotStyle, CustomAgentSetting, HubSettings, HubSettingsPatch } from "@pawl/protocol";

/** Field path → message. */
export type FormErrors = Record<string, string>;

export interface FormResult {
  /** Only the changed keys (empty when nothing changed or when there are errors). */
  patch: HubSettingsPatch;
  errors: FormErrors;
}

function finish(s: HubSettings, errors: FormErrors, edit: (d: EditableSettings) => void): FormResult {
  if (Object.keys(errors).length > 0) return { patch: {}, errors };
  const d = editableSettings(s);
  edit(d);
  return { patch: settingsPatch(s, d), errors };
}

function put(errors: FormErrors, key: string, problem: string | null): void {
  if (problem) errors[key] = problem;
}

// ---------------------------------------------------------------- Hub

/** Bounds the form allows (the hub has the final say). 0 = never / keep forever where noted. */
export const HUB_LIMITS = {
  hookSessionStaleHours: [1, 8760],
  ownedSessionIdleHours: [0, 8760],
  eventRetentionDays: [0, 3650],
} as const;

export interface HubForm {
  port: string;
  offline: boolean;
  hookSessionStaleHours: string;
  ownedSessionIdleHours: string;
  eventRetentionDays: string;
}

export function hubForm(s: HubSettings): HubForm {
  return {
    port: String(s.hub.port),
    offline: s.hub.offline,
    hookSessionStaleHours: String(s.hub.hookSessionStaleHours),
    ownedSessionIdleHours: String(s.hub.ownedSessionIdleHours),
    eventRetentionDays: String(s.hub.eventRetentionDays),
  };
}

export function hubPatch(f: HubForm, s: HubSettings): FormResult {
  const errors: FormErrors = {};
  put(errors, "hub.port", checkPort(f.port));
  for (const k of ["hookSessionStaleHours", "ownedSessionIdleHours", "eventRetentionDays"] as const) {
    const [min, max] = HUB_LIMITS[k];
    put(errors, `hub.${k}`, checkWholeNumber(f[k], min, max, k === "eventRetentionDays" ? "days" : "hours"));
  }
  return finish(s, errors, (d) => {
    d.hub.port = parseWholeNumber(f.port)!;
    d.hub.offline = f.offline;
    d.hub.hookSessionStaleHours = parseWholeNumber(f.hookSessionStaleHours)!;
    d.hub.ownedSessionIdleHours = parseWholeNumber(f.ownedSessionIdleHours)!;
    d.hub.eventRetentionDays = parseWholeNumber(f.eventRetentionDays)!;
  });
}

// ---------------------------------------------------------------- Nudges & reports

export const NUDGE_MAX_MINUTES = 24 * 60;
export const TICK_LIMITS = [1, 3600] as const;

/** ms → minutes text without trailing zeros (300000 → "5", 90000 → "1.5"). */
export function msToMinutes(ms: number): string {
  return String(Math.round((ms / 60_000) * 100) / 100);
}

export interface NudgesForm {
  reportOn: boolean;
  reportTime: string;
  nudgeMinutes: string;
  repeatMinutes: string;
  tickSecs: string;
}

/** Shown in the time field when the daily report is off (and used when it is turned back on). */
export const DEFAULT_REPORT_TIME = "18:00";

export function nudgesForm(s: HubSettings): NudgesForm {
  const t = s.hub.dailyReportTime.trim();
  return {
    reportOn: t !== "",
    reportTime: t || DEFAULT_REPORT_TIME,
    nudgeMinutes: msToMinutes(s.watchdog.waitingNudgeAfterMs),
    repeatMinutes: msToMinutes(s.watchdog.repeatEveryMs),
    tickSecs: String(s.watchdog.tickSecs),
  };
}

function checkMinutes(raw: string): string | null {
  const n = parsePositiveDecimal(raw);
  if (n === null) return "Enter a number of minutes, e.g. 5 or 1.5.";
  if (n > NUDGE_MAX_MINUTES) return `Use at most ${NUDGE_MAX_MINUTES} minutes (a day).`;
  return null;
}

export function nudgesPatch(f: NudgesForm, s: HubSettings): FormResult {
  const errors: FormErrors = {};
  if (f.reportOn) put(errors, "hub.dailyReportTime", f.reportTime.trim() ? checkReportTime(f.reportTime) : "Pick a time, e.g. 18:00.");
  put(errors, "watchdog.waitingNudgeAfterMs", checkMinutes(f.nudgeMinutes));
  put(errors, "watchdog.repeatEveryMs", checkMinutes(f.repeatMinutes));
  put(errors, "watchdog.tickSecs", checkWholeNumber(f.tickSecs, TICK_LIMITS[0], TICK_LIMITS[1], "seconds"));
  return finish(s, errors, (d) => {
    d.hub.dailyReportTime = f.reportOn ? normalizeReportTime(f.reportTime) : "";
    const nudge = Math.round(parsePositiveDecimal(f.nudgeMinutes)! * 60_000);
    const repeat = Math.round(parsePositiveDecimal(f.repeatMinutes)! * 60_000);
    // Unchanged text keeps the exact stored value (minutes are rounded for display).
    if (f.nudgeMinutes.trim() !== msToMinutes(s.watchdog.waitingNudgeAfterMs)) d.watchdog.waitingNudgeAfterMs = nudge;
    if (f.repeatMinutes.trim() !== msToMinutes(s.watchdog.repeatEveryMs)) d.watchdog.repeatEveryMs = repeat;
    d.watchdog.tickSecs = parseWholeNumber(f.tickSecs)!;
  });
}

// ---------------------------------------------------------------- Bot

export interface BotForm {
  model: string;
  ollamaUrl: string;
  ollamaModel: string;
  /** `bot.defaultAgent`: "" = Automatic; null when the hub predates the setting (the picker is hidden). */
  defaultAgent: string | null;
  /**
   * The provider fields; null when the hub predates them (those controls are hidden): `provider`, the OpenAI /
   * Gemini models, `understand`, and for provider "acp" the agent that thinks for the pet and its model.
   */
  provider: BotProvider | null;
  openaiModel: string | null;
  geminiModel: string | null;
  understand: boolean | null;
  acpAgent: string | null;
  acpModel: string | null;
  /** How Pawl talks ("plain" | "playful"); null when the hub predates it (the choice is hidden). */
  style: BotStyle | null;
}

function orNull<T>(v: T | undefined, ok: (v: unknown) => boolean): T | null {
  return v !== undefined && ok(v) ? v : null;
}
const isString = (v: unknown) => typeof v === "string";

export function botForm(s: HubSettings): BotForm {
  const agent = s.bot.defaultAgent;
  return {
    model: s.bot.model,
    ollamaUrl: s.bot.ollamaUrl ?? "",
    ollamaModel: s.bot.ollamaModel ?? "",
    defaultAgent: agent === undefined ? null : (agent ?? ""),
    provider: orNull(s.bot.provider, isBotProvider),
    openaiModel: orNull(s.bot.openaiModel, isString),
    geminiModel: orNull(s.bot.geminiModel, isString),
    understand: orNull(s.bot.understand, (v) => typeof v === "boolean"),
    acpAgent: orNull(s.bot.acpAgent, isString),
    acpModel: orNull(s.bot.acpModel, isString),
    style: orNull(s.bot.style, isBotStyle),
  };
}

export function botPatch(f: BotForm, s: HubSettings): FormResult {
  const errors: FormErrors = {};
  if (!f.model.trim()) errors["bot.model"] = "Enter a model id, e.g. claude-haiku-5-5.";
  else if (/\s/.test(f.model.trim())) errors["bot.model"] = "A model id has no spaces.";
  if (f.ollamaUrl.trim()) put(errors, "bot.ollamaUrl", checkUrl(f.ollamaUrl, ["http", "https"]));
  if (/\s/.test(f.ollamaModel.trim())) errors["bot.ollamaModel"] = "A model name has no spaces.";
  for (const [key, v] of [
    ["bot.openaiModel", f.openaiModel],
    ["bot.geminiModel", f.geminiModel],
    ["bot.acpModel", f.acpModel],
  ] as const) {
    if (typeof v === "string" && /\s/.test(v.trim())) errors[key] = "A model name has no spaces.";
  }
  if (f.provider === "acp" && typeof f.acpAgent === "string") {
    if (!f.acpAgent.trim()) errors["bot.acpAgent"] = "Pick the agent that thinks for your pet.";
    else if (!ACP_BOT_AGENTS.includes(f.acpAgent.trim())) errors["bot.acpAgent"] = "Only Claude Code can think for your pet for now.";
  }
  return finish(s, errors, (d) => {
    d.bot.model = f.model.trim();
    d.bot.ollamaUrl = f.ollamaUrl.trim() || null;
    d.bot.ollamaModel = f.ollamaModel.trim() || null;
    if (f.defaultAgent !== null) d.bot.defaultAgent = f.defaultAgent || null;
    if (f.provider !== null) d.bot.provider = f.provider;
    if (f.openaiModel !== null) d.bot.openaiModel = f.openaiModel.trim();
    if (f.geminiModel !== null) d.bot.geminiModel = f.geminiModel.trim();
    if (f.understand !== null) d.bot.understand = f.understand;
    if (f.acpAgent !== null && f.acpAgent.trim()) d.bot.acpAgent = f.acpAgent.trim();
    if (f.acpModel !== null) d.bot.acpModel = f.acpModel.trim();
    if (f.style !== null) d.bot.style = f.style;
  });
}

// ---------------------------------------------------------------- Machines & pairing

export interface PairingForm {
  on: boolean;
  listen: string;
}

export function pairingForm(s: HubSettings): PairingForm {
  return { on: s.peer.listen !== null, listen: s.peer.listen ?? "" };
}

export function pairingPatch(f: PairingForm, s: HubSettings): FormResult {
  const errors: FormErrors = {};
  if (f.on) put(errors, "peer.listen", checkListenAddr(f.listen));
  return finish(s, errors, (d) => {
    d.peer.listen = f.on ? f.listen.trim() : null;
  });
}

/** Devices introduced by paired hubs (group roster, step 7). Null when the hub predates the setting (protocol < 8). */
export function introsForm(s: HubSettings): { accept: boolean } | null {
  const v = (s.peer as Partial<HubSettings["peer"]>).acceptRosterIntros;
  return typeof v === "boolean" ? { accept: v } : null;
}

export function introsPatch(f: { accept: boolean }, s: HubSettings): FormResult {
  return finish(s, {}, (d) => {
    d.peer.acceptRosterIntros = f.accept;
  });
}

// ---------------------------------------------------------------- folder lists (start roots, repos)

/** Check every folder of a list; errors keyed `<key>[i]`. */
export function folderListErrors(key: string, folders: readonly string[]): FormErrors {
  const errors: FormErrors = {};
  folders.forEach((f, i) => put(errors, `${key}[${i}]`, checkFolder(f, folders.slice(0, i))));
  return errors;
}

export interface RemoteForm {
  acceptIntents: boolean;
  startRoots: string[];
}

export function remoteForm(s: HubSettings): RemoteForm {
  return { acceptIntents: s.peer.acceptIntents, startRoots: [...s.peer.startRoots] };
}

export function remotePatch(f: RemoteForm, s: HubSettings): FormResult {
  const roots = f.startRoots.map((r) => r.trim());
  return finish(s, folderListErrors("peer.startRoots", roots), (d) => {
    d.peer.acceptIntents = f.acceptIntents;
    d.peer.startRoots = roots;
  });
}

export function reposPatch(paths: readonly string[], s: HubSettings): FormResult {
  const list = paths.map((p) => p.trim());
  return finish(s, folderListErrors("repos.paths", list), (d) => {
    d.repos.paths = list;
  });
}

// ---------------------------------------------------------------- Relay

/** Relay URL: wss:// or https:// for any host; ws:// or http:// only for this computer (local testing). */
export function checkRelayUrl(raw: string): string | null {
  const problem = checkUrl(raw, ["wss", "https", "ws", "http"]);
  if (problem) return "Enter the relay URL, e.g. wss://relay.example.dev.";
  const m = /^([a-z]+):\/\/(\[[^\]]+\]|[^/:?#]+)/i.exec(raw.trim())!;
  const secure = /^(wss|https)$/i.test(m[1]!);
  const host = m[2]!.toLowerCase();
  const loopback = host === "localhost" || host === "[::1]" || /^127\./.test(host);
  if (!secure && !loopback) return "Use wss:// (ws:// only works for a relay on this computer).";
  if (/[?#]/.test(raw) || /^[a-z]+:\/\/[^/]*@/i.test(raw.trim())) return "Leave out any ?query, #fragment or user name.";
  return null;
}

export function relayPatch(url: string, s: HubSettings): FormResult {
  const errors: FormErrors = {};
  if (url.trim()) put(errors, "relay.url", checkRelayUrl(url));
  return finish(s, errors, (d) => {
    d.relay.url = url.trim() || null;
  });
}

// ---------------------------------------------------------------- Custom agents

export interface AgentDraft {
  id: string;
  name: string;
  commandLine: string;
}

export function agentDraft(a?: CustomAgentSetting): AgentDraft {
  return a ? { id: a.id, name: a.name, commandLine: formatCommandLine(a.command) } : { id: "", name: "", commandLine: "" };
}

/** A lowercase id from a name ("My Agent 2" → "my-agent-2"), for prefilling the id field. */
export function suggestAgentId(name: string): string {
  return name
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .slice(0, 40)
    .replace(/-+$/g, "");
}

/**
 * The custom agent list with `draft` added (`index` null) or replacing item `index`, or the problems. Errors are
 * keyed `customAgents[i].id|name|command` (i = the item's position in the new list).
 */
export function applyAgentDraft(
  list: readonly CustomAgentSetting[],
  index: number | null,
  draft: AgentDraft,
  builtinIds: readonly string[],
): { list: CustomAgentSetting[] | null; errors: FormErrors } {
  const at = index ?? list.length;
  const errors: FormErrors = {};
  if (index === null && list.length >= CUSTOM_AGENTS_MAX) {
    errors["customAgents"] = `You can add up to ${CUSTOM_AGENTS_MAX} custom agents.`;
    return { list: null, errors };
  }
  const p = checkCustomAgent(draft, list, index, builtinIds);
  put(errors, `customAgents[${at}].id`, p.id);
  put(errors, `customAgents[${at}].name`, p.name);
  put(errors, `customAgents[${at}].command`, p.command);
  if (Object.keys(errors).length > 0) return { list: null, errors };
  const agent: CustomAgentSetting = { id: draft.id.trim(), name: draft.name.trim(), command: parseCommandLine(draft.commandLine) };
  const next = list.map((a) => ({ ...a, command: [...a.command] }));
  if (index === null) next.push(agent);
  else next[index] = agent;
  return { list: next, errors };
}

export function customAgentsPatch(list: readonly CustomAgentSetting[], s: HubSettings): HubSettingsPatch {
  return finish(s, {}, (d) => {
    d.customAgents = list.map((a) => ({ ...a, command: [...a.command] }));
  }).patch;
}
