import { createContext, useContext } from "react";
import type { HubClient, HubState } from "@pawl/hub-client";
import type { PetSpritesConfig, PetView } from "@pawl/pet-engine";
import type { ConnectionMeta, HubConnection } from "../lib/connection";

export type Route =
  | { view: "home" }
  | { view: "session"; sessionId: string }
  | { view: "new" }
  | { view: "chat"; petId?: string }
  | { view: "approvals"; requestId?: string }
  | { view: "rules" }
  | { view: "settings"; section?: string | undefined }
  | { view: "machines" }
  | { view: "setup" };

export interface PanelCtx {
  conn: HubConnection;
  client: HubClient;
  meta: ConnectionMeta;
  state: HubState;
  views: PetView[];
  sprites: PetSpritesConfig | null;
  now: number;
  route: Route;
  navigate(route: Route): void;
  /** Show a transient message at the bottom of the panel. */
  toast(text: string, tone?: "info" | "error"): void;
}

export const PanelContext = createContext<PanelCtx | null>(null);

export function usePanel(): PanelCtx {
  const ctx = useContext(PanelContext);
  if (!ctx) throw new Error("usePanel outside <PanelContext>");
  return ctx;
}
