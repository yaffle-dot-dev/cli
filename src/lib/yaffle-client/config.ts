/**
 * CLI configuration and credential storage.
 *
 * Stores credentials in ~/.yaffle/credentials.json (like Terraform).
 */

import { readFile, writeFile, mkdir } from "node:fs/promises"
import { homedir } from "node:os"
import { join } from "node:path"
import type { Credentials } from "./types.js"

export interface Config {
  /** Default API URL */
  apiUrl?: string
  /** Default org (inferred from git if not set) */
  defaultOrg?: string
}

export interface StoredCredentials {
  [host: string]: Credentials
}

const CONFIG_DIR = join(homedir(), ".yaffle")
const CONFIG_FILE = join(CONFIG_DIR, "config.json")
const CREDENTIALS_FILE = join(CONFIG_DIR, "credentials.json")

async function ensureConfigDir(): Promise<void> {
  await mkdir(CONFIG_DIR, { recursive: true, mode: 0o700 })
}

/**
 * Load config from ~/.yaffle/config.json
 */
export async function loadConfig(): Promise<Config> {
  try {
    const content = await readFile(CONFIG_FILE, "utf-8")
    return JSON.parse(content)
  } catch {
    return {}
  }
}

/**
 * Save config to ~/.yaffle/config.json
 */
export async function saveConfig(config: Config): Promise<void> {
  await ensureConfigDir()
  await writeFile(CONFIG_FILE, JSON.stringify(config, null, 2), {
    mode: 0o600,
  })
}

/**
 * Load credentials from ~/.yaffle/credentials.json
 */
export async function loadCredentials(): Promise<StoredCredentials> {
  try {
    const content = await readFile(CREDENTIALS_FILE, "utf-8")
    return JSON.parse(content)
  } catch {
    return {}
  }
}

/**
 * Save credentials for a host
 */
export async function saveCredentials(
  host: string,
  credentials: Credentials
): Promise<void> {
  await ensureConfigDir()
  const all = await loadCredentials()
  all[host] = credentials
  await writeFile(CREDENTIALS_FILE, JSON.stringify(all, null, 2), {
    mode: 0o600, // Only owner can read/write
  })
}

/**
 * Get credentials for a host
 */
export async function getCredentials(host: string): Promise<Credentials | null> {
  const all = await loadCredentials()
  return all[host] || null
}

/**
 * Remove credentials for a host
 */
export async function removeCredentials(host: string): Promise<void> {
  const all = await loadCredentials()
  delete all[host]
  await writeFile(CREDENTIALS_FILE, JSON.stringify(all, null, 2), {
    mode: 0o600,
  })
}

/**
 * Get the host part of a URL for credential lookup
 */
export function getHost(apiUrl: string): string {
  try {
    const url = new URL(apiUrl)
    return url.host
  } catch {
    return apiUrl
  }
}
