import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { PanelApp } from "./PanelApp";
import "./panel.css";
import "./settings.css";
import "./ai.css";

// `vite dev` in a browser tab: ?voice=mock / ?voice=ready installs a stand-in voice engine (never in a build).
const devVoice = import.meta.env.DEV ? import("../lib/voiceMock").then((m) => m.installVoiceMockFromUrl()) : Promise.resolve();

void devVoice.finally(() =>
  createRoot(document.getElementById("root")!).render(
    <StrictMode>
      <PanelApp />
    </StrictMode>,
  ),
);
