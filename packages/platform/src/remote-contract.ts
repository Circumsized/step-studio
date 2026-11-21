/**
 * The remote-connection contract, shared by the renderer and whichever shell
 * implements it.
 *
 * Moved here from `apps/desktop/src/remote/shared.ts` when Electron was deleted.
 * It is 58 lines with no Electron import — a type surface and a channel table —
 * but the renderer imported it by relative path from eight files, so leaving it
 * behind would have made `apps/desktop` undeletable. The SSH implementation that
 * used it stayed with Electron; only the contract moved.
 */

import type { ClientBootstrap, ClientCommandAccepted, ClientCommandRequest, ClientEvent, ClientQueryRequest, ClientQueryResponse, CommandName, QueryName, WorkspaceListReadModel } from "@step-studio/client-contract";

export const REMOTE_CHANNELS = {
  installRuntime: "step-studio:remote:install-runtime",
  approve: "step-studio:remote:approve", retryNetwork: "step-studio:remote:retry-network",
  list: "step-studio:remote:list", save: "step-studio:remote:save", remove: "step-studio:remote:remove", connect: "step-studio:remote:connect", disconnect: "step-studio:remote:disconnect", select: "step-studio:remote:select",
  bootstrap: "step-studio:remote:bootstrap", query: "step-studio:remote:query", command: "step-studio:remote:command", directories: "step-studio:remote:directories", register: "step-studio:remote:register", event: "step-studio:remote:event", changed: "step-studio:remote:changed",
} as const;
export type RemoteNetworkMode = "remote-first" | "remote-only" | "local-only";
export type RemotePreparationStage = "probe" | "confirmation" | "download" | "upload" | "install" | "verify" | "configure" | "connect";
export interface RemoteConnectionProfile {
  id: string; name: string; host: string; user?: string; port?: number;
  /** Detected information retained for display; never a user-supplied requirement. */
  platform?: "linux" | "win32" | "darwin";
  arch?: string; authMode?: "agent" | "password";
  installRoot?: string; networkMode?: RemoteNetworkMode; localProxyUrl?: string;
  installationApproved?: boolean; lastDirectory?: string;
}
export interface RemoteCredentials { password: string; }
export interface RemoteConnectionStatus {
  id: string;
  state: "disconnected" | "connecting" | "awaiting-approval" | "recovering" | "degraded" | "synchronizing" | "connected" | "failed";
  message?: string;
  arch?: string;
  generation?: number;
  attempt?: number;
  maxAttempts?: number;
  retryAt?: number;
  hostRestarted?: boolean;
  operation?: "installing-runtime";
  stage?: RemotePreparationStage;
  platform?: "linux" | "win32" | "darwin";
  installRoot?: string;
  receivedBytes?: number;
  totalBytes?: number;
  networkRoute?: "remote" | "local";
  networkMessage?: string;
}
export interface RemoteInventory { profiles: RemoteConnectionProfile[]; statuses: RemoteConnectionStatus[]; activeId?: string | null; }
export interface RemoteDirectoryList { path: string; parent: string; directories: { name: string; path: string }[]; }
export interface RemoteApi {
  list(): Promise<RemoteInventory>;
  save(profile: Omit<RemoteConnectionProfile, "id"> & { id?: string }): Promise<RemoteConnectionProfile>;
  remove(id: string): Promise<void>;
  connect(id: string, credentials?: RemoteCredentials): Promise<void>;
  approveDeployment(id: string): Promise<void>;
  retryNetwork(id: string): Promise<void>;
  installRuntime(id: string): Promise<{ installed: boolean }>;
  disconnect(id: string): Promise<void>;
  select(id: string | null): Promise<void>;
  bootstrap(id: string): Promise<ClientBootstrap>;
  query<T extends QueryName>(id: string, request: ClientQueryRequest<T>): Promise<ClientQueryResponse<T>>;
  command<T extends CommandName>(id: string, request: ClientCommandRequest<T>): Promise<ClientCommandAccepted<T>>;
  directories(id: string, path?: string): Promise<RemoteDirectoryList>;
  register(id: string, path: string): Promise<WorkspaceListReadModel>;
  onEvent(listener: (event: { id: string; generation: number; event: ClientEvent }) => void): () => void;
  onChanged(listener: () => void): () => void;
}
