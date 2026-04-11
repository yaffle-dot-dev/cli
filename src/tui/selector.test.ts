import { describe, expect, test } from "bun:test"

import { parseSelector, resolveSelector } from "./selector.js"

const graph = {
  workspaces: [
    "infra/shared",
    "apps/web/infra",
    "apps/docs/infra",
    "apps/infra",
  ],
  edges: [
    ["apps/web/infra", "infra/shared"],
    ["apps/docs/infra", "infra/shared"],
    ["apps/infra", "apps/web/infra"],
    ["apps/infra", "apps/docs/infra"],
  ] as Array<[string, string]>,
}

describe("parseSelector", () => {
  test("parses a simple workspace path", () => {
    expect(parseSelector("apps/infra")).toEqual({
      raw: "apps/infra",
      workspacePath: "apps/infra",
      includeUpstream: false,
      includeDownstream: false,
    })
  })

  test("parses graph expansion operators", () => {
    expect(parseSelector("+apps/infra+")).toEqual({
      raw: "+apps/infra+",
      workspacePath: "apps/infra",
      includeUpstream: true,
      includeDownstream: true,
    })
  })

  test("rejects unsupported interior plus syntax", () => {
    expect(() => parseSelector("apps/+infra")).toThrow(/unsupported selector syntax/)
  })
})

describe("resolveSelector", () => {
  test("returns the exact workspace for a plain selector", () => {
    const selector = parseSelector("apps/infra")
    expect(resolveSelector(selector, graph, graph.workspaces)).toEqual(["apps/infra"])
  })

  test("expands upstream dependencies", () => {
    const selector = parseSelector("+apps/infra")
    expect(resolveSelector(selector, graph, graph.workspaces)).toEqual([
      "infra/shared",
      "apps/web/infra",
      "apps/docs/infra",
      "apps/infra",
    ])
  })

  test("expands downstream dependents", () => {
    const selector = parseSelector("infra/shared+")
    expect(resolveSelector(selector, graph, graph.workspaces)).toEqual([
      "infra/shared",
      "apps/web/infra",
      "apps/docs/infra",
      "apps/infra",
    ])
  })

  test("returns no matches when the root is absent", () => {
    const selector = parseSelector("apps/missing/infra")
    expect(resolveSelector(selector, graph, graph.workspaces)).toEqual([])
  })
})
