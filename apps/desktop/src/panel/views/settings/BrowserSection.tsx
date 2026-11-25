// Settings > Browser (v0.2 item 5): the pet may drive your own Chrome for a task you confirm. Off by default. Chrome's
// auto-connect (Chrome 144+) needs remote debugging turned on once in Chrome; each task Chrome asks you to Allow and
// shows its banner; every click and every typing asks in Pawl first. Sites on the list open without asking.
import { useState } from "react";
import { ALLOW_SITES_MAX, browserOf, browserPatch, CHROME_DEBUG_URL, CHROME_MIN_VERSION, parseAllowSite } from "@pawl/hub-client";
import { CopyField } from "../../CopyField";
import { SettingsSection, Toggle, useSaveSettings, type SaveOutcome } from "./shared";
import type { SectionProps } from "./sections";

function outcomeError(out: SaveOutcome): string | null {
  if (out.ok || out.cancelled) return null;
  return out.fieldError ? out.fieldError.reason : out.message;
}

export function BrowserSection({ settings, live }: SectionProps) {
  const browser = browserOf(settings);
  const save = useSaveSettings();
  const [busy, setBusy] = useState(false);
  const [site, setSite] = useState("");
  const [error, setError] = useState<string | null>(null);
  // What the typed site would be saved as, and a warning for plain http or an unusual port, before adding it.
  const typed = site.trim() ? parseAllowSite(site) : null;
  const siteWarning = typed?.ok ? (typed.warning ?? (typed.origin !== site.trim() ? `Saved as ${typed.origin}.` : null)) : null;

  if (!browser) {
    return (
      <SettingsSection id="browser" title="Browser" description="Let your pet do web tasks in your own Chrome.">
        <p className="hint">This hub does not support browser tasks yet. Update pawl on this machine and restart the hub.</p>
      </SettingsSection>
    );
  }

  const run = async (patch: Parameters<typeof save>[0]) => {
    setBusy(true);
    try {
      const err = outcomeError(await save(patch));
      setError(err);
      return !err;
    } finally {
      setBusy(false);
    }
  };

  const add = async () => {
    const parsed = parseAllowSite(site);
    if (!parsed.ok) {
      setError(parsed.error);
      return;
    }
    if (browser.allowSites.includes(parsed.origin)) {
      setSite("");
      return;
    }
    if (browser.allowSites.length >= ALLOW_SITES_MAX) {
      setError(`At most ${ALLOW_SITES_MAX} sites.`);
      return;
    }
    const typed = site;
    // Clear the field only if it still holds what was added (the user may have typed the next site meanwhile).
    if (await run(browserPatch(browser, { allowSites: [...browser.allowSites, parsed.origin] }))) setSite((cur) => (cur === typed ? "" : cur));
  };

  return (
    <SettingsSection
      id="browser"
      title="Browser"
      description="Let your pet do web tasks you confirm (open a pull request and check CI, look at a staging page) in your own Chrome, signed in as you."
    >
      <Toggle
        label="Let your pet use your Chrome"
        hint="Off by default. The pet only starts a browser task after you confirm it."
        checked={browser.enabled}
        disabled={!live || busy}
        onChange={() => void run(browserPatch(browser, { enabled: !browser.enabled }))}
      />
      <div className="browser-how">
        <strong>How it works</strong>
        <ul>
          <li>
            Pawl uses your own Chrome through Chrome's auto-connect (Chrome {CHROME_MIN_VERSION} or newer). No separate browser, no copy of your
            profile.
          </li>
          <li>
            Once: open this page in Chrome and turn on remote debugging. Paste it into Chrome's address bar (Pawl can't open Chrome's own pages for you):
          </li>
        </ul>
        <CopyField label="Chrome remote debugging page" text={CHROME_DEBUG_URL} />
        <ul>
          <li>Each task: Chrome asks you to Allow the connection and shows a banner while the pet is in control.</li>
          <li>Every click and every typing asks you here first, showing exactly what will be typed. The pet never types passwords or payment details.</li>
          <li>Text on web pages is never taken as instructions: the pet only does what you asked.</li>
        </ul>
      </div>
      {browser.enabled && (
        <div className="field">
          <span className="field-label">Sites opened without asking</span>
          <span className="hint">
            A task may open pages on these sites without asking first. Clicking, typing, submitting, uploads and dialogs still ask. A site without
            https:// or http:// means https.
          </span>
          {browser.allowSites.length > 0 ? (
            <ul className="browser-sites" aria-label="Sites opened without asking">
              {browser.allowSites.map((h) => (
                <li key={h} className="browser-site">
                  <span className="mono">{h}</span>
                  <button
                    type="button"
                    className="browser-site-remove"
                    aria-label={`Remove ${h}`}
                    disabled={!live || busy}
                    onClick={() => void run(browserPatch(browser, { allowSites: browser.allowSites.filter((x) => x !== h) }))}
                  >
                    ×
                  </button>
                </li>
              ))}
            </ul>
          ) : (
            <span className="hint">None: every page asks first.</span>
          )}
          <form
            className="browser-add"
            onSubmit={(e) => {
              e.preventDefault();
              void add();
            }}
          >
            <input
              className={`input mono${error ? " is-invalid" : ""}`}
              value={site}
              placeholder="github.com (means https://github.com)"
              aria-label="Site to open without asking"
              disabled={!live}
              onChange={(e) => {
                setSite(e.target.value);
                setError(null);
              }}
            />
            <button type="submit" className="pill" disabled={!live || busy || !site.trim()}>
              Add site
            </button>
          </form>
        </div>
      )}
      {!error && siteWarning && <span className="field-warning">{siteWarning}</span>}
      {error && (
        <span className="field-error" role="alert">
          {error}
        </span>
      )}
    </SettingsSection>
  );
}
