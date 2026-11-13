export {
  resourceHandle,
  runtimeProcessHandle,
  type ResourceHandle,
  type RuntimeProcessHandle,
} from "./handles.js";
export {
  privateEndpoint,
  type EndpointAuthority,
  type EndpointKind,
  type Platform,
  type PlatformPort,
  type PrivateEndpoint,
} from "./platform-port.js";
export type { RuntimeContainmentPort } from "./containment.js";
export {
  REMOTE_CHANNELS,
  type RemoteApi,
  type RemoteConnectionProfile,
  type RemoteConnectionStatus,
  type RemoteCredentials,
  type RemoteDirectoryList,
  type RemoteInventory,
  type RemoteNetworkMode,
  type RemotePreparationStage,
} from "./remote-contract.js";
