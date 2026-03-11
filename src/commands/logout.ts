/**
 * yaffle logout - Remove stored credentials
 */

import { removeCredentials, getHost, loadConfig } from "@yaffle/client"

const DEFAULT_API_URL = "https://yaffle.local:6969"

export async function logout(args: string[]): Promise<void> {
  const config = await loadConfig()
  const apiUrl = config.apiUrl || DEFAULT_API_URL
  const host = getHost(apiUrl)

  await removeCredentials(host)
  console.log(`Logged out from ${host}`)
}
