/**
 * ./index.js - Yaffle API client
 *
 * Usage:
 *
 * ```ts
 * import { YaffleClient, TokenAuth, DeviceFlowAuth } from "./index.js"
 *
 * // With static token (CI/CD)
 * const client = new YaffleClient({
 *   apiUrl: "https://yaffle.local:6969",
 *   auth: new TokenAuth(process.env.YAFFLE_TOKEN),
 * })
 *
 * // With device flow (CLI)
 * const auth = new DeviceFlowAuth("https://yaffle.local:6969", "yaffle-cli")
 * const { userCode, verificationUri } = await auth.initiate()
 * console.log(`Go to ${verificationUri} and enter code: ${userCode}`)
 * // ... poll until approved ...
 *
 * // Get outputs
 * const result = await client.getOutputs({
 *   org: "myorg",
 *   repo: "myrepo",
 *   target: { type: "pr", prNumber: 123 },
 *   workspace: "apps/infra",
 *   wait: true,
 * })
 * ```
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
