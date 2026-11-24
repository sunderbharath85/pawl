// First-run setup (PLAN.md migration step 5b): opens once by itself, reopened from Settings → This app.
// Hub running? (start it) → name this machine → install hooks for the agents found (preview first) → optionally
// turn on the peer listener and pair a machine.
import { useEffect, useState } from "react";
import { machineDisplayName, pairingAllowsText } from "@krypto/hub-client";
import { isTauri, native } from "../../lib/native";
import { useSettings } from "../../lib/settings";
import { usePanel } from "../context";
import { PetAvatar } from "../PetAvatar";
import { HooksPanel } from "./settings/AgentsSection";
import { MachineNameForm } from "./settings/AppSections";
import { ListenerControl } from "./settings/PairingSection";
import { useHubSettings } from "./settings/shared";

const STEPS = ["Hub", "Name", "Hooks", "Pair"] as const;
type Step = 0 | 1 | 2 | 3 | 4;

export function SetupView() {
  const { meta, navigate, state } = usePanel();
  const [, setSettings] = useSettings();
  const [step, setStep] = useState<Step>(0);
  const connected = meta.phase === "connected";

  // Shown once by itself: opening it counts.
  useEffect(() => setSettings({ setupOffered: true }), [setSettings]);

  const finish = (then: () => void) => {
    setSettings({ setupOffered: true, setupDone: true });
    then();
  };
  const skip = () => {
    setSettings({ setupOffered: true });
    navigate({ view: "home" });
  };
  const next = () => setStep((s) => Math.min(4, s + 1) as Step);
  const back = () => setStep((s) => Math.max(0, s - 1) as Step);

  return (
    <section className="view setup-view">
      <header className="view-head setup-head">
        <PetAvatar species="dog" activity={step === 4 ? "happy" : connected ? "idle" : "napping"} size={64} />
        <div>
          <h1>Set up Krypto</h1>
          <p>A few steps so your pets can watch your coding agents. Everything here can be changed later in Settings.</p>
        </div>
      </header>

      <ol className="setup-steps" aria-label="Setup steps">
        {STEPS.map((label, i) => (
          <li key={label} className={i === step ? "is-current" : i < step ? "is-done" : undefined} aria-current={i === step ? "step" : undefined}>
            <span className="setup-step-num">{i < step ? "✓" : i + 1}</span>
            {label}
          </li>
        ))}
      </ol>

      <div className="card setup-card">
        {step === 0 && <HubStep />}
        {step === 1 && (
          <>
            <h2>Name this machine</h2>
            <p className="hint">Your other machines and the pets' name tags use it, e.g. “Windows PC” or “Work laptop”.</p>
            <MachineNameForm autoFocus onSaved={() => next()} />
          </>
        )}
        {step === 2 && (
          <>
            <h2>Hooks for the agents on this machine</h2>
            <p className="hint">
              Hooks let Krypto follow sessions you start yourself in a terminal or editor. Preview shows the exact change to each agent's
              config before anything is written.
            </p>
            <HooksPanel live={connected} foundOnly />
          </>
        )}
        {step === 3 && <PairStep />}
        {step === 4 && (
          <>
            <h2>All set{state.machine ? `, ${machineDisplayName(state.machine)}` : ""}!</h2>
            <p className="hint">Start a session from the panel, or keep using your agents as usual; their pets will show up on the strip.</p>
            <div className="form-actions">
              <button className="pill pill-primary" onClick={() => finish(() => navigate({ view: "new" }))}>
                Start a session
              </button>
              <button className="pill" onClick={() => finish(() => navigate({ view: "home" }))}>
                Go to Home
              </button>
            </div>
          </>
        )}
      </div>

      <div className="setup-nav">
        <button className="link" onClick={skip}>
          Skip setup
        </button>
        <span className="spacer" />
        {step > 0 && step < 4 && (
          <button className="pill" onClick={back}>
            Back
          </button>
        )}
        {step === 3 && (
          <button className="pill" onClick={() => finish(() => navigate({ view: "machines" }))} disabled={!connected}>
            Pair a machine now
          </button>
        )}
        {step < 4 && (
          <button className="pill pill-primary" onClick={next} disabled={step === 0 && !connected}>
            {step === 3 ? "Finish" : step === 1 || step === 2 ? "Next" : "Continue"}
          </button>
        )}
      </div>
    </section>
  );
}

function HubStep() {
  const { meta, conn } = usePanel();
  const [hubBin, setHubBin] = useState<string | null | undefined>(undefined);
  useEffect(() => {
    if (!isTauri) return;
    native.locateHub().then(setHubBin).catch(() => setHubBin(null));
  }, [meta.phase]);

  return (
    <>
      <h2>The hub</h2>
      <p className="hint">The hub is the small background program (krypto serve) that talks to your agents. It only listens on this computer.</p>
      {meta.phase === "connected" ? (
        <p className="setup-ok">
          The hub is running{meta.hubVersion ? ` (krypto ${meta.hubVersion})` : ""}. You're connected.
        </p>
      ) : meta.phase === "starting" || meta.phase === "connecting" || meta.phase === "booting" ? (
        <p className="hint">
          <span className="spinner inline-spinner" aria-hidden /> {meta.phase === "starting" ? "Starting the hub…" : "Looking for the hub…"}
        </p>
      ) : meta.phase === "unauthorized" ? (
        <p className="field-error">The running hub did not accept this app's token. Restart the hub, then try again.</p>
      ) : (
        <>
          <p>The hub is not running yet.</p>
          {meta.startError && (
            <p className="field-error" role="alert">
              {meta.startError}
            </p>
          )}
          {hubBin === null && <p className="hint">krypto was not found: put it on PATH or next to this app, or set KRYPTO_HUB_BIN.</p>}
          <div className="form-actions">
            <button className="pill pill-primary" onClick={() => void conn.startHub()} disabled={!isTauri}>
              Start the hub
            </button>
            <button className="pill" onClick={() => void conn.refresh(true)}>
              Check again
            </button>
          </div>
        </>
      )}
    </>
  );
}

function PairStep() {
  const hs = useHubSettings();
  return (
    <>
      <h2>Pair another machine (optional)</h2>
      <p className="hint">
        Paired machines see each other's sessions, pets and approvals. To invite one from here, turn on the peer listener; to join an
        invite made elsewhere, you don't need it. You can do this any time in Machines.
      </p>
      <p className="pairing-allows">{pairingAllowsText(hs.settings?.peer.acceptIntents)} You can change that in Settings → Remote control.</p>
      {hs.settings ? (
        <ListenerControl settings={hs.settings} live={hs.connected} compact />
      ) : (
        <p className="hint">{hs.connected ? (hs.loadError ?? "Loading…") : "Connect the hub first."}</p>
      )}
    </>
  );
}
