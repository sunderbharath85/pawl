import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { PetApp } from "./PetApp";
import "./pet.css";

// `vite dev` in a browser tab: ?voice=mock / ?voice=ready installs a stand-in voice engine (never in a build).
const devVoice = import.meta.env.DEV ? import("../lib/voiceMock").then((m) => m.installVoiceMockFromUrl()) : Promise.resolve();

void devVoice.finally(() =>
  createRoot(document.getElementById("root")!).render(
    <StrictMode>
      <PetApp />
    </StrictMode>,
  ),
);
