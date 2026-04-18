/**
 * Yaffle API client.
 */

import { EventSource } from "eventsource"
import type { AuthProvider } from "./auth.js"
import type {
  ApiResponse,
  EnvironmentGroup,
  EnvironmentPreviewGroup,
  OrgInfo,
  Preview,
  PreviewOverviewResponse,
  PreviewStatus,
  ResourceSpan,
  Run,
  StreamUpdate,
  Target,
  TerraformOutput,
} from "./types.js"

export interface YaffleClientOptions {
  /** Yaffle API base URL */
  apiUrl: string
  /** Authentication provider */
  auth: AuthProvider
  /** Logger for debug output */
  logger?: Logger
}

export interface Logger {
  info: (msg: string) => void
  warn: (msg: string) => void
  error: (msg: string) => void
}

const defaultLogger: Logger = {
  info: (msg) => console.log(msg),
  warn: (msg) => console.warn(msg),
  error: (msg) => console.error(msg),
}

export class YaffleClient {
  private apiUrl: string
  private auth: AuthProvider
  private log: Logger

  constructor(options: YaffleClientOptions) {
    this.apiUrl = options.apiUrl.replace(/\/$/, "") // Remove trailing slash
    this.auth = options.auth
    this.log = options.logger ?? defaultLogger
  }

  /**
   * Make an authenticated API request
   */
  private async request<T>(
    path: string,
    options: RequestInit = {}
  ): Promise<T> {
    const credentials = await this.auth.getCredentials()

    const response = await fetch(`${this.apiUrl}${path}`, {
      ...options,
      headers: {
        Authorization: `Bearer ${credentials.accessToken}`,
        Accept: "application/json",
        ...options.headers,
      },
    })

    if (!response.ok) {
      const text = await response.text()
      throw new Error(`API request failed: ${response.status} ${text}`)
    }

    return response.json()
  }

  private async requestText(
    path: string,
    options: RequestInit = {},
  ): Promise<string> {
    const credentials = await this.auth.getCredentials()

    const response = await fetch(`${this.apiUrl}${path}`, {
      ...options,
      headers: {
        Authorization: `Bearer ${credentials.accessToken}`,
        Accept: "text/plain, application/json",
        ...options.headers,
      },
    })

    if (!response.ok) {
      const text = await response.text()
      throw new Error(`API request failed: ${response.status} ${text}`)
    }

    return response.text()
  }

  /**
   * Find a preview by PR number or environment
   */
  async findPreview(
    org: string,
    repo: string,
    target: Target,
    workspace: string
  ): Promise<Preview | null> {
    const params = new URLSearchParams({
      org,
      repo,
      workspace_path: workspace,
    })

    if (target.type === "pr") {
      params.set("pr_number", String(target.prNumber))
    } else {
      params.set("environment", target.name)
    }

    const data = await this.request<ApiResponse<Preview[]>>(
      `/api/previews?${params}`
    )

    return data.data?.[0] || null
  }

  /**
   * Get outputs for a preview by fetching the full workspace data
   */
  async getPreviewOutputs(
    org: string,
    repo: string,
    target: Target,
    workspace: string
  ): Promise<Record<string, TerraformOutput> | null> {
    try {
      // Fetch workspace data from the appropriate endpoint
      const path = target.type === "pr"
        ? `/api/orgs/${encodeURIComponent(org)}/repos/${encodeURIComponent(repo)}/pr/${target.prNumber}`
        : `/api/orgs/${encodeURIComponent(org)}/repos/${encodeURIComponent(repo)}/env/${encodeURIComponent(target.name)}`

      const data = await this.request<ApiResponse<{
        workspaces: Array<{
          preview: { workspacePath: string }
          outputs: Record<string, TerraformOutput> | null
        }>
      }>>(path)

      // Find the matching workspace
      const ws = data.data?.workspaces?.find(
        (w) => w.preview.workspacePath === workspace
      )

      return ws?.outputs ?? null
    } catch (err) {
      if (err instanceof Error && err.message.includes("404")) {
        return null
      }
      throw err
    }
  }

  /**
   * Wait for a preview to reach a terminal state using SSE
   */
  async waitForPreview(
    previewId: string,
    timeoutSeconds: number = 300
  ): Promise<StreamUpdate> {
    const credentials = await this.auth.getCredentials()

    return new Promise((resolve, reject) => {
      const timeoutMs = timeoutSeconds * 1000
      const url = `${this.apiUrl}/api/previews/${previewId}/stream?token=${encodeURIComponent(credentials.accessToken)}`

      this.log.info(`Waiting for preview ${previewId}...`)

      const es = new EventSource(url)
      let resolved = false

      const timeout = setTimeout(() => {
        if (!resolved) {
          resolved = true
          es.close()
          reject(new Error(`Timeout waiting for preview after ${timeoutSeconds}s`))
        }
      }, timeoutMs)

      es.addEventListener("update", (event: MessageEvent) => {
        if (resolved) return

        try {
          const data = JSON.parse(event.data) as StreamUpdate

          if (!data.preview) {
            this.log.warn("Received update with no preview data")
            return
          }

          this.log.info(`Preview status: ${data.preview.status}`)

          // Check for terminal states
          if (data.preview.status === "ready") {
            resolved = true
            clearTimeout(timeout)
            es.close()
            resolve(data)
          } else if (data.preview.status === "failed") {
            resolved = true
            clearTimeout(timeout)
            es.close()
            reject(new Error("Preview failed"))
          } else if (data.preview.status === "destroyed") {
            resolved = true
            clearTimeout(timeout)
            es.close()
            reject(new Error("Preview was destroyed"))
          }
        } catch (err) {
          this.log.warn(`Failed to parse SSE event: ${err}`)
        }
      })

      es.onerror = (err: Event) => {
        if (resolved) return
        this.log.warn(`SSE connection error: ${err.type}`)

        setTimeout(() => {
          if (!resolved && es.readyState === 2) {
            resolved = true
            clearTimeout(timeout)
            reject(new Error("SSE connection closed unexpectedly"))
          }
        }, 5000)
      }

      es.onopen = () => {
        this.log.info("Connected to preview stream")
      }
    })
  }

  /**
   * Get outputs for a target, optionally waiting for it to be ready
   */
  async getOutputs(options: {
    org: string
    repo: string
    target: Target
    workspace: string
    wait?: boolean
    waitTimeout?: number
  }): Promise<{
    previewId: string
    status: string
    outputs: Record<string, TerraformOutput> | null
  }> {
    const { org, repo, target, workspace, wait = false, waitTimeout = 300 } = options

    const targetLabel =
      target.type === "pr" ? `PR #${target.prNumber}` : `env: ${target.name}`

    this.log.info(`Fetching outputs for ${org}/${repo} ${targetLabel} workspace=${workspace}`)

    const preview = await this.findPreview(org, repo, target, workspace)

    if (!preview) {
      throw new Error(
        `No preview found for ${org}/${repo} ${targetLabel} workspace=${workspace}`
      )
    }

    this.log.info(`Found preview ${preview.id} with status: ${preview.status}`)

    let outputs: Record<string, TerraformOutput> | null = null
    let status: PreviewStatus = preview.status

    if (wait && preview.status !== "ready") {
      this.log.info(`Waiting for preview to be ready (timeout: ${waitTimeout}s)...`)
      const result = await this.waitForPreview(preview.id, waitTimeout)
      status = result.preview?.status ?? "failed"
      outputs = result.outputs
    } else if (preview.status === "ready") {
      outputs = await this.getPreviewOutputs(org, repo, target, workspace)
    }

    return {
      previewId: preview.id,
      status,
      outputs,
    }
  }

  /**
   * List all previews for a repository
   */
  async listPreviews(org: string, repo: string): Promise<Preview[]> {
    const data = await this.request<ApiResponse<Preview[]>>(
      `/api/previews?org=${encodeURIComponent(org)}&repo=${encodeURIComponent(repo)}`
    )
    return data.data || []
  }

  async listOrgs(): Promise<OrgInfo[]> {
    const data = await this.request<ApiResponse<OrgInfo[]>>("/api/orgs")
    return data.data || []
  }

  async listEnvironments(options: {
    org: string
    repo?: string
    view?: "full" | "dag"
  }): Promise<EnvironmentGroup[]> {
    const searchParams = new URLSearchParams({
      org: options.org,
    })

    if (options.repo) {
      searchParams.set("repo", options.repo)
    }

    if (options.view) {
      searchParams.set("view", options.view)
    }

    const data = await this.request<ApiResponse<EnvironmentGroup[]>>(
      `/api/environments?${searchParams}`,
    )

    return data.data || []
  }

  async getPreviewOverview(options: {
    org: string
    repo?: string
    status?: string
    prNumber?: number
    limit?: number
    cursor?: string
  }): Promise<PreviewOverviewResponse> {
    const searchParams = new URLSearchParams({
      org: options.org,
    })

    if (options.repo) {
      searchParams.set("repo", options.repo)
    }

    if (options.status) {
      searchParams.set("status", options.status)
    }

    if (options.prNumber != null) {
      searchParams.set("pr_number", String(options.prNumber))
    }

    if (options.limit != null) {
      searchParams.set("limit", String(options.limit))
    }

    if (options.cursor) {
      searchParams.set("cursor", options.cursor)
    }

    return this.request<PreviewOverviewResponse>(`/api/previews/overview?${searchParams}`)
  }

  async getEnvironment(options: {
    org: string
    repo: string
    environmentName: string
    view?: "full" | "dag"
  }): Promise<EnvironmentPreviewGroup> {
    const searchParams = new URLSearchParams()
    if (options.view) {
      searchParams.set("view", options.view)
    }

    const suffix = searchParams.size > 0 ? `?${searchParams}` : ""
    const data = await this.request<ApiResponse<EnvironmentPreviewGroup>>(
      `/api/orgs/${encodeURIComponent(options.org)}/repos/${encodeURIComponent(options.repo)}/environment/${encodeURIComponent(options.environmentName)}${suffix}`,
    )

    return data.data
  }

  async getRun(runId: string): Promise<Run> {
    const data = await this.request<ApiResponse<Run>>(`/api/runs/${encodeURIComponent(runId)}`)
    return data.data
  }

  async getRunOutput(runId: string): Promise<string> {
    return this.requestText(`/api/runs/${encodeURIComponent(runId)}/output`)
  }

  async getRunSpans(runId: string): Promise<ResourceSpan[]> {
    const data = await this.request<ApiResponse<ResourceSpan[]>>(
      `/api/runs/${encodeURIComponent(runId)}/spans`,
    )
    return data.data || []
  }

  async rerunPreview(previewId: string): Promise<{ rerunQueued: boolean; runGroupId: string; jobId: string }> {
    const data = await this.request<ApiResponse<{ rerunQueued: boolean; runGroupId: string; jobId: string }>>(
      `/api/previews/${encodeURIComponent(previewId)}/rerun`,
      { method: "POST" },
    )

    return data.data
  }

  async triggerApply(previewId: string): Promise<{ applyStarted: boolean; jobId: string }> {
    const data = await this.request<ApiResponse<{ applyStarted: boolean; jobId: string }>>(
      `/api/previews/${encodeURIComponent(previewId)}/apply`,
      { method: "POST" },
    )

    return data.data
  }

  async cancelRun(runId: string): Promise<{ cancelled: boolean }> {
    const data = await this.request<ApiResponse<{ cancelled: boolean }>>(
      `/api/runs/${encodeURIComponent(runId)}/cancel`,
      { method: "POST" },
    )

    return data.data
  }
}
