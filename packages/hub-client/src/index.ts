// @krypto/hub-client — framework-free core. Browser/webview/Node safe: no React, no Node built-ins here.
// React binding: "@krypto/hub-client/react". Node credential helpers: "@krypto/hub-client/node".
export {
  HubClient,
  HubRequestError,
  JOIN_PEER_TIMEOUT_MS,
  MACHINE_NAME_MAX,
  normalizeMachineName,
  pickPermissionOption,
  type HubClientOptions,
  type RequestOptions,
} from "./client";
export {
  applyAck,
  applyWelcome,
  DEFAULT_LIMITS,
  initialHubState,
  lastEventsBySession,
  latestEventSeq,
  machineCounts,
  mergeEvents,
  reduceServerMsg,
  removeDevice,
  scopeToMachine,
  sortedMachines,
  upsertDevice,
  upsertMachine,
  type ClientRequest,
  type DistributiveOmit,
  type HubConnectionStatus,
  type HubState,
  type StateLimits,
} from "./state";
export {
  extractInviteUri,
  INVITE_TTL_DEFAULT_SECS,
  INVITE_TTL_MAX_SECS,
  INVITE_TTL_MIN_SECS,
  INVITE_URI_PREFIX,
  isUnspecifiedAddr,
  parseInviteUri,
  type InviteInfo,
} from "./peer";
export { DEFAULT_HUB_HTTP_URL, DEFAULT_HUB_WS_URL, toHubHttpUrl, toHubWsUrl } from "./url";
export { fetchHubHealth, type HubHealth } from "./health";
