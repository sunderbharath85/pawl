import { useEffect, useMemo, useState } from "react";
import { HubRequestError } from "@pawl/hub-client";
import { NEVER_AUTO_ALLOW, isAutoAllowable, type Rule, type RuleAction, type ToolKind } from "@pawl/protocol";
import { TOOL_KINDS, TOOL_KIND_ICON, errorText } from "../../lib/format";
import { usePanel } from "../context";
import * as I from "../icons";

const ACTIONS: Array<{ value: RuleAction; label: string }> = [
  { value: "auto_allow", label: "Allow" },
  { value: "ask", label: "Ask me" },
  { value: "deny", label: "Deny" },
];

const FLOOR_LIST = NEVER_AUTO_ALLOW.map((k) => k.replace("_", " ")).join(", ");

/** Security hard floor (mirrors the hub): auto_allow needs a named, non-risky tool kind. */
function violatesFloor(r: Rule): boolean {
  return r.action === "auto_allow" && !isAutoAllowable(r.match.toolKind);
}

function kindLabel(kind: ToolKind | undefined): string {
  return kind ? kind.replace("_", " ") : "any tool";
}

/** The hub rejects floor violations with ack error `invalid_rule: <detail>`. */
function invalidRuleMessage(err: unknown): string | null {
  const text = err instanceof HubRequestError ? `${err.code} ${err.message}` : errorText(err);
  const m = /invalid_rule\s*:?\s*([^\n]*)/.exec(text);
  if (!m) return null;
  const detail = (m[1] ?? "").replace(/\s*hub request "set_rules" failed:.*$/, "").trim();
  return detail || "One of these rules breaks the auto-allow safety floor.";
}

function newRuleId(): string {
  return `rule-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 6)}`;
}

function sameRules(a: Rule[], b: Rule[]): boolean {
  return JSON.stringify(a) === JSON.stringify(b);
}

export function RulesView() {
  const { client, state, toast, meta } = usePanel();
  const [draft, setDraft] = useState<Rule[]>(state.rules);
  const [saving, setSaving] = useState(false);
  const [saveError, setSaveError] = useState<string | null>(null);
  const dirty = !sameRules(draft, state.rules);
  const invalidCount = draft.filter(violatesFloor).length;

  // Any edit makes the last rejection stale.
  useEffect(() => setSaveError(null), [draft]);

  // Adopt server changes while the user has nothing pending.
  useEffect(() => {
    if (!dirty) setDraft(state.rules);
  }, [state.rules]);

  const agents = useMemo(() => [...state.agents].sort((a, b) => a.name.localeCompare(b.name)), [state.agents]);

  const update = (idx: number, patch: Partial<Rule> & { match?: Rule["match"] }) =>
    setDraft((d) => d.map((r, i) => (i === idx ? { ...r, ...patch, match: patch.match ?? r.match } : r)));
  const setMatch = (idx: number, key: keyof Rule["match"], value: string) =>
    setDraft((d) =>
      d.map((r, i) => {
        if (i !== idx) return r;
        const match = { ...r.match } as Record<string, string | undefined>;
        if (value) match[key] = value;
        else delete match[key];
        const next: Rule = { ...r, match: match as Rule["match"] };
        // A kind below the floor can never keep auto-allowing: fall back to asking.
        return violatesFloor(next) ? { ...next, action: "ask" } : next;
      }),
    );
  const move = (idx: number, dir: -1 | 1) =>
    setDraft((d) => {
      const j = idx + dir;
      if (j < 0 || j >= d.length) return d;
      const next = d.slice();
      [next[idx], next[j]] = [next[j]!, next[idx]!];
      return next;
    });

  const save = async () => {
    if (invalidCount > 0) return;
    setSaving(true);
    setSaveError(null);
    try {
      const saved = await client.setRules(draft.map((r) => ({ ...r, match: cleanMatch(r.match) })));
      setDraft(saved);
      toast("Rules saved. Good boy.");
    } catch (err) {
      const invalid = invalidRuleMessage(err);
      if (invalid) setSaveError(invalid);
      else toast(errorText(err), "error");
    } finally {
      setSaving(false);
    }
  };

  return (
    <section className="view">
      <header className="view-head">
        <h1>Rules</h1>
        <p>
          When an agent asks permission, the first matching rule decides. No match means your pet asks you. Auto-allow only
          works for a named, low-risk tool kind: {FLOOR_LIST} and &ldquo;any tool&rdquo; always come to you (or hit a deny
          rule).
        </p>
      </header>

      <div className="rules">
        {draft.length === 0 && <div className="hint">No rules: every request comes to you.</div>}
        {draft.map((r, idx) => {
          const canAutoAllow = isAutoAllowable(r.match.toolKind);
          const invalid = violatesFloor(r);
          return (
            <div key={r.id} className={`rule-row action-${r.action}${invalid ? " is-invalid" : ""}`}>
              <span className="rule-num">{idx + 1}</span>
              <div className="rule-match">
                <span className="rule-word">When</span>
                <select
                  className="input select"
                  value={r.match.toolKind ?? ""}
                  onChange={(e) => setMatch(idx, "toolKind", e.target.value)}
                  aria-label="Tool kind"
                >
                  <option value="">any tool</option>
                  {TOOL_KINDS.map((k) => (
                    <option key={k} value={k}>
                      {TOOL_KIND_ICON[k]} {k.replace("_", " ")}
                    </option>
                  ))}
                </select>
                <span className="rule-word">on</span>
                <input
                  className="input mono rule-path"
                  value={r.match.path ?? ""}
                  placeholder="any path (glob, e.g. **/*.md)"
                  onChange={(e) => setMatch(idx, "path", e.target.value)}
                  aria-label="Path glob"
                />
                <span className="rule-word">by</span>
                <select
                  className="input select"
                  value={r.match.agent ?? ""}
                  onChange={(e) => setMatch(idx, "agent", e.target.value)}
                  aria-label="Agent"
                >
                  <option value="">any agent</option>
                  {agents.map((a) => (
                    <option key={a.id} value={a.id}>
                      {a.name}
                    </option>
                  ))}
                  {r.match.agent && !agents.some((a) => a.id === r.match.agent) && (
                    <option value={r.match.agent}>{r.match.agent}</option>
                  )}
                </select>
              </div>
              <div className="segmented" role="radiogroup" aria-label="Action">
                {ACTIONS.map((a) => {
                  const blocked = a.value === "auto_allow" && !canAutoAllow;
                  return (
                    <button
                      key={a.value}
                      role="radio"
                      aria-checked={r.action === a.value}
                      className={`seg seg-${a.value}${r.action === a.value ? " is-active" : ""}`}
                      disabled={blocked && r.action !== a.value}
                      title={
                        blocked
                          ? `${kindLabel(r.match.toolKind)} is never auto-allowed. Pick a specific low-risk tool kind first.`
                          : undefined
                      }
                      onClick={() => {
                        if (!blocked) update(idx, { action: a.value });
                      }}
                    >
                      {a.label}
                    </button>
                  );
                })}
              </div>
              <div className="rule-tools">
                <button className="icon-btn" onClick={() => move(idx, -1)} disabled={idx === 0} aria-label="Move up">
                  <I.ArrowUp size={15} />
                </button>
                <button className="icon-btn" onClick={() => move(idx, 1)} disabled={idx === draft.length - 1} aria-label="Move down">
                  <I.ArrowDown size={15} />
                </button>
                <button className="icon-btn danger" onClick={() => setDraft((d) => d.filter((_, i) => i !== idx))} aria-label="Delete rule">
                  <I.Trash size={15} />
                </button>
              </div>
              {invalid && (
                <div className="rule-invalid">
                  {kindLabel(r.match.toolKind)} can never be auto-allowed. Pick a specific low-risk tool kind, or switch this
                  rule to Ask me or Deny.
                </div>
              )}
            </div>
          );
        })}
      </div>

      {saveError && (
        <div className="rules-error" role="alert">
          <strong>The hub refused these rules.</strong> {saveError}
        </div>
      )}

      <div className="form-actions">
        <button
          className="pill"
          onClick={() => setDraft((d) => [...d, { id: newRuleId(), match: { toolKind: "read" }, action: "auto_allow" }])}
        >
          <I.Plus size={14} /> Add rule
        </button>
        <span className="spacer" />
        {dirty && (
          <button className="pill pill-ghost" onClick={() => setDraft(state.rules)}>
            Revert
          </button>
        )}
        {invalidCount > 0 && (
          <span className="hint">
            Fix {invalidCount} rule{invalidCount > 1 ? "s" : ""} to save
          </span>
        )}
        <button
          className="pill pill-primary"
          disabled={!dirty || saving || invalidCount > 0 || meta.phase !== "connected"}
          onClick={() => void save()}
        >
          {saving ? "Saving…" : "Save rules"}
        </button>
      </div>
    </section>
  );
}

function cleanMatch(m: Rule["match"]): Rule["match"] {
  const out: Rule["match"] = {};
  if (m.toolKind) out.toolKind = m.toolKind;
  if (m.path?.trim()) out.path = m.path.trim();
  if (m.agent) out.agent = m.agent;
  return out;
}
