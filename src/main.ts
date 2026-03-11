#!/usr/bin/env bun
/**
 * Yaffle CLI
 *
 * Usage:
 *   yaffle login                    - Authenticate with Yaffle
 *   yaffle logout                   - Remove stored credentials
 *   yaffle outputs --pr 123         - Get outputs for a PR preview
 *   yaffle outputs --env main       - Get outputs for an environment
 *   yaffle deploy marketing --pr 123 - Deploy marketing site
 */

import { login } from "./commands/login.js"
import { logout } from "./commands/logout.js"
import { outputs } from "./commands/outputs.js"

const HELP = `
Yaffle CLI

Usage:
  yaffle <command> [options]

Commands:
  login              Authenticate with Yaffle (opens browser)
  logout             Remove stored credentials
  outputs            Get Terraform outputs from a preview
  whoami             Show current user

Options:
  --help, -h         Show help
  --version, -v      Show version

Examples:
  yaffle login
  yaffle outputs --pr 123 --workspace apps/infra
  yaffle outputs --env main --workspace apps/infra --wait
`

async function main(): Promise<void> {
  const args = process.argv.slice(2)
  const command = args[0]

  if (!command || command === "--help" || command === "-h") {
    console.log(HELP)
    process.exit(0)
  }

  if (command === "--version" || command === "-v") {
    console.log("yaffle 0.1.0")
    process.exit(0)
  }

  try {
    switch (command) {
      case "login":
        await login(args.slice(1))
        break
      case "logout":
        await logout(args.slice(1))
        break
      case "outputs":
        await outputs(args.slice(1))
        break
      case "whoami":
        console.log("TODO: whoami")
        break
      default:
        console.error(`Unknown command: ${command}`)
        console.log(HELP)
        process.exit(1)
    }
  } catch (err) {
    console.error(`Error: ${err instanceof Error ? err.message : err}`)
    process.exit(1)
  }
}

main()
