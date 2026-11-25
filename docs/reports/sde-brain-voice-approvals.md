# sde-brain: approvals and questions by voice (v0.2 item 3)

Rev 2 (2026-11-25): t13's decisions applied. A spoken deny acts at once, the owner-audit gap is accepted,
t29 keeps the code out of text-to-speech, and docs/CONTRACTS.md has a "v0.2 Voice approvals" subsection.

Worktree `E:\workspace\ai-pet-voice` (branch feat/voice). Scope: crates/brain and crates/hub, plus
one-line `voice_code: None` / `via: None` fills in crates/acp and crates/a2a so the workspace compiles
with t13's protocol fields. apps/ and packages/ untouched. Nothing committed.

## What a spoken approve / deny does

A chat with `via: "voice"` is parsed like typed chat; only approve / deny follow the voice rules
(`crates/brain/src/chat/spoken.rs`):

| Request | Spoken "approve" | Spoken "approve 4 7" (its code) | Spoken "deny" (or "no") |
|---|---|---|---|
| read / search / think / edit, this machine | 1st time: read-back; after the read-back: approved once | approved once | denied at once |
| execute, delete, fetch, move, switch_mode, other, unknown | read-back + `Say: “approve 4 7”`; still nothing after the read-back | approved once | denied at once |
| any request of another machine | read-back (names the machine) + `Say: “approve 6 2”` | forwarded to the owner as a confirmed approval | forwarded deny, at once |

- **Read-back:** `Codex on Desk wants to run a command: \`bun test\`.` It gives the tool kind, the command, file or title, and the machine. Commands are shown up to 300 characters, so a long command's tail is not hidden. It is for approvals only; if the transcript already contains the right code, it is skipped.
- **Bare "yes":** "yes", "sure" and "go ahead" are never enough by voice. "yes, approve" counts as explicit; the parser now accepts it for typed chat too.
- **Wrong code:** "That code doesn't match, so nothing happened." No action is taken and the hub counts the miss. A code that names another agent's request than the one the user named also counts as a mismatch, as does one on a machine other than the one named.
- **Always allow:** never by voice. The pet refuses and says why. A request that offers only "always allow" is sent to the screen.
- **Deny:** the safe direction. It acts at once, with no read-back and no code. A code or an agent name picks the request when several wait; otherwise the pet asks which one. A wrong code does nothing.
- **Code selects the request:** the code picks the request on any machine. Ordinals ("the first one") still pick. Spoken numerals are treated as codes, not as "approve 2" picks.
- **Hook sessions:** their requests are left to the terminal. None exist today; this is a defensive filter.
- **Questions:** "what is it asking?" and "what's Codex doing?" get the same reply as typed chat, and the reply never contains codes.

**Number parsing** (`spoken::spoken_numbers`). Each of these reads as "47":
- "4 7", "47.", "4-7"
- "four seven", "forty seven", "forty-seven"
- "for seven" (homophones such as for/to/won/ate/oh count only next to a spelled number word)
- "approve 4 7 for codex"

These read as no number at all: "this one", "go to codex", "api2", "2nd".

## Hub side (`crates/hub/src/core/voice.rs`)

**Codes**
- Every request this hub creates gets a random code from 10 to 99, assigned under the pending lock. It is never shared with another pending request, and any code the agent side sends is replaced.
- The code goes out with `permission_request`, the snapshot, and the mirror to peers.
- Event history (`request_permission` payload) never stores it.
- On a mirrored request, the code is kept only if it is exactly two digits.

**Chat**
- `via` must be `"voice"` or absent; any other value is rejected as `invalid`.
- Per chat, the read-back is kept for the next message only, for at most 2 minutes. A typed message in between clears it.
- Three wrong codes within 10 minutes pause voice approvals. The pet and the hub both refuse until then; deny still works.

**The hub re-checks before acting** (`resolve_by_voice`):
- no `allow_always`;
- no hook-session request;
- a risky or remote approval must have that request's code in the transcript and must not be paused.

The read-back for a local read or edit is left to the brain.

**Remote:** with the right code, `resolve_remote_as(confirmed: true)` runs. The code counts as the confirmation on this device, and the owner applies its own rules as before.

**Audit**
- Schema v10 adds `via` and `transcript` columns to `audit_log` and `intent_log`. The transcript is cut to 300 characters.
- A spoken decision on this machine is audited with `via = "voice"`, the transcript, and actor `voice:chat:desktop#N`.
- The `permission_resolved` event payload carries `via: "voice"`.
- A spoken decision on another machine's request is logged in this hub's intent log (origin row) with `via` and the transcript.

**Models never see codes**
- `understand::pack` builds its lines from explicit fields, and a test checks the code is absent.
- A spoken transcript that contains a pending request's code is never sent to a model. Small talk and understanding are skipped and the reply is deterministic.

## Decisions (t13, applied in rev 2)

1. **Deny acts immediately by voice.** No read-back and no code; the read-back is for approvals only.
2. **Known gap: the owner's audit cannot say "voice".** `Intent::Approve` has no `via` field, so the owner's audit row for a voice approval from another machine shows the origin with `via` null. The origin's intent log holds `via` and the transcript. Accepted by t13.
3. **The code is never spoken.** The code line is the reply's last line (`Say: “approve 4 7” to allow it once, or “deny”.`), and t29 makes text-to-speech skip it.
4. **`docs/CONTRACTS.md`:** a new "v0.2 Voice approvals" subsection under "v0.2 Voice" covers the API, the voiceCode rules, `via`, schema v10 and the wrong-code pause. The section's sentence "voice approvals are the next step" now points to that subsection. CRLF endings kept.

Unchanged from rev 1:
- `ServerMsg::PermissionResolved` has no `via`, so it lives only in the recorded `permission_resolved` event.
- Codes run 10 to 99 (no leading zero). With 90 requests pending, a new one gets no code and is approved on screen only.
- Three wrong codes in 10 minutes pause approving by voice, against guessing.

## Public API (also in docs/CONTRACTS.md, "v0.2 Voice approvals")

**pawl-brain**
- New module `chat::spoken`: `spoken_numbers`, `says_code`, `is_voice_code`, `spell_code`, `needs_code`.
- `BrainContext` gains `spoken`, `read_back` and `voice_locked`.
- `BrainReply` gains `read_back` and `code_miss` (serde-skipped) and now derives `Default`.
- `BrainAction::ResolvePermission` may name another machine's request (voice with code only).

**pawl-hub**
- `Hub::chat_as(pet_id, text, spoken, actor)`.
- `AuditEntry` and `IntentLogEntry` gain `via` and `transcript`.

## Other changes

- **Flaky test fixed:** `without_a_model_replies_are_plain_and_hint_once` sometimes failed because sessions shared a millisecond timestamp. The test now gives the extra sessions older timestamps.
- **Not a gate failure:** `cargo clippy -p pawl-brain` run alone stops in pawl-protocol (`large_enum_variant` on `ServerMsg`). The workspace clippy gate passes.

## Tests

**Brain** (`chat/tests.rs`, `chat/spoken.rs`)
- local read/edit/search/think: read-back, then approve; bare yes refused;
- risky kinds need the code;
- wrong codes;
- another machine's request: needs its code, denied at once, typed chat unchanged;
- always allow refused;
- deny acts at once (no read-back, no code; code or agent picks; wrong code does nothing; another machine's; while paused);
- the code picks the request; read-back preferred; ordinals;
- pause after wrong codes;
- hook sessions excluded;
- questions answered as typed;
- codes never reach a model;
- the read-back shows the end of a long command;
- number parsing; the `needs_code` table.

**Hub** (`core/tests/voice.rs`, `core/voice.rs`)
- codes unique, replaced, kept out of history;
- a command needs its code; audit `via` and transcript; `permission_resolved` `via`;
- transcript cut to 300 characters;
- read-back lasts one message;
- the hub re-checks always-allow and codes;
- three wrong codes pause approvals;
- `via` validation;
- mirrored code validation and offline intent log with `via`;
- two real hubs: a spoken code approves the other machine's request, and typed chat still does not;
- code picking.

## Gates

- `cargo test --workspace`: all green (brain 124, hub 264, acp 25, a2a 105, plus the rest).
- `cargo clippy --workspace --all-targets -- -D warnings`: clean.
