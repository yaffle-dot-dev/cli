#!/usr/bin/env bun

import { apply } from "./commands/apply.js"
import { login } from "./commands/login.js"
import { logout } from "./commands/logout.js"
import { outputs } from "./commands/outputs.js"
import { plan } from "./commands/plan.js"
import { tui } from "./commands/tui.js"

const HELP = `
Yaffle CLI

Usage:
  yaffle <command> [options]

Commands:
  tui                Open the operator TUI
  plan               Open plan mode in the TUI
  apply              Open apply mode in the TUI
  login              Authenticate with Yaffle (API key flow)
  logout             Remove stored credentials
  outputs            Get Terraform outputs from a preview
  whoami             Show current user

Options:
  --help, -h         Show help
  --version, -v      Show version

Examples:
  yaffle
  yaffle tui
  yaffle plan --env main
  yaffle plan --env main --select +apps/infra
  yaffle apply --pr 123
  yaffle outputs --pr 123 --workspace apps/infra
`

async function main(): Promise<number> {
  const args = process.argv.slice(2)
  const command = args[0]

  if (!command) {
    if (process.stdout.isTTY) {
      await tui([])
      return 0
    }

    console.log(HELP)
    return 0
  }

  if (command === "--help" || command === "-h") {
    console.log(HELP)
    return 0
  }

  if (command === "--version" || command === "-v") {
    console.log("yaffle 0.1.0")
    return 0
  }

  try {
    switch (command) {
      case "tui":
        await tui(args.slice(1))
        return 0
      case "plan":
        await plan(args.slice(1))
        return 0
      case "apply":
        await apply(args.slice(1))
        return 0
      case "login":
        await login(args.slice(1))
        return 0
      case "logout":
        await logout(args.slice(1))
        return 0
      case "outputs":
        await outputs(args.slice(1))
        return 0
      case "whoami":
        console.log("TODO: whoami")
        return 0
      default:
        console.error(`Unknown command: ${command}`)
        console.log(HELP)
        return 1
    }
  } catch (err) {
    console.error(`Error: ${err instanceof Error ? err.message : err}`)
    return 1
  }
}

void main().then((code) => {
  process.exitCode = code
})
