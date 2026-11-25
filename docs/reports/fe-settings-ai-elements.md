# fe-settings: the session view as a real conversation (AI Elements)

Branch `feat/one-pet` (worktree `E:\workspace\ai-pet-onepet`). Nothing is committed; t13 commits.

## What the user asked

The user sent a screenshot of a Claude Code hook session in the panel and said: "what is all this? use AI elements from vercel to fix this, I do not understand a single thing in that session".

The view listed raw hook events, and the session titles were raw too:

| Raw text shown | Example |
|---|---|
| Hook event lines | `claude · PreToolUse: Bash: …`, `UserPromptSubmit: <task-notification>…`, `Notification: idle_prompt` |
| Sidebar titles | `[Image #40]`, `<task-notification> <task-id>…` |

Decision (t13): Vercel's ai-elements are deprecated in favour of **AI Elements from shadcn/ui** (ui.shadcn.com/docs/ai-elements), so this work uses those.

- **Variant:** the **base** variant (Base UI) with the default preset, **base-nova**.
- **How the components arrived:** the files come from the shadcn CLI, run in a throwaway Vite project in my scratchpad, so the CLI never installed anything into the repo. Then:
  - the component files were copied into `apps/desktop/src/components/ui`;
  - `"use client"` was removed from them;
  - the dependencies were added with `bash scripts/bun-locked.sh add`.
- **Docs read first:**
  - `/docs/installation/vite` and `/docs/ai-elements`;
  - the base pattern pages: message, chat, tool, task, reasoning, chain-of-thought, confirmation, prompt-input, plan, terminal;
  - the component pages: trace, code-block, message-scroller, collapsible, attachment;
  - Streamdown's README (its Tailwind `@source` setup and props).

## Packages added (apps/desktop only)

| Package | Version | Kind | Why |
|---|---|---|---|
| `@base-ui/react` | ^1.9.0 | dependency | primitives of the base variant (Collapsible, Button, merge-props, use-render) |
| `class-variance-authority` | ^0.7.1 | dependency | component variants |
| `cn` | ^0.5.2 | dependency | shadcn's class merger (replaces clsx + tailwind-merge in the new CLI output) |
| `lucide-react` | ^1.79.0 | dependency | icons (preset base-nova) |
| `@shadcn/react` | ^0.3.1 | dependency | Message Scroller primitive |
| `shiki` | ^4.6.0 | dependency | Code Block highlighting (lazy-loaded) |
| `streamdown` | ^2.8.1 | dependency | markdown of agent replies (the Message pattern) |
| `tailwindcss` | ^4.3.5 | devDependency | Tailwind v4 (build time) |
| `@tailwindcss/vite` | ^4.3.5 | devDependency | Vite plugin |
| `tw-animate-css` | ^1.4.0 | devDependency | animation utilities the components use |
| `shadcn` | ^4.23.0 | devDependency | only for `shadcn/tailwind.css` (custom variants such as `data-open`, `shimmer`, scroll-fade utilities); CSS at build time |

What was left out:

- No `ai` or `@ai-sdk/react`: we render our own transcript.
- `@types/node` was already there.
- `@fontsource-variable/geist` from the preset was left out; the panel keeps its own fonts.
- `package.json` keeps its CRLF line endings, and `bun.lock` was updated.

## Setup in apps/desktop

- **Tailwind without the global preflight.** `src/panel/ai.css` is imported only by the panel entry. It contains:
  - `@layer theme, base, components, utilities;`
  - `@import "tailwindcss/theme.css" layer(theme);`
  - `@import "tailwindcss/utilities.css" layer(utilities) source(none);`
  - `tw-animate-css` and `shadcn/tailwind.css`;
  - `@source` lines for `src/components`, `src/hooks`, `src/lib/highlighter.tsx`, `src/panel/transcript` and Streamdown's `dist`.

  The panel's hand-written CSS is unlayered, so it always wins over the layered utilities. Utilities are generated only for those files, so a class name in panel markup can never pick up a Tailwind rule.
- **The reset and the theme are scoped.**
  - The part of preflight the components rely on applies only inside `.ai-scope`, with `:where()` (zero specificity, base layer).
  - The shadcn variables are defined only on `.ai-scope`, because the panel already uses `--card`, `--accent` and `--radius`.
  - Inside the scope they map to the panel's warm palette. `tokens.css` gains three aliases for this: `--pawl-card`, `--pawl-accent` and `--on-accent`.
- **Dark mode.** The panel had none.
  - `tokens.css` now has a warm dark palette under `prefers-color-scheme: dark`.
  - `panel.css` and `settings.css` override the few colours they hard-code: backgrounds such as the sidebar gradient, hero and prompt bar, and status text colours.
  - `@custom-variant dark` in `ai.css` follows the same media query, so the shiki dark colours, the components and the panel always agree.
  - The pet window's bubble uses the same tokens, so it follows too. Decision: everything follows the system setting.
- **Aliases.** `@/*` resolves to `src/*`, in `tsconfig.json` (`paths`; the test config inherits it) and in `vite.config.ts` (`resolve.alias`).
- **Bundle splitting.**
  - The session view is `React.lazy` in `PanelApp`, so AI Elements, Base UI, Streamdown and the transcript code load when a session is first opened.
  - `hooks/use-highlighted-code.ts` loads `lib/highlighter` (shiki) with a dynamic `import()`; each language and theme is its own chunk.
  - The pet window (`index.html`) imports none of it. I checked the built chunks: no Streamdown, shiki or Base UI code, and no `.ai-scope` CSS.

## What was built

### 1. hub-client: `toTranscript(session, events)`

The transcript adapter is in `src/transcript.ts` and `src/injected.ts`. It is pure and tested.

It returns items of these kinds:

- `user`: text plus attachments;
- `assistant`: markdown;
- `reasoning`;
- `tool`: name, title, status (running, done, error or denied), input and output as `CodeText`, and an error line;
- `plan`;
- `permission`: its outcome (approved, denied, cancelled or pending) and who decided;
- `notice`: with a tone of muted, info, waiting or error, and an optional collapsed body.

**Owned (ACP) sessions:**

- Message and thought chunks merge into one item.
- `tool_call` and `tool_call_update` merge by `toolCallId`:
  - the input comes from `rawInput`, or from a diff block in `content`;
  - the output comes from text content or `rawOutput`.
- Plans update in place within a turn.
- `request_permission` and `permission_resolved` give the outcome. A refused call is marked "denied".
- A `stop` that is not `end_turn` becomes a notice, and errors become error notices.
- Unknown event types become a muted line, never JSON.

**Hook sessions (from `HookPayload.raw`):**

- `UserPromptSubmit.prompt` becomes the user turn.
- `PreToolUse` and `PostToolUse` (or `PostToolUseFailure`) pair into one call by `tool_use_id`, else by the oldest open call of the same tool.
  - **Title:** the description, else the command, pattern, query, URL or file path. File paths are shown relative to the session folder.
  - **Input:** the command as bash, an edit as a diff, else the arguments as JSON.
  - **Output:** stdout/stderr, a file's text, a result string, else JSON.
  - Output is cut to 60 lines or 6,000 characters, and the UI says how many lines were left out.
- `TodoWrite` becomes the plan.
- `Stop` becomes the assistant message (`last_assistant_message`). A turn's end settles calls that never reported back.
- Notifications:
  - `idle_prompt` becomes "Waiting for you in the terminal" (or "in VS Code" for `vscode_hook` sessions);
  - `permission_prompt` becomes "Asked for permission in the terminal", with the message as the collapsed body;
  - elicitation becomes "Asked you a question…";
  - housekeeping types are hidden.
- `SessionStart`, `SubagentStop` and `PreCompact` become quiet notices; `SubagentStart` and `PostCompact` are hidden.
- **Codex:** the notify payload's `input-messages` and `last-assistant-message`, plus the Claude-style lifecycle hooks.
- **Copilot:** camelCase payloads, with `toolArgs` as a JSON string and `toolResult.textResultForLlm`.

**Text the agent harness injected** (`parsePrompt`):

| Injected text | Shown as |
|---|---|
| `<system-reminder>` | a collapsed "Injected text" notice (it was hidden before the review fixes below) |
| `<task-notification>` | "Background task finished: \<summary\>" ("failed" or "update" by status) |
| `<hc from="t13">` | "Message from t13", body collapsed |
| `<pasted_content>` | a collapsed "Pasted text" attachment |
| `[Image #N]` | an image chip |

A block cut off by the hub's 4,000-character limit runs to the end of the text.

**For display:**

- `groupWork(items)` folds runs of two or more tool calls into one "Worked: ran 6 commands, read 2 files" step ("Working: …" while one runs). It adds "N failed" and "N refused".
- `workSummary`, `clipCode`, `relativePath`, `requestInputCode` and `hookPlace` are exported.

### 2. `cleanTitle(text, {agent, repo})`

- It applies the same stripping rules, puts the title on one line and caps it at 80 characters. An empty result becomes "\<agent\> in \<repo\>".
- `folderName(cwd)` gives the repo name.
- The desktop's `sessionTitle()` in `lib/format.ts` uses it, so the sidebar, Home, Approvals, the session header and the session pet's bubble all show clean titles.
- The screenshot run showed why this matters: the hub had stored the title as the raw `<task-notification>…` text, and the panel shows "Claude Code in acme-web".

### 3. The session view

`SessionView` plus `src/panel/transcript/`:

- **Turns.** The person's turns are right-aligned `Bubble`s on `Message`, with image chips and a collapsible "Pasted text". The agent's replies are Streamdown markdown, with fenced code in a `Code Block` (highlighted, copy button).
  - Links never navigate the panel window: they show as text with the address on hover.
  - Remote images show their alt text.
- **Steps.**
  - Each tool call is a `Trace` line with a kind icon and its name and title.
  - Status shows as a spinner, an error icon or a refusal icon. A call waiting on a request says "waiting for your answer".
  - Lines with detail are collapsible, showing input and output Code Blocks (diffs tinted) and errors in the destructive variant.
- **Grouping.** Consecutive calls form one collapsible "Worked: …" step (Chain of Thought).
- **Other blocks.** Thoughts are a collapsible "Thought" (Reasoning), and plans show "N of M done" (Plan).
- **Past permission requests** are one line each ("Asked for approval: … approved by a rule").
- **Notices** are small muted lines with a time, and an expandable body when there is one.
- **Pending requests** are Confirmation cards (`Item` plus `Button`s) at the end, with the command shown as code. They keep our approve flow:
  - the shared `usePermissionAnswer` hook holds it, and `PermissionCard` (Approvals) now uses the same hook, so there is one implementation;
  - another machine's request needs a confirm step on this device;
  - another machine's always-allow is blocked;
  - nothing is sent while that machine is offline;
  - "Already answered" is handled;
  - the request stays until the hub resolves it.
- **No duplicate requests.** A request that is still pending is not repeated as a history line. The panel's top pending strip also leaves out the open session's own requests.
- **Scrolling.** `MessageScroller` with `autoScroll` sticks to the bottom while events arrive, and stops when the reader scrolls up. Its button jumps back to the latest.
- **"Working…" line.** It shows while the agent works. It is not shown after a hook session said it is waiting for the person.
- **Header, tidied.** Pet, title, status, agent, machine (remote), source, and the folder chip now shows the repo name (full path on hover).
- **Hook sessions** keep the note that prompts and approvals stay in the terminal, reworded.

### 4. Chat view and pet bubble with the same Message components

Not done. The pet window must stay free of AI Elements, so the bubble cannot use them without breaking the bundle rule. The Chat view could; I left it for a later pass rather than risk it now.

## Additions in the same round (t13)

- **Bot provider settings (protocol fields added by t13).**
  - hub-client handles `provider` (now with `acp`), `openaiModel`, `geminiModel`, `understand`, `acpAgent` and `acpModel`. Each is sent only when it changed, and never to a hub without the field.
  - Write-only secret keys: `bot.openaiApiKey` and `bot.geminiApiKey`.
  - There are labels, `BOT_PROVIDERS` and `botProviderLabel` ("Claude Code (your Claude plan, no API key)", or the chosen agent's name).
  - Adding or replacing an Anthropic, OpenAI or Gemini key is a risky change. The in-app confirm says what leaves the machine (`cloudPrivacyText`); clearing a key needs no confirm.
  - Tests are included.
- **Settings > Bot.**
  - A provider picker, and for `acp`, a picker limited to this machine's installed agents, a model field, and the note about plan usage and tools being off.
  - The Anthropic, OpenAI and Gemini model fields and write-only key fields, plus Ollama.
  - The provider decides which fields show.
  - "Understand any wording" has a privacy note: when a cloud provider is used, your message plus a short summary of your sessions (titles, repo names, states, pending approvals; never file contents) is sent to that provider.
  - The default agent picker is kept.
  - `lib/settingsForms.ts` gains `BotForm` fields and validation (no spaces in model names; an agent is required for `acp`), with tests.
- **The default pet's bubble.**
  - When the pet's last reply is the bot's "Not sure what you mean…", three example chips appear and tapping one sends it.
  - The chips are fitted to the moment: "approve" when something waits, "what's everyone doing?", running the tests in the most recent session's repo, else "daily report" (`exampleChips`, `isNotSureReply`, tested).
  - There is no panel button.

## Files

**packages/hub-client**

- New: `src/injected.ts`, `src/transcript.ts`, `test/transcript.test.ts`.
- `src/settings.ts`: bot provider fields, keys, risk, labels.
- `src/index.ts`: exports.
- `test/settings.test.ts`.

**apps/desktop**

- New UI files:
  - `src/components/ui/{badge,bubble,button,code-block,collapsible,item,message,message-scroller,separator,trace}.tsx` (copied shadcn/ui, `"use client"` stripped);
  - `src/hooks/use-highlighted-code.ts` (lazy highlighter) and `src/lib/highlighter.tsx`.
- New panel files:
  - `src/panel/ai.css`;
  - `src/panel/usePermissionAnswer.ts`;
  - `src/panel/transcript/{Transcript,ConfirmationCard,CodeView,Markdown}.tsx` and `Transcript.test.ts`.
- Changed:
  - `src/panel/views/SessionView.tsx`: transcript and header;
  - `src/panel/views/PermissionCard.tsx`: now uses the shared hook;
  - `src/panel/views/PendingStrip.tsx`;
  - `src/panel/PanelApp.tsx`: lazy session view;
  - `src/panel/main.tsx`;
  - `src/panel/panel.css`: dark overrides and session list overflow;
  - `src/panel/settings.css`;
  - `src/styles/tokens.css`: dark palette and aliases;
  - `src/lib/format.ts`: `sessionTitle` uses `cleanTitle`;
  - `src/lib/settingsForms.ts` and its test;
  - `src/panel/views/settings/sections.tsx`: Settings > Bot;
  - `src/lib/bubble.ts` and its test: chips;
  - `src/pet/Bubble.tsx` and `src/pet/pet.css`: chips and clean session titles.
- Config: `vite.config.ts`, `tsconfig.json`, `package.json` and the root `bun.lock`.

**docs**

- `docs/screenshots/session-view-light.png`, `session-view-dark.png`: the hook session.
- `docs/screenshots/session-view-tools-light.png`: the work step expanded, with command output and a diff.
- `docs/screenshots/session-view-owned-light.png`, `session-view-owned-dark.png`: an owned session with a pending Confirmation.

## Commands and results

All were run in the worktree.

| Command | Result |
|---|---|
| `packages/hub-client`: `bunx tsc --noEmit -p .` | ok |
| `packages/hub-client`: `bun test` | 334 pass, 0 fail (after the review fixes) |
| `apps/desktop`: `bun run typecheck` (3 configs) | ok |
| `apps/desktop`: `bun test src` | 89 pass, 0 fail (10 files, with the transcript render smoke test) |
| `apps/desktop`: `bunx vite build` | ok |
| `apps/vscode`: `bunx tsc --noEmit -p .` | ok |
| `apps/vscode`: `bun test` | 23 pass |

The src-tauri crate was not changed in this task.

### Bundle sizes (vite build, minified)

"Before" is the last build of the one-pet work, earlier in this session (same config, before AI Elements).

| File | Before | After |
|---|---|---|
| pet entry JS | 18.91 kB (gz 7.00) | 23.03 kB (gz 8.43) |
| pet CSS | 9.12 kB | 10.12 kB |
| panel entry JS | 162.79 kB (gz 48.78) | 162.26 kB (gz 49.22) |
| shared chunk (React, hub-client, …) | 360.18 kB (gz 112.70) | 354.03 kB (gz 110.96) |
| panel CSS | 44.46 kB (gz 8.81) | 129.35 kB (gz 20.74) |
| SessionView chunk (lazy, new) | — | 662.74 kB (gz 202.07) |
| highlighter chunk (lazy, new; plus one small chunk per language and theme) | — | 153.24 kB (gz 49.77) |

- **Pet entry:** the growth comes from the one-pet review fixes (bubble state, proposal store, chips), not from AI Elements. The pet loads none of the new code.
- **Panel CSS:** the growth is mostly Streamdown's utility classes plus the Tailwind theme variables.
- **SessionView chunk:** Base UI, Streamdown and its markdown pipeline, Message Scroller, icons, and the transcript.
- **Highlighter:** shiki core and engine, loaded on the first code block.

### Screenshots

Taken with Playwright Chromium (1240×860; light and dark via `colorScheme`) from the production build served at `http://localhost:1420`.

- **The hub:** a throwaway one on a temp data dir and a random port, never the real hub data. It ran `E:\workspace\ai-pet\target\debug\pawl.exe` (built from main this morning); the worktree's crates are being changed by sde-brain.
- **The hook session:** Claude Code hook payloads sent through `pawl hook claude`, covering:
  - a prompt with an image marker and pasted text;
  - Bash, Read, Grep and Edit calls;
  - a reply in markdown;
  - a background-task notification;
  - `idle_prompt`.
- **The owned session:** a scripted ACP agent with thoughts, a plan, read, search, edit (diff) and test-run tool calls, a markdown reply, then a second turn left waiting on a permission request.
- The harness lives in my scratchpad (`shots/shoot.ts`, `shots/story-agent.ts`); nothing was added to the repo for it.
- The browser console shows a CORS refusal for `/health` from the dev origin. It is harmless and existed before: the panel connects over WebSocket.

## Review fixes (t13 review)

1. **IMPORTANT: an approval could hide the end of what is approved.**
   - Permission input is never cut. `requestInputCode` keeps all of it (`omittedLines` is always 0), and the block scrolls.
   - A new `requestCommandText(toolCall)` gives the whole command (else the whole input, else the title). The confirm step shows it, not only the agent's title, in all three places that approve:
     - the session's Confirmation card;
     - Approvals (`PermissionCard`): the `perm-raw` block is no longer cut at 1,600 characters;
     - the pet bubble: the command is shown under the title and in the confirm step.
   - Tests: an 80-line command ending in `curl … | sh` is kept whole; a long write's input is kept whole; the confirm text is the command.
2. **IMPORTANT: stripping injected text could hide real text.**
   - Only complete blocks are taken out. Every block taken out is shown:
     - `<system-reminder>` becomes a muted "Injected text" notice with its body collapsed;
     - a task notification keeps its full body collapsed.
   - An opening tag without its closing tag stays in the text as the person typed it.
   - The one exception is text the hub cut off (it ends with "…" or "…[truncated N chars]"). Then the block runs to the end and is still shown, marked "(cut off)".
   - A tag's attributes can no longer swallow the next tag.
   - A reminder inside pasted text stays in the pasted text.
   - `cleanTitle` uses the same rules, so a hub-cut `<task-notification>…` title still falls back to "\<agent\> in \<repo\>", while "why does \<hc\> appear" keeps its title.
   - Tests cover each case.
3. **Long text.** Agent replies, thoughts, pasted text and notice bodies show their first 40,000 characters and a "Show N more characters" button (`LongText.tsx`).
4. **Re-render cost.**
   - `reuseItems` and `reuseBlocks` (hub-client, tested) keep the object of every unchanged item and work step, using cheap string comparisons.
   - `SessionTranscript` keeps the previous result.
   - `Block` is `React.memo`, so a new event re-renders only what it touched.
5. **ACP agent picker.** An empty choice shows "Choose an agent…" (or "No agents installed") instead of seeming to pick the first agent. Saving still requires an agent.
6. **Tool status.** A late `PreToolUse`, or an ACP update out of order, never moves a finished call back to running. Tested for hook and ACP.
7. **Stable open state.** Open and closed state is remembered by item id for the whole view (`OpenState`). A tool line keeps its state when it moves into a "Worked: …" step, and a step keeps its state as it grows (its id is its first call's id).
8. **More risky changes.**
   - Switching the provider to a cloud provider that already has a key asks first ("Use OpenAI for the bot?", with what leaves the machine). This includes "Automatic", when it would pick a cloud provider with a key.
   - Turning "Understand any wording" on while a cloud provider is in use asks first.
   - Saving the key in the same change asks only once.
   - `cloudProviderInUse` is exported. Tests are included.
9. **Example chips.** Only one send at a time: the chips are disabled while one is sending.

Gates after the fixes:

| Package | Result |
|---|---|
| hub-client | tsc ok, 334 pass |
| desktop | 3 tsc configs ok, 89 pass, vite build ok |
| vscode | tsc ok, 23 pass |

Build sizes after the fixes:

| File | Size |
|---|---|
| pet entry JS | 23.18 kB |
| panel entry JS | 162.34 kB |
| shared chunk | 372.89 kB |
| SessionView (lazy) | 648.88 kB (gz 197.18) |
| panel CSS | 129.63 kB |

- The shared chunk grew by about 19 kB because the pet bubble now uses `requestCommandText`, so the transcript helpers moved into it. The pet still loads no AI Elements, Streamdown or shiki code (checked).
- The screenshots were taken again.

## Decisions

- **Base variant, preset base-nova**, copied through the CLI in a scratch project, so the repo's lockfile is only changed by `bun-locked.sh`.
- **Tailwind is layered without preflight**, with a scoped mini reset and shadcn variables only under `.ai-scope`, so the rest of the panel is untouched.
- **Dark mode follows the system,** for the whole panel and the pet bubble, through the existing tokens.
- **Markdown links do not navigate** (the panel is the app window). There is no `@streamdown/code`, math or mermaid plugin: fenced code goes to our Code Block.
- **Tool and request paths are shown relative to the session folder.** Searches show their pattern rather than the folder.
- **One approval implementation** (`usePermissionAnswer`), shared by the Approvals cards and the transcript's Confirmation.
- **A pending request shows once in the session view,** as its card. It is left out of the top strip and the history lines.

## Known gaps

- **Stored titles:** the hub still stores raw titles for old sessions. The display side cleans them; sde-brain is fixing the hub side.
- **Codex and Copilot hook sessions** are mapped on a best-effort basis from what the hub stores, and are tested only with fixture payloads.
- **Clipped prompts:** the hub cuts raw strings at 4,000 characters, so a very long prompt or output ends with "…[truncated N chars]" inside the text.
- **Sidebar sideways scroll:** when an element in the session list is scrolled into view programmatically (Playwright did this), the list could scroll sideways because a row was wider than the list. I set `overflow-x: hidden` on the list; the row's own width is unchanged.
- **Item 4 not done:** the Chat view and the session pet's bubble still use their own message markup.
- **Panel CSS size:** it grew by about 12 kB gzipped because Streamdown's classes are always generated. They could be limited with a safelist later.

## Follow-up: what "Auto" uses, and ACP limited to Claude Code

**What the bot uses now.** `botProviderNow(bot, agents, machineId)` is a new hub-client helper, with tests. It works from the saved settings.

- For "Auto" it picks the first provider that is set up, in this order:
  1. Anthropic, OpenAI, Gemini (each needs its key);
  2. Ollama (needs a URL and a model);
  3. the ACP agent, when it is installed on this machine.
- It returns the provider and one line, such as "Auto: using Claude Code on your Claude plan" or "Auto: using OpenAI (your API key)".
- When nothing is set up, the line says the pet answers with simple built-in replies.
- A chosen provider that is not set up says what it needs, for example "OpenAI needs its API key".

**Settings > Bot** shows that line under the Provider picker:

- It is highlighted in honey when it spends the person's plan, with "Every message to your pet uses your plan's usage".
- It notes "(Saved settings; save to apply your changes.)" while the form has unsaved edits.

**ACP agent picker, Claude Code only.** `ACP_BOT_AGENTS = ["claude-code"]` matches the hub (sde-brain).

- Claude Code is always listed, marked "(not installed)" when it is missing.
- Other installed agents are listed but disabled, with "(not supported yet)".
- Saving another agent is refused with "Only Claude Code can think for your pet for now." (tested).

**Gates:** hub-client 335 pass; desktop 3 tsc configs, 89 pass, vite build ok; vscode 23 pass.

**Not checked by eye:** the hub binary I use for screenshots predates the provider fields, so the panel hides these controls against it.

## Follow-up: scrollbars

The user said "scrollbar looks weird": the sessions list showed grey Windows scrollbars, a vertical one with arrows and a horizontal one, because long titles overflowed.

**Themed scrollbars, defined once.**

- `styles/tokens.css` defines the variables, so the panel, its AI Elements (`.ai-scope`) and the pet window all use them:
  - `--scrollbar-size`: 8px;
  - `--scrollbar-thumb`: a soft tan with transparency, lighter in dark mode;
  - `--scrollbar-thumb-hover`: darker;
  - `--scrollbar-track`: transparent.
- The `::-webkit-scrollbar` rules (WebView2 and WebKit) give a rounded thumb inset by a transparent border, no arrow buttons, a darker thumb on hover, and a transparent corner.
- Chromium ignores those rules on an element that sets the standard `scrollbar-width` / `scrollbar-color`. So the standard properties (`thin`, plus the same colours) are applied only under `@supports not selector(::-webkit-scrollbar)`, which covers Firefox-class engines.
- The shadcn `scrollbar-*` class names generate no CSS (neither Tailwind core nor shadcn defines them), so nothing overrides the theme inside `.ai-scope`.

**No sideways scrolling from text.**

- `.main-scroll`, the session transcript's viewport and the pet bubble's body: `overflow-x: hidden`, `min-width: 0`.
- `.sidebar`: `min-width: 0`, `overflow: hidden`.
- The session list scrolls only vertically, and only when it overflows.
- Session rows: `width: 100%`, `min-width: 0`, and the text column `flex: 1` and `overflow: hidden`. Titles and subtitles stay on one line ending in "…", with the full title as a tooltip on the row and on the session view's heading.
- Code blocks keep their own scroll inside the block.

**Checked in Playwright Chromium** against a throwaway hub with 14 long-titled sessions, with scrollbars shown:

- An audit of every element with `overflow-x: auto` or `scroll` (code blocks excluded) found nothing wider than its box on Home, a session, Settings or Approvals, in light or dark.
- Screenshots: `docs/screenshots/panel-sidebar-light.png` and `panel-sidebar-dark.png`.

**Gates:** desktop 3 tsc configs, 89 pass, vite build ok; hub-client 335 pass; vscode 23 pass.
