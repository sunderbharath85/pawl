import type { PanelView } from "@krypto/protocol";
import type { Route } from "./context";

/** The route for a view another client asked for (`open_panel`, e.g. "Krypto: Open Settings" in VS Code). */
export function routeForView(view: PanelView): Route {
  switch (view) {
    case "home":
    case "settings":
    case "machines":
    case "approvals":
    case "pets":
    case "rules":
    case "chat":
    case "setup":
      return { view };
    default:
      // A view from a newer client: show home rather than nothing.
      return { view: "home" };
  }
}
