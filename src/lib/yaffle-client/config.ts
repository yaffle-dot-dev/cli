/**
 * CLI configuration and credential storage.
 */

import { readFile, writeFile, mkdir } from "node:fs/promises"
import { homedir } from "node:os"
import { join } from "node:path"

import type { Credentials } from "./types.js"

export interface Config {
  apiUrl?: string
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

export async function loadConfig(): Promise<Config> {
  try {
    const content = await readFile(CONFIG_FILE, "utf-8")
    return JSON.parse(content)
  } catch {
    return {}
  }
}

export async function saveConfig(config: Config): Promise<void> {
  await ensureConfigDir()
  await writeFile(CONFIG_FILE, JSON.stringify(config, null, 2), {
    mode: 0o600,
  })
}

export async function loadCredentials(): Promise<StoredCredentials> {
  try {
    const content = await readFile(CREDENTIALS_FILE, "utf-8")
    return JSON.parse(content)
  } catch {
    return {}
  }
}

export async function saveCredentials(
  host: string,
  credentials: Credentials,
): Promise<void> {
  await ensureConfigDir()
  const all = await loadCredentials()
  all[host] = credentials
  await writeFile(CREDENTIALS_FILE, JSON.stringify(all, null, 2), {
    mode: 0o600,
  })
}

export async function getCredentials(host: string): Promise<Credentials | null> {
  const all = await loadCredentials()
  return all[host] || null
}

export async function removeCredentials(host: string): Promise<void> {
  const all = await loadCredentials()
  delete all[host]
  await writeFile(CREDENTIALS_FILE, JSON.stringify(all, null, 2), {
    mode: 0o600,
  })
}

export function getHost(apiUrl: string): string {
  try {
    const url = new URL(apiUrl)
    return url.host
  } catch {
    return apiUrl
  }
}
