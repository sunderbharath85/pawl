// @krypto/hub-client — framework-free core. Browser/webview/Node safe: no React, no Node built-ins here.
// React binding: "@krypto/hub-client/react". Node credential helpers: "@krypto/hub-client/node".
export {
  HubClient,
  HubRequestError,
  JOIN_PEER_TIMEOUT_MS,
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
  activeMachineId,
  agentFor,
  checkMachineName,
  groupByMachine,
  isLocal,
  MACHINE_NAME_MAX,
  machineDisplayName,
  machinesLine,
  machineSummary,
  normalizeMachineName,
  OS_LABEL,
  petFor,
  readOnlyReason,
  renameErrorText,
  UNKNOWN_MACHINE_NAME,
  type MachineGroup,
  type MachineNameCheck,
} from "./machine";
export { checkPetName, PET_NAME_MAX, petNameMessage, petRenameErrorText, type PetNameCheck, type PetNameError } from "./petNames";
export {
  BACKFILL_WINDOW,
  backfillSession,
  SessionBackfill,
  sessionBackfill,
  type BackfillResult,
  type EventSource,
  type FetchEvents,
} from "./backfill";
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
