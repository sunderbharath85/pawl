// Browser use (v0.2 item 5), the pure part: Settings > Browser (off by default; sites a task may open without asking),
// browser sessions, and the confirm wording. The pet drives the user's own Chrome through Chrome's auto-connect
// (Chrome 144+): Chrome asks to Allow each connection and shows its banner; every click and every typing asks in Pawl.
import type { HubSettings, HubSettingsPatch, Session } from "@pawl/protocol";
import type { RiskyChange } from "./settings";

/** Chrome's page for turning on remote debugging (it can't be opened from another app: the user pastes it). */
export const CHROME_DEBUG_URL = "chrome://inspect/#remote-debugging";
export const CHROME_MIN_VERSION = 144;
export const ALLOW_SITES_MAX = 50;

export function browserOf(s: HubSettings | null | undefined): { enabled: boolean; allowSites: string[] } | null {
  return s?.browser ?? null;
}

export function isBrowserSession(s: Pick<Session, "purpose"> | null | undefined): boolean {
  return s?.purpose === "browser";
}

/**
 * A site for the allow list, as an origin (what the hub stores): "github.com" -> "https://github.com"; a pasted URL
 * keeps only its origin ("https://github.com/acme/web/pull/42" -> "https://github.com"); a default port is dropped.
 * Plain http and other ports must be written out, and get a warning. No wildcards: each site is listed.
 */
export function parseAllowSite(raw: string): { ok: true; origin: string; warning: string | null } | { ok: false; error: string } {
  const t = raw.trim();
  if (!t) return { ok: false, error: "Type a site, like github.com." };
  if (t.includes("*")) return { ok: false, error: "List each site on its own (no wildcards), like docs.github.com." };
  const withScheme = /^[a-z][a-z0-9+.-]*:\/\//i.test(t) ? t : `https://${t}`;
  let u: URL;
  try {
    u = new URL(withScheme);
  } catch {
    return { ok: false, error: "That is not a site. Use a host name like docs.github.com." };
  }
  if (u.protocol !== "https:" && u.protocol !== "http:") return { ok: false, error: "Only https:// (or http://) sites." };
  if (u.username || u.password) return { ok: false, error: "Leave the user name and password out." };
  const host = u.hostname;
  const ip = /^\d{1,3}(?:\.\d{1,3}){3}$/.test(host) || host.startsWith("[");
  if (host !== "localhost" && !ip && !/^(?=.{1,253}$)([a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?\.)+[a-z][a-z0-9-]{0,62}$/.test(host)) {
    return { ok: false, error: "That is not a site. Use a host name like docs.github.com." };
  }
  const warnings: string[] = [];
  if (u.protocol === "http:") warnings.push("plain http: the pages are not encrypted, so others on the network could see or change them");
  if (u.port) warnings.push(`port ${u.port}: not the usual web port, so make sure this is the site you mean`);
  return { ok: true, origin: u.origin, warning: warnings.length ? `${u.origin} uses ${warnings.join("; and ")}.` : null };
}

export function browserPatch(base: { enabled: boolean; allowSites: string[] }, next: { enabled?: boolean; allowSites?: string[] }): HubSettingsPatch {
  const browser: NonNullable<HubSettingsPatch["browser"]> = {};
  if (next.enabled !== undefined && next.enabled !== base.enabled) browser.enabled = next.enabled;
  if (next.allowSites !== undefined && JSON.stringify(next.allowSites) !== JSON.stringify(base.allowSites)) browser.allowSites = [...next.allowSites];
  return Object.keys(browser).length ? { browser } : {};
}

/** Turning browser use on, and letting more sites open without asking, need a confirm. */
export function browserRiskyChanges(base: HubSettings, patch: HubSettingsPatch): RiskyChange[] {
  const p = patch.browser;
  const cur = base.browser;
  if (!p || !cur) return [];
  const out: RiskyChange[] = [];
  if (p.enabled === true && !cur.enabled) {
    out.push({
      key: "browser.enabled",
      title: "Let your pet use your Chrome?",
      body:
        "For a browser task you confirm, Pawl connects to your own Chrome (signed in as you) through Chrome's auto-connect. " +
        "Chrome asks you to Allow each connection and shows a banner while it is controlled. Every click and every typing asks you here first, " +
        "and the pet never types passwords or payment details. Text on web pages is never taken as instructions.",
      confirm: "Turn on browser tasks",
    });
  }
  const added = (p.allowSites ?? []).filter((h) => !cur.allowSites.includes(h));
  if (added.length > 0) {
    // Plain http and unusual ports are said in the confirm too.
    const warnings = added.map((o) => parseAllowSite(o)).flatMap((r) => (r.ok && r.warning ? [r.warning] : []));
    out.push({
      key: "browser.allowSites",
      title: added.length === 1 ? `Open ${added[0]} without asking?` : `Open these ${added.length} sites without asking?`,
      body:
        `A browser task may open pages on ${added.join(", ")} without asking first. ` +
        "Clicking, typing, submitting, uploads and dialogs still ask every time." +
        (warnings.length ? ` Careful: ${warnings.join(" ")}` : ""),
      confirm: added.length === 1 ? "Allow this site" : "Allow these sites",
    });
  }
  return out;
}
