import { useEffect, useMemo, useState } from "react";
import type { Rule, RuleAction, ToolKind } from "@ai-pet/protocol";
import { TOOL_KINDS, TOOL_KIND_ICON, errorText } from "../../lib/format";
import { usePanel } from "../context";
import * as I from "../icons";

const ACTIONS: Array<{ value: RuleAction; label: string }> = [
  { value: "auto_allow", label: "Allow" },
  { value: "ask", label: "Ask me" },
  { value: "deny", label: "Deny" },
];

const DANGEROUS: ToolKind[] = ["delete", "execute", "fetch"];

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
  const dirty = !sameRules(draft, state.rules);

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
        return { ...r, match: match as Rule["match"] };
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
    setSaving(true);
    try {
      const saved = await client.setRules(draft.map((r) => ({ ...r, match: cleanMatch(r.match) })));
      setDraft(saved);
      toast("Rules saved. Good boy.");
    } catch (err) {
      toast(errorText(err), "error");
    } finally {
      setSaving(false);
    }
  };

  return (
    <section className="view">
      <header className="view-head">
        <h1>Rules</h1>
        <p>
          When an agent asks permission, the first matching rule decides. No match means your pet asks you. Destructive kinds
          (delete, execute, fetch) are only auto-allowed by a rule that names that kind.
        </p>
      </header>

      <div className="rules">
        {draft.length === 0 && <div className="hint">No rules: every request comes to you.</div>}
        {draft.map((r, idx) => {
          const risky = r.action === "auto_allow" && (!r.match.toolKind || DANGEROUS.includes(r.match.toolKind));
          return (
            <div key={r.id} className={`rule-row action-${r.action}`}>
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
                {ACTIONS.map((a) => (
                  <button
                    key={a.value}
                    role="radio"
                    aria-checked={r.action === a.value}
                    className={`seg seg-${a.value}${r.action === a.value ? " is-active" : ""}`}
                    onClick={() => update(idx, { action: a.value })}
                  >
                    {a.label}
                  </button>
                ))}
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
              {risky && <div className="rule-warn">Careful: this can auto-allow commands, deletes or network access.</div>}
            </div>
          );
        })}
      </div>

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
        <button className="pill pill-primary" disabled={!dirty || saving || meta.phase !== "connected"} onClick={() => void save()}>
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
