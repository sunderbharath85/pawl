//! Settings > A2A > "Connect coding agents" (docs/CONTRACTS.md "v0.2 A2A: connect coding agents").
//!
//! Connect = the localhost A2A server on (said in the confirm, never silent), an A2A caller for the
//! agent with approvals off for good (`connect_target`) and a fresh token, and a `pawl` MCP entry
//! running `pawl mcp` in the agent's own user-level MCP config (`pawl_discovery::mcp_config`).
//! Remove revokes the caller first (its token stops at once), then takes the entry out, best effort.
//! Local only, like every settings message; every apply / remove is audited and logged without the
//! token.

use std::path::{Path, PathBuf};

use pawl_discovery::mcp_config::{self as mcp, ChangedWhileEditing, EntryState, McpChange, McpCheck, McpEntry, McpEnv, McpPlan, McpTarget};
use pawl_protocol::{A2aConnectState, A2aConnectStatus, HookAction, HookTargetName};

use super::a2a_server::A2aStatus;
use super::settings::target_label;
use super::*;
use crate::config::A2aCaller;
use crate::settings::{self as cfgset, InvalidSetting};
use crate::store::SettingsAuditKind;

/// Said in every Connect plan.
const LOCAL_TOKEN_NOTE: &str = "\"Can never answer approvals\" holds for this A2A token. Pawl's local API token in its data folder can be read by any program running as you, so an agent with shell access is only as contained as your OS user.";
/// Said in VS Code's plans.
const VSCODE_SYNC_NOTE: &str = "If VS Code Settings Sync syncs mcp.json, this entry (with this machine's token) reaches your other machines, and Connect on each machine replaces the other's token.";

fn target_of(name: HookTargetName) -> McpTarget {
    match name {
        HookTargetName::Claude => McpTarget::Claude,
        HookTargetName::Codex => McpTarget::Codex,
        HookTargetName::CopilotCli => McpTarget::CopilotCli,
        HookTargetName::CopilotVscode => McpTarget::Vscode,
    }
}

fn name_of(t: McpTarget) -> HookTargetName {
    match t {
        McpTarget::Claude => HookTargetName::Claude,
        McpTarget::Codex => HookTargetName::Codex,
        McpTarget::CopilotCli => HookTargetName::CopilotCli,
        McpTarget::Vscode => HookTargetName::CopilotVscode,
    }
}

/// The URL `pawl mcp` calls: the A2A server only ever listens on 127.0.0.1.
pub(super) fn a2a_url(cfg: &HubConfig) -> String {
    format!("http://127.0.0.1:{}", cfg.a2a.port)
}

fn caller(cfg: &HubConfig, t: McpTarget) -> Option<&A2aCaller> {
    cfg.a2a.callers.iter().find(|c| c.id == t.caller_id())
}

/// The caller Connect made (or took over) for this agent; a hand-made one with the same id is not.
fn marked_caller(cfg: &HubConfig, t: McpTarget) -> Option<&A2aCaller> {
    caller(cfg, t).filter(|c| c.connect_target == Some(name_of(t).into()))
}

/// What the agent's entry should hold now (None: its caller has no token, so no entry is current).
fn wanted(cfg: &HubConfig, t: McpTarget, exe: &Path) -> Option<McpEntry> {
    let token = caller(cfg, t)?.token.clone()?;
    Some(McpEntry { exe: exe.to_path_buf(), url: a2a_url(cfg), token })
}

fn status_of(cfg: &HubConfig, server: &A2aStatus, t: McpTarget, check: McpCheck, found: bool) -> A2aConnectStatus {
    let (state, mut detail) = match check.state {
        Ok(EntryState::Current) if !cfg.a2a.server => (A2aConnectState::Outdated, Some("Pawl's A2A server is off; Connect turns it on (127.0.0.1 only), no new token".to_string())),
        Ok(EntryState::Current) => (A2aConnectState::Connected, None),
        Ok(EntryState::Stale) if caller(cfg, t).is_none() => (A2aConnectState::Outdated, Some(format!("the A2A caller \"{}\" was removed; Connect makes it again", t.caller_id()))),
        Ok(EntryState::Stale) => (A2aConnectState::Outdated, Some("the entry points at another Pawl, port or token (or is there twice); Connect updates it".to_string())),
        Ok(EntryState::Absent) => (A2aConnectState::Missing, None),
        Err(e) => (A2aConnectState::Unsupported, Some(e)),
    };
    if state == A2aConnectState::Connected && !server.running {
        // On in the settings but not listening (e.g. its port is taken): the agent's calls fail.
        let why = server.error.clone().unwrap_or_else(|| "it is not running".to_string());
        detail = Some(format!("Pawl's A2A server is not running: {why}"));
    }
    A2aConnectStatus {
        target: name_of(t),
        name: t.name().to_string(),
        agent_installed: found,
        state,
        config_path: check.config_path.to_string_lossy().into_owned(),
        caller_id: t.caller_id().to_string(),
        caller_exists: caller(cfg, t).is_some(),
        caller_marked: marked_caller(cfg, t).is_some(),
        detail,
    }
}

/// Run file work off the async threads.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
    tokio::task::spawn_blocking(f).await.context("agent config")?
}

/// What Connect does to Pawl's own A2A settings.
struct Changes {
    /// A caller the user added by hand gets taken over (its open tasks stay with the old owner).
    takeover: bool,
    new_token: bool,
}

fn changes(cur: &HubConfig, t: McpTarget, current: bool) -> Changes {
    let takeover = caller(cur, t).is_some_and(|c| c.connect_target.is_none());
    Changes { takeover, new_token: !current || takeover }
}

/// The plan's lines about Pawl's side (server, caller), before the file part.
fn connect_lines(t: McpTarget, cur: &HubConfig, ch: &Changes) -> String {
    let mut s = String::new();
    if !cur.a2a.server {
        s.push_str(&format!("Turns on Pawl's A2A server (on 127.0.0.1 only, port {}).\n", cur.a2a.port));
    }
    match caller(cur, t) {
        None => s.push_str(&format!(
            "Creates the A2A caller \"{}\" ({}) with a new token. It can never answer approvals: tool calls of the work it starts ask you here.\n",
            t.caller_id(),
            t.name()
        )),
        Some(c) if ch.takeover => s.push_str(&format!(
            "Takes over the caller \"{}\" you added; its open tasks stay with the old owner and its token stops working. From now on it can never answer approvals.\n",
            c.id
        )),
        Some(c) => {
            if ch.new_token {
                s.push_str(&format!("Gives the A2A caller \"{}\" a new token (its old token stops working).\n", c.id));
            } else {
                s.push_str(&format!("Keeps the A2A caller \"{}\" and its token: no new token.\n", c.id));
            }
            if c.allow_approvals {
                s.push_str(&format!("Turns approvals off for the A2A caller \"{}\": a connected agent never answers approvals.\n", c.id));
            }
        }
    }
    s
}

fn notes(t: McpTarget) -> String {
    let mut s = format!("\nNote: {LOCAL_TOKEN_NOTE}\n");
    if t == McpTarget::Vscode {
        s.push_str(&format!("Note: {VSCODE_SYNC_NOTE}\n"));
    }
    s
}

/// Record on the agent's caller in `cfg` that Connect added the servers object itself.
fn set_added_servers(cfg: &mut HubConfig, t: McpTarget, changed: &mut Vec<&'static str>) {
    if let Some(c) = cfg.a2a.callers.iter_mut().find(|c| c.id == t.caller_id() && c.connect_target == Some(name_of(t).into())) {
        if !c.connect_added_servers {
            c.connect_added_servers = true;
            if !changed.contains(&cfgset::A2A_CALLERS) {
                changed.push(cfgset::A2A_CALLERS);
            }
        }
    }
}

/// The result of one `a2a_connect`.
struct Outcome {
    plan: String,
    applied: bool,
    new_token: bool,
    warning: Option<String>,
}

impl Hub {
    /// The home folder and env the agents use (tests: the hooks home, no env overrides).
    fn mcp_place(&self) -> Result<(PathBuf, McpEnv)> {
        match self.hooks_home() {
            Some(h) => Ok((h, McpEnv::default())),
            None => Ok((pawl_discovery::hooks::agent_home()?, McpEnv::from_system())),
        }
    }

    async fn connect_status_now(&self, t: McpTarget) -> Result<A2aConnectStatus> {
        let cfg = self.config();
        let server = self.a2a_status();
        let exe = Self::hook_exe()?;
        let (home, env) = self.mcp_place()?;
        blocking(move || {
            // A crash may have left a temp copy holding a token.
            mcp::clean_stale_temps(t, &home, &env);
            let check = mcp::check(t, &home, &env, wanted(&cfg, t, &exe).as_ref());
            let found = mcp::agent_found(t, &pawl_discovery::detect::HostEnv::from_system());
            Ok(status_of(&cfg, &server, t, check, found))
        })
        .await
    }

    /// `a2a_connect_status`: claude, codex, copilot-cli, copilot-vscode, in that order.
    pub async fn a2a_connect_status(&self) -> Result<Vec<A2aConnectStatus>> {
        let mut out = Vec::new();
        for t in McpTarget::ALL {
            out.push(self.connect_status_now(t).await?);
        }
        Ok(out)
    }

    /// `a2a_connect{target, action}`: preview (nothing written) / apply (Connect) / remove.
    pub async fn a2a_connect(&self, target: HookTargetName, action: HookAction, actor: &Actor) -> Result<Value> {
        let t = target_of(target);
        let exe = Self::hook_exe()?;
        let (home, env) = self.mcp_place()?;
        let _one = self.inner.settings_write.lock().await;
        let cur = self.config();
        let starts_server = !cur.a2a.server && action != HookAction::Remove;
        let out = match action {
            HookAction::Preview => self.connect_preview(t, &cur, &exe, home, env).await?,
            HookAction::Apply => self.connect_apply(t, &cur, &exe, home, env, actor).await?,
            HookAction::Remove => self.connect_remove(t, &cur, home, env, actor).await?,
        };
        drop(_one);
        let status = self.connect_status_now(t).await?;
        let mut ack = json!({ "plan": out.plan, "applied": out.applied, "status": status, "startsServer": starts_server, "newToken": out.new_token });
        if let Some(w) = out.warning {
            ack["warning"] = json!(w);
        }
        Ok(ack)
    }

    /// Is the entry there and current, and can the file be edited at all?
    async fn entry_current(t: McpTarget, cur: &HubConfig, exe: &Path, home: &Path, env: &McpEnv) -> Result<bool> {
        let now = wanted(cur, t, exe);
        let (home, env) = (home.to_path_buf(), env.clone());
        blocking(move || match mcp::check(t, &home, &env, now.as_ref()).state {
            Ok(s) => Ok(s == EntryState::Current),
            Err(e) => Err(anyhow!(e)),
        })
        .await
    }

    async fn connect_preview(&self, t: McpTarget, cur: &HubConfig, exe: &Path, home: PathBuf, env: McpEnv) -> Result<Outcome> {
        let current = Self::entry_current(t, cur, exe, &home, &env).await?;
        let ch = changes(cur, t, current);
        let entry = McpEntry { exe: exe.to_path_buf(), url: a2a_url(cur), token: mcp::TOKEN_PLACEHOLDER.to_string() };
        let plan = blocking(move || mcp::plan(t, &home, &env, McpChange::Connect(&entry))).await?;
        let lines = connect_lines(t, cur, &ch);
        let nothing = cur.a2a.server && current && !ch.takeover && !caller(cur, t).is_some_and(|c| c.allow_approvals);
        let file = if current { format!("== {}: {} ==\nthe pawl entry is up to date; nothing written\n", t.name(), plan.path.display()) } else { plan.summary };
        let text = if nothing {
            format!("Already connected; nothing to change.\n\n{file}")
        } else {
            format!("Connect {} to Pawl (preview: nothing written yet)\n\n{lines}\n{file}{}", t.name(), notes(t))
        };
        Ok(Outcome { plan: text, applied: false, new_token: !nothing && ch.new_token, warning: None })
    }

    /// Plan and write the entry (`Some`) or its removal (`None`, with `drop_empty_servers`), reading
    /// the file again and retrying once if the agent rewrote it in between.
    async fn write_entry(t: McpTarget, home: &Path, env: &McpEnv, entry: Option<McpEntry>, drop_empty_servers: bool) -> Result<(McpPlan, String)> {
        let mut last = None;
        for _ in 0..2 {
            let (home, env, entry) = (home.to_path_buf(), env.clone(), entry.clone());
            let r = blocking(move || {
                let change = match &entry {
                    Some(e) => McpChange::Connect(e),
                    None => McpChange::Remove { drop_empty_servers },
                };
                let plan = mcp::plan(t, &home, &env, change)?;
                let outcome = mcp::apply(&plan)?;
                Ok((plan, outcome))
            })
            .await;
            match r {
                Err(e) if ChangedWhileEditing::is(&e) => last = Some(e),
                other => return other,
            }
        }
        Err(last.expect("tried"))
    }

    async fn connect_apply(&self, t: McpTarget, cur: &HubConfig, exe: &Path, home: PathBuf, env: McpEnv, actor: &Actor) -> Result<Outcome> {
        // A config Pawl cannot edit is refused before anything changes.
        let current = Self::entry_current(t, cur, exe, &home, &env).await?;
        let ch = changes(cur, t, current);
        let lines = connect_lines(t, cur, &ch);
        let mut next = cur.clone();
        let mut changed: Vec<&'static str> = Vec::new();
        if !next.a2a.server {
            next.a2a.server = true;
            changed.push(cfgset::A2A_SERVER);
        }
        let token = match (ch.new_token, wanted(cur, t, exe)) {
            (false, Some(e)) => e.token,
            _ => a2a_server::new_token()?,
        };
        match next.a2a.callers.iter_mut().find(|c| c.id == t.caller_id()) {
            Some(c) => {
                // Marked for this agent: approvals off for good.
                if c.allow_approvals || c.connect_target != Some(name_of(t).into()) {
                    c.allow_approvals = false;
                    c.connect_target = Some(name_of(t).into());
                    changed.push(cfgset::A2A_CALLERS);
                }
                if ch.new_token {
                    c.token = Some(token.clone());
                    changed.push(cfgset::A2A_TOKENS);
                }
            }
            None => {
                if next.a2a.callers.len() >= cfgset::MAX_A2A_CALLERS {
                    return Err(InvalidSetting { field: cfgset::A2A_CALLERS.into(), reason: format!("at most {} callers: remove one first", cfgset::MAX_A2A_CALLERS) }.into());
                }
                next.a2a.callers.push(A2aCaller {
                    id: t.caller_id().into(),
                    name: t.name().into(),
                    token: Some(token.clone()),
                    allow_approvals: false,
                    connect_target: Some(name_of(t).into()),
                    connect_added_servers: false,
                });
                changed.extend([cfgset::A2A_CALLERS, cfgset::A2A_TOKENS]);
            }
        }
        let entry = McpEntry { exe: exe.to_path_buf(), url: a2a_url(&next), token };
        // The file is planned (and so checked) before Pawl's own config changes.
        let first = {
            let (home, env, entry) = (home.clone(), env.clone(), entry.clone());
            blocking(move || mcp::plan(t, &home, &env, McpChange::Connect(&entry))).await?
        };
        if changed.is_empty() && !first.changed() {
            return Ok(Outcome { plan: format!("Already connected; nothing to change.\n\n{}", first.summary), applied: false, new_token: false, warning: None });
        }
        // Connect adding the servers object itself is recorded, so Remove can take that object away again.
        if first.added_servers {
            set_added_servers(&mut next, t, &mut changed);
        }
        if !changed.is_empty() {
            self.commit_settings(cur, next.clone(), &changed, actor, SettingsAuditKind::Settings).await?;
        }
        // A hand-made caller taken over starts as a new task owner. Only now, with its old token
        // already revoked, so no request with that token can get the new generation in between.
        let old_gen = if ch.takeover { self.take_a2a_generation(t.caller_id()) } else { None };
        let (plan, outcome) = match Self::write_entry(t, &home, &env, Some(entry), false).await {
            Ok(w) => w,
            Err(e) => {
                // Without its entry the caller is no use: put Pawl's A2A settings back.
                if !changed.is_empty() {
                    let mut undo = self.config();
                    undo.a2a.server = cur.a2a.server;
                    undo.a2a.callers = cur.a2a.callers.clone();
                    if let Err(u) = self.commit_settings(&next, undo, &changed, actor, SettingsAuditKind::Settings).await {
                        tracing::warn!("could not undo the A2A settings after a failed connect: {u:#}");
                    }
                }
                self.put_a2a_generation(t.caller_id(), old_gen);
                return Err(e.context(format!("could not connect {}; nothing was changed", t.name())));
            }
        };
        // The file changed between the plan and the write: what was written decides the record.
        if plan.added_servers != first.added_servers && plan.added_servers {
            let now = self.config();
            let mut fixed = now.clone();
            let mut keys = Vec::new();
            set_added_servers(&mut fixed, t, &mut keys);
            if !keys.is_empty() {
                if let Err(e) = self.commit_settings(&now, fixed, &keys, actor, SettingsAuditKind::Settings).await {
                    tracing::warn!("could not record that Connect added the servers object: {e:#}");
                }
            }
        }
        let key = format!("a2a.connect.{}", target_label(name_of(t)));
        tracing::info!(agent = t.caller_id(), takeover = ch.takeover, new_token = ch.new_token, by = %actor.label, "coding agent connected to the A2A server");
        self.audit_settings(actor, SettingsAuditKind::A2aConnect, vec![key], Some("apply".into())).await;
        let file = if plan.changed() { format!("{}{outcome}\n", plan.summary) } else { format!("== {}: {} ==\nthe pawl entry is up to date; nothing written\n", t.name(), plan.path.display()) };
        Ok(Outcome {
            plan: format!("Connected {} to Pawl.\n\n{lines}\n{file}\n{} picks it up in its next session.\n{}", t.name(), t.name(), notes(t)),
            applied: true,
            new_token: ch.new_token,
            warning: None,
        })
    }

    async fn connect_remove(&self, t: McpTarget, cur: &HubConfig, home: PathBuf, env: McpEnv, actor: &Actor) -> Result<Outcome> {
        // Only a caller Connect made goes; one the user added by hand with that id stays.
        let marked = marked_caller(cur, t);
        let had_caller = marked.is_some();
        let drop_empty_servers = marked.is_some_and(|c| c.connect_added_servers);
        let hand_made = caller(cur, t).filter(|_| marked.is_none()).map(|c| c.id.clone());
        let path = mcp::config_path(t, &home, &env);
        // 1. The token stops working first, whatever the agent's file looks like.
        if had_caller {
            let mut next = cur.clone();
            next.a2a.callers.retain(|c| c.id != t.caller_id());
            self.commit_settings(cur, next, &[cfgset::A2A_CALLERS, cfgset::A2A_TOKENS], actor, SettingsAuditKind::Settings).await?;
        }
        // 2. The entry, best effort.
        let (file_text, file_changed, warning) = match Self::write_entry(t, &home, &env, None, drop_empty_servers).await {
            Ok((plan, outcome)) => {
                let changed = plan.changed();
                (if changed { format!("{}{outcome}\n", plan.summary) } else { plan.summary }, changed, None)
            }
            Err(e) if had_caller => {
                tracing::warn!(agent = t.caller_id(), "could not take the pawl entry out: {e:#}");
                (String::new(), false, Some(format!("Token revoked. Take the pawl entry out of {} by hand ({e:#}).", path.display())))
            }
            Err(e) => return Err(e),
        };
        let left = hand_made.map(|id| format!("Leaves the A2A caller \"{id}\" alone: you added it, Connect did not make it.\n")).unwrap_or_default();
        if !had_caller && !file_changed {
            return Ok(Outcome { plan: format!("{file_text}{left}{} is not connected; nothing to change.\n", t.name()), applied: false, new_token: false, warning: None });
        }
        let mut text = file_text;
        if had_caller {
            text.push_str(&format!("Removed the A2A caller \"{}\": its token stopped working.\n", t.caller_id()));
        }
        text.push_str(&left);
        let key = format!("a2a.connect.{}", target_label(name_of(t)));
        tracing::info!(agent = t.caller_id(), entry_removed = file_changed, by = %actor.label, "coding agent disconnected from the A2A server");
        self.audit_settings(actor, SettingsAuditKind::A2aConnect, vec![key], Some("remove".into())).await;
        Ok(Outcome { plan: text, applied: true, new_token: false, warning })
    }
}

#[cfg(test)]
pub(crate) fn caller_token(hub: &Hub, id: &str) -> Option<String> {
    hub.config().a2a.callers.iter().find(|c| c.id == id).and_then(|c| c.token.clone())
}
