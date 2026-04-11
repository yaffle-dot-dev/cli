/**
 * yaffle login - Authenticate with Yaffle using an API key
 *
 * 1. User creates an API key in the Yaffle web UI (Settings > API Keys)
 * 2. User runs `yaffle login` and pastes the key
 * 3. CLI stores the key in ~/.yaffle/credentials.json
 */

import {
  YaffleClient,
  TokenAuth,
  saveCredentials,
  getHost,
} from "@yaffle/client"
import { DEFAULT_API_URL, resolveApiUrl } from "../lib/api-url.js"

function getSettingsUrl(apiUrl: string): string {
  return `${apiUrl.replace(/\/+$/, "")}/app/_/settings`
}

export async function login(args: string[]): Promise<void> {
  // Parse --api-url flag (overrides env var)
  const apiUrl = resolveApiUrl({
    flagValue: getArg(args, "--api-url"),
    envValue: process.env.YAFFLE_API_URL,
    defaultValue: DEFAULT_API_URL,
  })

  const settingsUrl = getSettingsUrl(apiUrl)

  console.log("Yaffle CLI Login")
  console.log()
  console.log("To get an API key:")
  console.log(`  1. Go to ${settingsUrl}`)
  console.log("  2. Click 'Create API Key'")
  console.log("  3. Copy and paste it below")
  console.log()

  // Read API key from stdin (hidden input)
  const apiKey = await promptSecret("API Key: ")

  if (!apiKey) {
    throw new Error("No API key provided")
  }

  if (!apiKey.startsWith("yfl_")) {
    throw new Error("Invalid API key format. API keys start with 'yfl_'")
  }

  console.log()
  console.log("Verifying API key...")

  // Verify the API key works by making a test request
  const client = new YaffleClient({
    apiUrl,
    auth: new TokenAuth(apiKey),
    logger: {
      info: () => {},
      warn: () => {},
      error: () => {},
    },
  })

  try {
    // Try to list previews as a test - this will fail if the key is invalid
    // We don't actually need the results, just need to verify auth works
    await testAuth(apiUrl, apiKey)
  } catch (err) {
    throw new Error(`API key verification failed: ${err instanceof Error ? err.message : err}`)
  }

  // Save credentials
  const host = getHost(apiUrl)
  await saveCredentials(host, { accessToken: apiKey })

  console.log()
  console.log("Successfully authenticated!")
  console.log(`Credentials saved to ~/.yaffle/credentials.json`)
}

function getArg(args: string[], flag: string): string {
  const idx = args.indexOf(flag)
  if (idx !== -1 && idx + 1 < args.length) {
    return args[idx + 1]
  }
  return ""
}

async function promptSecret(question: string): Promise<string> {
  const stdin = process.stdin
  
  // If not a TTY (piped input), just read normally
  if (!stdin.isTTY) {
    process.stdout.write(question)
    return new Promise((resolve) => {
      let data = ""
      stdin.setEncoding("utf8")
      stdin.on("data", (chunk) => { data += chunk })
      stdin.on("end", () => resolve(data.trim().split("\n")[0]))
    })
  }
  
  // TTY: use raw mode to hide input
  return new Promise((resolve) => {
    process.stdout.write(question)
    
    const wasRaw = stdin.isRaw
    stdin.setRawMode(true)
    stdin.resume()
    stdin.setEncoding("utf8")
    
    let input = ""
    
    const onData = (char: string) => {
      // Handle Ctrl+C
      if (char === "\u0003") {
        stdin.setRawMode(wasRaw ?? false)
        stdin.removeListener("data", onData)
        process.stdout.write("\n")
        process.exit(1)
      }
      
      // Handle Enter
      if (char === "\r" || char === "\n") {
        stdin.setRawMode(wasRaw ?? false)
        stdin.removeListener("data", onData)
        stdin.pause()
        process.stdout.write("\n")
        resolve(input.trim())
        return
      }
      
      // Handle Backspace
      if (char === "\u007F" || char === "\b") {
        if (input.length > 0) {
          input = input.slice(0, -1)
        }
        return
      }
      
      // Add character to input (don't echo)
      input += char
    }
    
    stdin.on("data", onData)
  })
}

async function testAuth(apiUrl: string, apiKey: string): Promise<void> {
  // Make a simple authenticated request to verify the key
  const response = await fetch(`${apiUrl}/api/auth/get-session`, {
    headers: {
      Authorization: `Bearer ${apiKey}`,
      Accept: "application/json",
    },
  })

  // 200 or 401 tells us the API is reachable and the auth was processed
  // A 401 with our API key means the key is invalid
  // Any other error means network/server issue
  if (response.status === 401) {
    const data = await response.json().catch(() => ({}))
    throw new Error(data.error?.message || "Invalid API key")
  }

  if (!response.ok && response.status !== 404) {
    throw new Error(`Server error: ${response.status}`)
  }
}
