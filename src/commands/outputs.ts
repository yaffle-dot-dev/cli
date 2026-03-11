/**
 * yaffle outputs - Get Terraform outputs from a preview
 */

import {
  YaffleClient,
  TokenAuth,
  getCredentials,
  getHost,
  loadConfig,
  type Target,
  type TerraformOutput,
} from "@yaffle/client"
import { execSync } from "node:child_process"

const DEFAULT_API_URL = "https://yaffle.local:6969"

const HELP = `
Usage: yaffle outputs [options]

Get Terraform outputs from a Yaffle preview.

Options:
  --pr <number>       PR number
  --env <name>        Environment name (main, staging, etc.)
  --workspace <path>  Workspace path (default: ".")
  --wait              Wait for preview to be ready
  --timeout <secs>    Wait timeout in seconds (default: 300)
  --format <fmt>      Output format: json, env, github (default: json)
  --api-url <url>     Yaffle API URL
  --help              Show this help

Examples:
  yaffle outputs --pr 123 --workspace apps/infra
  yaffle outputs --env main --workspace apps/infra --wait
`

interface Args {
  apiUrl: string
  target: Target
  workspace: string
  wait: boolean
  waitTimeout: number
  format: "json" | "env" | "github"
}

export async function outputs(args: string[]): Promise<void> {
  if (args.includes("--help") || args.includes("-h")) {
    console.log(HELP)
    return
  }

  const parsed = await parseArgs(args)
  const client = await createClient(parsed.apiUrl)

  const { org, repo } = getOrgRepo()

  const result = await client.getOutputs({
    org,
    repo,
    target: parsed.target,
    workspace: parsed.workspace,
    wait: parsed.wait,
    waitTimeout: parsed.waitTimeout,
  })

  if (!result.outputs) {
    console.error("[warn] No outputs found")
  }

  const output = formatOutput(
    result.outputs || {},
    parsed.format,
    result.previewId,
    result.status
  )
  console.log(output)
}

async function parseArgs(args: string[]): Promise<Args> {
  const config = await loadConfig()

  const apiUrl = getArg(args, "--api-url") || config.apiUrl || DEFAULT_API_URL

  // Parse target
  const prStr = getArg(args, "--pr")
  const envName = getArg(args, "--env")

  let target: Target
  if (prStr) {
    const prNumber = parseInt(prStr, 10)
    if (isNaN(prNumber)) {
      throw new Error("--pr must be a number")
    }
    target = { type: "pr", prNumber }
  } else if (envName) {
    target = { type: "env", name: envName }
  } else {
    throw new Error("Must specify --pr <number> or --env <name>")
  }

  const workspace = getArg(args, "--workspace") || "."
  const wait = args.includes("--wait")
  const waitTimeout = parseInt(getArg(args, "--timeout") || "300", 10)
  const format = (getArg(args, "--format") || "json") as Args["format"]

  if (!["json", "env", "github"].includes(format)) {
    throw new Error("--format must be json, env, or github")
  }

  return { apiUrl, target, workspace, wait, waitTimeout, format }
}

async function createClient(apiUrl: string): Promise<YaffleClient> {
  // Try stored credentials first
  const host = getHost(apiUrl)
  const stored = await getCredentials(host)

  let token: string

  if (stored) {
    token = stored.accessToken
  } else {
    // Fall back to environment variables
    token = process.env.YAFFLE_TOKEN || process.env.GITHUB_TOKEN || ""

    // Try gh auth token
    if (!token) {
      try {
        token = execSync("gh auth token", {
          encoding: "utf8",
          stdio: ["pipe", "pipe", "ignore"],
        }).trim()
      } catch {
        // Ignore
      }
    }
  }

  if (!token) {
    throw new Error(
      "Not authenticated. Run 'yaffle login' or set YAFFLE_TOKEN/GITHUB_TOKEN"
    )
  }

  return new YaffleClient({
    apiUrl,
    auth: new TokenAuth(token),
    logger: {
      info: (msg) => console.error(`[info] ${msg}`),
      warn: (msg) => console.error(`[warn] ${msg}`),
      error: (msg) => console.error(`[error] ${msg}`),
    },
  })
}

function getOrgRepo(): { org: string; repo: string } {
  // Check env vars first
  if (process.env.GITHUB_REPOSITORY) {
    const [org, repo] = process.env.GITHUB_REPOSITORY.split("/")
    return { org, repo }
  }

  // Try git remote
  try {
    const remote = execSync("git remote get-url origin", {
      encoding: "utf8",
      stdio: ["pipe", "pipe", "ignore"],
    }).trim()

    const match = remote.match(/github\.com[:/]([^/]+)\/([^/.]+)/)
    if (match) {
      return { org: match[1], repo: match[2] }
    }
  } catch {
    // Ignore
  }

  throw new Error("Could not determine org/repo. Set GITHUB_REPOSITORY or run from a git repo.")
}

function getArg(args: string[], flag: string): string {
  const idx = args.indexOf(flag)
  if (idx !== -1 && idx + 1 < args.length) {
    return args[idx + 1]
  }
  return ""
}

function formatOutput(
  outputs: Record<string, TerraformOutput>,
  format: "json" | "env" | "github",
  previewId: string,
  status: string
): string {
  switch (format) {
    case "json":
      return JSON.stringify({ previewId, status, outputs }, null, 2)

    case "env": {
      const lines: string[] = [
        `export YAFFLE_PREVIEW_ID="${previewId}"`,
        `export YAFFLE_PREVIEW_STATUS="${status}"`,
      ]
      for (const [name, output] of Object.entries(outputs)) {
        const value =
          typeof output.value === "object"
            ? JSON.stringify(output.value)
            : String(output.value)
        const escaped = value.replace(/"/g, '\\"')
        lines.push(`export ${toEnvName(name)}="${escaped}"`)
      }
      return lines.join("\n")
    }

    case "github": {
      const lines: string[] = [
        `preview-id=${previewId}`,
        `preview-status=${status}`,
        `outputs-json=${JSON.stringify(outputs)}`,
      ]
      for (const [name, output] of Object.entries(outputs)) {
        const value =
          typeof output.value === "object"
            ? JSON.stringify(output.value)
            : String(output.value)
        lines.push(`${name}=${value}`)
      }
      return lines.join("\n")
    }
  }
}

function toEnvName(name: string): string {
  return name
    .replace(/([a-z])([A-Z])/g, "$1_$2")
    .replace(/-/g, "_")
    .toUpperCase()
}
