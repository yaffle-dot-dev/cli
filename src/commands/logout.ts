/**
 * yaffle logout - Remove stored credentials
 */

import { removeCredentials, getHost } from "@yaffle/client"
import { DEFAULT_API_URL, resolveApiUrl } from "../lib/api-url.js"

export async function logout(args: string[]): Promise<void> {
  const apiUrl = resolveApiUrl({
    flagValue: getArg(args, "--api-url"),
    envValue: process.env.YAFFLE_API_URL,
    defaultValue: DEFAULT_API_URL,
  })
  const host = getHost(apiUrl)

  await removeCredentials(host)
  console.log(`Logged out from ${host}`)
}

function getArg(args: string[], flag: string): string {
  const idx = args.indexOf(flag)
  if (idx !== -1 && idx + 1 < args.length) {
    return args[idx + 1]
  }
  return ""
}
