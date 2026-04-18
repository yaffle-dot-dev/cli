/**
 * Local CLI copy of the Yaffle client layer so the public CLI repo is
 * self-contained.
 */

export { YaffleClient } from "./client.js"
export type { YaffleClientOptions, Logger } from "./client.js"

export { TokenAuth, DeviceFlowAuth, GitHubOIDCAuth } from "./auth.js"
export type { AuthProvider } from "./auth.js"

export {
  loadConfig,
  saveConfig,
  loadCredentials,
  saveCredentials,
  getCredentials,
  removeCredentials,
  getHost,
} from "./config.js"
export type { Config, StoredCredentials } from "./config.js"

export type {
  DependencyGraph,
  EnvironmentGroup,
  EnvironmentPreviewGroup,
  OrgInfo,
  Preview,
  PreviewOverviewResponse,
  PreviewStatus,
  ResourceSpan,
  TerraformOutput,
  Run,
  RunGroup,
  RunGroupSystemError,
  RunStatus,
  StreamUpdate,
  Target,
  WorkspacePreview,
  WorkspaceWithRuns,
  Credentials,
  DeviceCodeResponse,
  ApiResponse,
  ApiError,
} from "./types.js"
