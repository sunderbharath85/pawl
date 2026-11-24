// @ai-pet/hub-client — framework-free core. Browser/webview/Node safe: no React, no Node built-ins here.
// React binding: "@ai-pet/hub-client/react". Node credential helpers: "@ai-pet/hub-client/node".
export {
  HubClient,
  HubRequestError,
  pickPermissionOption,
  type HubClientOptions,
} from "./client";
export {
  applyAck,
  applyWelcome,
  DEFAULT_LIMITS,
  initialHubState,
  lastEventsBySession,
  mergeEvents,
  reduceServerMsg,
  type ClientRequest,
  type DistributiveOmit,
  type HubConnectionStatus,
  type HubState,
  type StateLimits,
} from "./state";
export { DEFAULT_HUB_HTTP_URL, DEFAULT_HUB_WS_URL, toHubHttpUrl, toHubWsUrl } from "./url";
export { fetchHubHealth, type HubHealth } from "./health";
