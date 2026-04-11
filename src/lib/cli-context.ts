import { execSync } from "node:child_process"

import {
  getCredentials,
  getHost,
  loadConfig,
  TokenAuth,
  YaffleClient,
} from "@yaffle/client"

import { DEFAULT_API_URL, resolveApiUrl } from "./api-url.js"

export interface CliTarget {
  environmentName: string
  mode: "env" | "pr"
}

export interface CliContext {
  apiUrl: string
  client: YaffleClient
  defaultOrg?: string
}

export function getArg(args: string[], flag: string): string {
  const idx = args.indexOf(flag)
  if (idx !== -1 && idx + 1 < args.length) {
    return args[idx + 1]
  }

  return ""
}

export function hasFlag(args: string[], ...flags: string[]): boolean {
  return flags.some((flag) => args.includes(flag))
}

export function inferGitHubRepo(): { org: string; repo: string } | null {
  if (process.env.GITHUB_REPOSITORY) {
    const [org, repo] = process.env.GITHUB_REPOSITORY.split("/")
    if (org && repo) {
      return { org, repo }
    }
  }

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
    return null
  }

  return null
}

export async function createCliContext(args: string[]): Promise<CliContext> {
  const config = await loadConfig()
  const apiUrl = resolveApiUrl({
    flagValue: getArg(args, "--api-url"),
    envValue: process.env.YAFFLE_API_URL,
    defaultValue: DEFAULT_API_URL,
  })

  const token = await resolveAccessToken(apiUrl)

  const client = new YaffleClient({
    apiUrl,
    auth: new TokenAuth(token),
    logger: {
      info: () => {},
      warn: () => {},
      error: () => {},
    },
  })

  return {
    apiUrl,
    client,
    defaultOrg: config.defaultOrg,
  }
}

export async function resolveOrgRepo(args: string[]): Promise<{ org?: string; repo?: string }> {
  const explicitOrg = getArg(args, "--org") || undefined
  const explicitRepo = getArg(args, "--repo") || undefined

  if (explicitOrg || explicitRepo) {
    return {
      org: explicitOrg,
      repo: explicitRepo,
    }
  }

  return inferGitHubRepo() ?? {}
}

export function parseTarget(args: string[]): CliTarget | null {
  const environmentName = getArg(args, "--env")
  const pr = getArg(args, "--pr")

  if (environmentName) {
    return {
      environmentName,
      mode: "env",
    }
  }

  if (pr) {
    const prNumber = Number.parseInt(pr, 10)
    if (Number.isNaN(prNumber) || prNumber <= 0) {
      throw new Error("--pr must be a positive number")
    }

    return {
      environmentName: `pr-${prNumber}`,
      mode: "pr",
    }
  }

  return null
}

async function resolveAccessToken(apiUrl: string): Promise<string> {
  let token = process.env.YAFFLE_TOKEN
    || process.env.YAFFLE_API_TOKEN
    || process.env.GITHUB_TOKEN
    || ""

  if (!token) {
    const host = getHost(apiUrl)
    const stored = await getCredentials(host)
    token = stored?.accessToken || ""
  }

  if (!token) {
    try {
      token = execSync("gh auth token", {
        encoding: "utf8",
        stdio: ["pipe", "pipe", "ignore"],
      }).trim()
    } catch {
      token = ""
    }
  }

  if (!token) {
    throw new Error("Not authenticated. Run 'yaffle login' or set YAFFLE_TOKEN")
  }

  return token
}
