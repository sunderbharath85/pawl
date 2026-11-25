import type { PanelView } from "@pawl/protocol";
import type { Route } from "./context";

/** The route for a view another client asked for (`open_panel`, e.g. "Pawl: Open Settings" in VS Code). */
export function routeForView(view: PanelView): Route {
  switch (view) {
    case "pets":
      // Agent pets are renamed under Settings > Pets (advanced): only the default pet is on screen.
      return { view: "settings", section: "pets" };
    case "home":
    case "settings":
    case "machines":
    case "approvals":
    case "rules":
    case "chat":
    case "setup":
      return { view };
    default:
      // A view from a newer client: show home rather than nothing.
      return { view: "home" };
  }
}
