import type { DependencyGraph } from "../lib/yaffle-client/index.js"

export interface ParsedSelector {
  raw: string
  workspacePath: string
  includeUpstream: boolean
  includeDownstream: boolean
}

export function parseSelector(input: string): ParsedSelector {
  const raw = input.trim()
  if (!raw) {
    throw new Error("selector cannot be empty")
  }

  const includeUpstream = raw.startsWith("+")
  const includeDownstream = raw.endsWith("+")
  const workspacePath = raw.replace(/^\+/, "").replace(/\+$/, "")

  if (!workspacePath) {
    throw new Error(`invalid selector: ${input}`)
  }

  if (workspacePath.includes("+")) {
    throw new Error(`unsupported selector syntax: ${input}`)
  }

  return {
    raw,
    workspacePath,
    includeUpstream,
    includeDownstream,
  }
}

export function resolveSelector(
  selector: ParsedSelector,
  graph: DependencyGraph | null,
  availableWorkspaces: string[],
): string[] {
  const available = new Set(availableWorkspaces)
  if (!available.has(selector.workspacePath)) {
    return []
  }

  if (!graph) {
    return [selector.workspacePath]
  }

  const upstream = new Map<string, string[]>()
  const downstream = new Map<string, string[]>()

  for (const workspace of graph.workspaces) {
    upstream.set(workspace, [])
    downstream.set(workspace, [])
  }

  for (const [source, target] of graph.edges) {
    upstream.set(source, [...(upstream.get(source) ?? []), target])
    downstream.set(target, [...(downstream.get(target) ?? []), source])
  }

  const selected = new Set<string>([selector.workspacePath])

  if (selector.includeUpstream) {
    walkGraph(selector.workspacePath, upstream, selected)
  }

  if (selector.includeDownstream) {
    walkGraph(selector.workspacePath, downstream, selected)
  }

  return graph.workspaces.filter((workspace) => selected.has(workspace) && available.has(workspace))
}

function walkGraph(
  start: string,
  adjacency: Map<string, string[]>,
  selected: Set<string>,
): void {
  const queue = [...(adjacency.get(start) ?? [])]

  while (queue.length > 0) {
    const current = queue.shift()
    if (!current || selected.has(current)) {
      continue
    }

    selected.add(current)
    queue.push(...(adjacency.get(current) ?? []))
  }
}
