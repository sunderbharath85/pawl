// A stand-in for the desktop's screen commands in `vite dev` (?screen=mock): a few made-up screens and windows drawn on
// a canvas, so the look flow can be built and screenshotted without Tauri. It follows the screen gate's rules
// (src-tauri/src/screen/gate.rs): a picker session (nonce) from `begin`, ids only from the latest list, at most 5 lists
// and 3 captures per session, `end` closes it; with the gate's own messages. Dev only (see pet/main.tsx).
import { SCREEN_OPEN_AGAIN, SCREEN_PICK_AGAIN, SCREEN_SLOW_DOWN, setScreenBackend, type ScreenBackend, type ScreenCapture, type ScreenSource } from "./screen";

const SOURCES: Array<Omit<ScreenSource, "thumb"> & { color: string; lines: string[] }> = [
  { id: "screen:1", kind: "screen", title: "Screen 1", app: "Display", color: "#2d5b8a", lines: ["Editor", "Terminal", "Browser"] },
  { id: "win:code", kind: "window", title: "greet.ts - acme-web", app: "Code", color: "#1e1e1e", lines: ["export function welcome() {", "  return 'hi';", "}"] },
  { id: "win:term", kind: "window", title: "bun test", app: "Windows Terminal", color: "#0c0c0c", lines: ["FAIL test/greet.test.ts", "expected 'hello' got 'hi'", "1 fail, 11 pass"] },
  { id: "win:web", kind: "window", title: "Pull request #42", app: "Chrome", color: "#f4f1ea", lines: ["CI: 1 check failing", "build / test (ubuntu)", "Details"] },
];

function draw(src: (typeof SOURCES)[number], w: number, h: number): string {
  if (typeof document === "undefined") return "AAAA";
  const c = document.createElement("canvas");
  c.width = w;
  c.height = h;
  const g = c.getContext("2d")!;
  g.fillStyle = src.color;
  g.fillRect(0, 0, w, h);
  g.fillStyle = "rgba(255,255,255,0.12)";
  g.fillRect(0, 0, w, Math.max(6, h * 0.08));
  const light = src.color === "#f4f1ea";
  g.fillStyle = light ? "#3a2a1f" : "#e8e2d6";
  g.font = `${Math.max(7, Math.round(h * 0.09))}px ui-monospace, monospace`;
  src.lines.forEach((line, i) => g.fillText(line, w * 0.06, h * (0.28 + i * 0.18)));
  return c.toDataURL("image/png").split(",")[1]!;
}

export interface ScreenMockOptions {
  /** Wait this long before answering (ms). */
  delayMs?: number;
}

export function createScreenMock(opts: ScreenMockOptions = {}): ScreenBackend & { open(): string | null; begun(): number } {
  const delay = opts.delayMs ?? 150;
  let session: { nonce: string; listed: string[]; lists: number; captures: number } | null = null;
  let seq = 0;
  const wait = () => new Promise((r) => setTimeout(r, delay));
  const current = (nonce: string) => {
    if (!session || session.nonce !== nonce) throw new Error(SCREEN_OPEN_AGAIN);
    return session;
  };
  return {
    open: () => session?.nonce ?? null,
    /** How many picker sessions were begun (the real gate allows one a second). */
    begun: () => seq,
    async begin() {
      session = { nonce: `mock-${++seq}`, listed: [], lists: 0, captures: 0 };
      return session.nonce;
    },
    async sources(nonce) {
      await wait();
      const s = current(nonce);
      if (++s.lists > 5) throw new Error(SCREEN_SLOW_DOWN);
      s.listed = SOURCES.map((x) => x.id);
      return SOURCES.map(({ color: _c, lines: _l, ...x }) => ({ ...x, thumb: draw(SOURCES.find((y) => y.id === x.id)!, 160, 100) }));
    },
    async capture(nonce, id): Promise<ScreenCapture> {
      await wait();
      const s = current(nonce);
      if (!s.listed.includes(id)) throw new Error(SCREEN_PICK_AGAIN);
      if (++s.captures > 3) throw new Error(SCREEN_SLOW_DOWN);
      return { mediaType: "image/png", data: draw(SOURCES.find((x) => x.id === id)!, 960, 600), width: 960, height: 600 };
    },
    async end(nonce) {
      if (session?.nonce === nonce) session = null;
    },
  };
}

export function installScreenMockFromUrl(): void {
  if (new URLSearchParams(window.location.search).get("screen") !== "mock") return;
  const mock = createScreenMock();
  setScreenBackend(mock);
  (window as unknown as { __pawlScreenMock: typeof mock }).__pawlScreenMock = mock;
}
