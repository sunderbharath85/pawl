import { usePanel } from "./context";
import { PetAvatar } from "./PetAvatar";

/** Shown whenever we're not connected: explains why and offers to start `pet-hub serve`. */
export function HubBanner() {
  const { meta, conn, toast } = usePanel();
  if (meta.phase === "connected") return null;

  const wake = () => {
    conn.startHub().catch((err) => toast(String(err), "error"));
  };

  let title = "Looking for the hub…";
  let body = `Connecting to ${meta.url}`;
  let action = false;
  switch (meta.phase) {
    case "booting":
      break;
    case "hub-down":
      title = "The hub is napping";
      body = meta.startError ?? `Nothing answers at ${meta.url}/health. Wake it to track and start agent sessions.`;
      action = true;
      break;
    case "starting":
      title = "Waking the hub…";
      body = "Starting pet-hub serve in the background.";
      break;
    case "connecting":
      title = "Sniffing for the hub…";
      body = `The hub is up; saying hello at ${meta.url}.`;
      break;
    case "unauthorized":
      title = "The hub did not accept our token";
      body =
        "The token in the hub data folder (or AI_PET_HUB_TOKEN) does not match the running hub. Restart the hub, then retry.";
      action = true;
      break;
  }

  const activity = meta.phase === "starting" ? "thinking" : meta.phase === "unauthorized" ? "sad" : "napping";

  return (
    <div className={`hub-banner phase-${meta.phase}`} role="status">
      <PetAvatar species="dog" activity={activity} size={54} />
      <div className="hub-banner-text">
        <strong>{title}</strong>
        <span>{body}</span>
        {meta.detail && meta.phase === "hub-down" && !meta.startError && (
          <span className="hub-banner-detail">{meta.detail}</span>
        )}
      </div>
      {action && (
        <div className="hub-banner-actions">
          {meta.phase === "hub-down" && (
            <button className="pill pill-primary" onClick={wake}>
              Start hub
            </button>
          )}
          <button className="pill" onClick={() => void conn.refresh(true)}>
            Retry
          </button>
        </div>
      )}
      {meta.phase === "starting" && <span className="spinner" aria-hidden />}
    </div>
  );
}
