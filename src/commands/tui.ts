import { createCliContext, parseTarget, resolveOrgRepo, getArg } from "../lib/cli-context.js"
import { launchTui, type TuiIntent } from "../tui/app.js"

const TUI_HELP = `
Usage: yaffle tui [options]

Open the operator TUI.

Options:
  --org <slug>       Focus an org when the TUI opens
  --repo <repo>      Focus a repo when opening a target environment
  --env <name>       Open a named environment directly
  --pr <number>      Open a PR environment directly
  --select <expr>    Focus matching workspaces inside the environment
  --api-url <url>    Override the Yaffle API URL
`

export async function tui(args: string[]): Promise<void> {
  if (args.includes("--help") || args.includes("-h")) {
    console.log(TUI_HELP)
    return
  }

  await launchIntentTui(args, "browse")
}

export async function launchIntentTui(args: string[], intent: TuiIntent): Promise<void> {
  if (!process.stdout.isTTY) {
    throw new Error("This command needs an interactive terminal")
  }

  const { apiUrl, client, defaultOrg } = await createCliContext(args)
  const selector = getArg(args, "--select") || null
  const target = parseTarget(args)
  const repoContext = await resolveOrgRepo(args)
  const initialOrg = repoContext.org || defaultOrg
  const initialRepo = repoContext.repo

  if (selector && !target) {
    throw new Error("--select currently requires --env or --pr")
  }

  if (target && (!initialOrg || !initialRepo)) {
    throw new Error("Could not determine --org and --repo. Pass them explicitly or run from a git repo.")
  }

  await launchTui({
    client,
    apiUrl,
    initialOrg,
    initialRepo,
    initialEnvironmentName: target?.environmentName,
    initialIntent: intent,
    initialSelector: selector,
    autoExecute: Boolean(selector && target && intent !== "browse"),
  })
}
