import { launchIntentTui } from "./tui.js"

const PLAN_HELP = `
Usage: yaffle plan [options]

Open plan mode in the operator TUI.

Examples:
  yaffle plan
  yaffle plan --env main
  yaffle plan --pr 123
  yaffle plan --env main --select +apps/infra
`

export async function plan(args: string[]): Promise<void> {
  if (args.includes("--help") || args.includes("-h")) {
    console.log(PLAN_HELP)
    return
  }

  await launchIntentTui(args, "plan")
}
