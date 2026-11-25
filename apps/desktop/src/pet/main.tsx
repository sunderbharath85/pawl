import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { PetApp } from "./PetApp";
import "./pet.css";

// `vite dev` in a browser tab: ?voice=mock / ?voice=ready installs a stand-in voice engine, ?screen=mock stand-in
// screens and windows (never in a build).
const devVoice = import.meta.env.DEV
  ? Promise.all([import("../lib/voiceMock").then((m) => m.installVoiceMockFromUrl()), import("../lib/screenMock").then((m) => m.installScreenMockFromUrl())])
  : Promise.resolve();

void devVoice.finally(() =>
  createRoot(document.getElementById("root")!).render(
    <StrictMode>
      <PetApp />
    </StrictMode>,
  ),
);
