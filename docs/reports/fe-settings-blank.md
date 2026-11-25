# fe-settings: blank hub window after a Settings toggle (0.1.1)

Report: "changing settings, turning on voice or A2A leads to a blank screen". Branch `fix/settings-blank`.

## Root cause (one, shared by both toggles)

It was not a crash. The DOM was all there; the window itself had scrolled the app out of view.

- `apps/desktop/src/panel/panel.css:1981` (0.1.1): `.toggle input { position: absolute; opacity: 0; pointer-events: none; }`
  hides the checkbox. `.toggle` (line 1971) was not positioned, and no ancestor up to the body was either, so the checkbox's
  containing block was the window. It sat at its unscrolled place in the long Settings page: about 2,600 px down for
  A2A and 6,200 px for Voice, in a 740 px window.
- Every settings switch is that `Toggle` (`panel/views/settings/shared.tsx`), including the A2A one ("Let other tools
  talk to Pawl", `A2aSection.tsx:85`) and the Voice one ("Talk to …", `VoiceSection.tsx:104`).
- Clicking the label focuses the checkbox, and the browser scrolls the window to bring it into view. `overflow: hidden`
  on the body stops the user from scrolling it, not the browser.
- The result: `.app` (100vh) moved to top -1903 px (measured), and only the dotted body background and the toast were left
  on screen.
- Toggles near the top of Settings were unaffected; any toggle below the first screen blanked the window. The mock-hub
  tests passed because they check the DOM, not what is on screen.
- The same happened in the real Tauri app (debug build under its own identifier, against a throwaway hub, driven over
  WebView2's remote debugging) and in Chromium with vite. The hub's settings replies and the voice command results
  have the shapes the UI expects; neither was involved.

## Fix

- `panel.css`: `.toggle { position: relative; }`, and the checkbox `inset: 0; margin: 0`. It now sits on its own
  toggle, inside the view's scroller.
- `lib/pinScroll.ts`, called in `panel/main.tsx`: the panel window never scrolls. On a window `scroll` event it goes
  straight back to (0, 0). This is a catch-all for any other hidden control positioned against the window.
- Error boundaries, so a real render crash can never blank the window:
  - `lib/ErrorBoundary.tsx`: the fallback can be a function `(error, reset)`, there is a `label` for the console line,
    and anything thrown is wrapped as an Error. Existing pet uses are unchanged.
  - `panel/CrashCard.tsx`: "Something went wrong in this section/view/window." with the error message and **Reload**.
  - `SectionGuard` (`settings/shared.tsx`) wraps each of the 16 settings sections, and its card keeps the section's
    anchor. `ViewGuard` (`PanelApp.tsx`) wraps the routed view, and `WindowGuard` (`main.tsx`) wraps the whole panel,
    where Reload reloads the window.

## Tests

- Unit tests (desktop, 172 pass):
  - `pinScroll.test.ts`;
  - `ErrorBoundary.test.ts` (non-Errors wrapped, the function fallback, reset, resetKey, node fallback);
  - `panel/toggleLayout.test.ts`, which pins that the hidden checkbox is positioned against `.toggle`;
  - `voice.test.ts`: `voice_status` and `voice_configure` exactly as the real Tauri app returned them (`mics: []` on a
    PC without models), folded through the reducer.
- Unit test (hub-client, 371 pass): `a2a.test.ts` has the hub's real `a2a` settings before and after the server
  toggle, as sent in the `update_settings` ack.
- E2E (`e2e/tests/ui.test.ts`, 7 pass):
  - The panel runs as the desktop app shows it: a fake Tauri IPC shell, so the production build takes its Tauri paths
    (hub credentials, home folder, voice engine with the real payloads).
  - The A2A server toggle (real hub) and the voice toggle keep the window at scroll 0 with `.app` at top 0. With the
    0.1.1 CSS forced back, the window still snaps back (the pin).
  - A section whose data makes it crash (voice status without `models`) shows the card with the error. The other
    sections and the sidebar still render, and Reload brings the section back.
  - Checked against the old code: with the CSS and the pin both reverted, the test fails exactly like the bug (window
    scrolled 2307 px). With only the CSS reverted, the pin keeps it passing.

## Gates

- desktop: typecheck (3 configs), 172 pass, vite build (no dev mocks in dist);
- hub-client: tsc, 371 pass;
- e2e ui: 7 pass.
- No Rust was touched, so there was no cargo test or clippy run.
