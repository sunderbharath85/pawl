import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { configurePetStyles } from "@pawl/pet-engine/react";
import { App } from "./App";
import { cspNonce } from "./vscodeApi";

// The webview CSP only allows <style> elements carrying this document's nonce; pet-engine injects
// its stylesheet once, so hand it the nonce before anything renders.
const nonce = cspNonce();
if (nonce) configurePetStyles({ nonce });

const root = document.getElementById("root");
if (root) {
  createRoot(root).render(
    <StrictMode>
      <App />
    </StrictMode>,
  );
}
