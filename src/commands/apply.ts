import { launchIntentTui } from "./tui.js"

const APPLY_HELP = `
Usage: yaffle apply [options]

Open apply mode in the operator TUI.

Examples:
  yaffle apply
  yaffle apply --env main
  yaffle apply --pr 123
  yaffle apply --env main --select apps/web/infra
`

export async function apply(args: string[]): Promise<void> {
  if (args.includes("--help") || args.includes("-h")) {
    console.log(APPLY_HELP)
    return
  }

  await launchIntentTui(args, "apply")
}
