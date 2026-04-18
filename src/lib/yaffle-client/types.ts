/**
 * Yaffle API types.
 */

export interface Preview {
  id: string
  repo: string
  prNumber: number | null
  environmentKind?: string | null
  environmentName?: string | null
  workspacePath: string
  ref?: string
  headSha?: string
  authorGithubId?: number | null
  authorLogin?: string | null
  status: PreviewStatus
  stateKey?: string
  mode?: string
  requireApproval?: boolean
  approvers?: string[] | null
  createdAt: string
  updatedAt?: string
}

export type PreviewStatus =
  | "pending"
  | "planning"
  | "planned"
  | "applying"
  | "awaiting_approval"
  | "ready"
  | "failed"
  | "destroying"
  | "destroyed"
  | (string & {})

export interface TerraformOutput {
  value: unknown
  type?: string
  sensitive?: boolean
}

export interface Run {
  id: string
  previewId: string
  runGroupId: string | null
  runType: "plan" | "apply" | "destroy" | (string & {})
  status: RunStatus
  checkRunId: number | null
  planSummary: string | null
  outputs: unknown
  errorMessage: string | null
  logOutput?: string | null
  startedAt: string | null
  completedAt: string | null
  createdAt: string
  durationMs?: number | null
}

export type RunStatus =
  | "pending"
  | "running"
  | "success"
  | "failed"
  | "cancelled"
  | (string & {})

export interface OrgInfo {
  id: string
  name: string
  slug: string
  role: string
  source: string
}

export interface DependencyGraph {
  workspaces: string[]
  edges: Array<[string, string]>
}

export interface RunGroupSystemError {
  kind: "config"
  title: string
  summary: string
  filePath: string
  line: number | null
  column: number | null
  excerpt: Array<{
    lineNumber: number
    text: string
    highlight: boolean
  }>
}

export interface RunGroup {
  id: string
  repo: string
  prNumber: number | null
  ref: string
  headSha: string
  trigger: string
  status: string
  dependencyGraph: DependencyGraph | null
  systemError: RunGroupSystemError | null
  createdAt: string
  startedAt: string | null
  completedAt: string | null
}

export interface WorkspacePreview {
  id: string
  workspacePath: string
  status: string
  connectionStatus: "ready" | "missing" | "conflict" | "not_required"
  missingProviders: string[]
  conflictProviders: string[]
  matchedConnections: Array<{ id: string; name: string; provider: string }>
  blockedReason: string | null
  stateKey: string
  mode: string
  requireApproval: boolean
  createdAt: string
}

export interface ResourceSpan {
  id: string
  resourceAddress: string
  resourceType: string | null
  action: string
  status: string
  startedAt: string
  completedAt: string | null
  durationMs: number | null
}

export interface WorkspaceWithRuns {
  preview: WorkspacePreview
  runs: Run[]
  outputs: unknown | null
  resourceSpans?: ResourceSpan[]
}

export interface EnvironmentGroup {
  repo: string
  ref: string
  environmentName: string
  headSha: string
  status: string
  updatedAt: string
  workspaces: Array<{
    previewId: string
    workspacePath: string
    status: string
    connectionStatus: "ready" | "missing" | "conflict" | "not_required"
    missingProviders: string[]
    conflictProviders: string[]
    matchedConnections: Array<{ id: string; name: string; provider: string }>
    blockedReason: string | null
    headSha: string
    lastRunId: string | null
    lastRunType: string | null
    lastRunStatus: string | null
    lastRunCompletedAt: string | null
    planSummary: string | null
  }>
}

export interface EnvironmentPreviewGroup {
  org: string
  repo: string
  environmentKind: "named" | "transient"
  environmentName: string
  ref: string
  headSha: string
  prNumber: number | null
  authorGithubId: number | null
  authorLogin: string | null
  workspaces: WorkspaceWithRuns[]
  runGroups: RunGroup[]
}

export interface PreviewOverviewResponse {
  data: Preview[]
  dependencyGraphs: Record<string, DependencyGraph>
  nextCursor: string | null
}

export interface StreamUpdate {
  preview: Preview | null
  runs: Run[]
  outputs: Record<string, TerraformOutput> | null
}

export type Target =
  | { type: "pr"; prNumber: number }
  | { type: "env"; name: string }

export interface Credentials {
  accessToken: string
  refreshToken?: string
  expiresAt?: number
}

export interface DeviceCodeResponse {
  deviceCode: string
  userCode: string
  verificationUri: string
  verificationUriComplete: string
  expiresIn: number
  interval: number
}

export interface ApiResponse<T> {
  data: T
}

export interface ApiError {
  error: {
    code: string
    message: string
  }
}
