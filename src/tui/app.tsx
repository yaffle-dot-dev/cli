import { createCliRenderer } from "@opentui/core"
import { createRoot, useKeyboard, useRenderer, useTerminalDimensions } from "@opentui/react"
import type {
  DependencyGraph,
  EnvironmentGroup,
  EnvironmentPreviewGroup,
  OrgInfo,
  Preview,
  PreviewOverviewResponse,
  Run,
  WorkspaceWithRuns,
  YaffleClient,
} from "../lib/yaffle-client/index.js"
import { useEffect, useMemo, useState, type ReactNode } from "react"

import { parseSelector, resolveSelector } from "./selector.js"

export type TuiIntent = "browse" | "plan" | "apply"

export interface TuiLaunchOptions {
  client: YaffleClient
  apiUrl: string
  initialOrg?: string
  initialRepo?: string
  initialEnvironmentName?: string
  initialIntent?: TuiIntent
  initialSelector?: string | null
  autoExecute?: boolean
}

interface PendingActionConfirmation {
  action: "plan" | "apply"
  workspaces: WorkspaceWithRuns[]
}

interface PendingActionMenu {
  workspaces: WorkspaceWithRuns[]
}

interface ShortcutSection {
  title: string
  items: Array<{ keys: string; description: string }>
}

type HomePane = "orgs" | "named" | "transient"
type EnvironmentPane = "graph" | "details"

interface StyledChunk {
  text: string
  fg?: string
}

interface OptimisticStatusOverride {
  status: string
  previousStatus: string
}

interface BrowserEnvironmentItem {
  key: string
  kind: "named" | "transient"
  repo: string
  environmentName: string
  label: string
  ref: string
  headSha: string
  status: string
  workspaceCount: number
  updatedAt: string
  prNumber: number | null
  authorLogin: string | null
}

interface GraphNodeLayout {
  workspace: WorkspaceWithRuns
  x: number
  y: number
  width: number
  height: number
  title: string
  subtitle: string
  selected: boolean
  marked: boolean
}

interface GraphEdgeSegment {
  key: string
  x: number
  y: number
  text: string
  tone: "selected" | "marked" | "normal"
}

interface GraphCanvasLayout {
  width: number
  height: number
  zoomLabel: string
  nodes: GraphNodeLayout[]
  edges: GraphEdgeSegment[]
  columns: string[][]
  positions: Map<string, { column: number; row: number }>
}

type PaneOption<T> = {
  name: string
  description: string
  value: T
}

type Dimension = number | "auto" | `${number}%`

type Screen =
  | {
      kind: "dashboard"
      intent: TuiIntent
    }
  | {
      kind: "environment"
      intent: TuiIntent
      org: string
      repo: string
      environmentName: string
      selector: string | null
      autoExecute: boolean
    }

const THEME = {
  background: "#0c0c0b",
  surface: "#18181a",
  surfaceRaised: "#242420",
  surfaceMuted: "#11110f",
  border: "#3d3d32",
  borderStrong: "#8bc431",
  text: "#fafaf8",
  textDim: "#787870",
  textMuted: "#b5b5a8",
  accent: "#8bc431",
  success: "#6da323",
  warning: "#fce047",
  danger: "#fa2d2d",
  info: "#c5eb82",
  selection: "#1d2e0a",
  statusPending: "#b5b5a8",
  statusPlanning: "#fce047",
  statusApplying: "#8bc431",
  statusReady: "#6da323",
  statusFailed: "#fa2d2d",
  statusDestroyed: "#787870",
}

const DETAIL_TABS = ["overview", "logs", "outputs"] as const

type DetailTab = (typeof DETAIL_TABS)[number]

export async function launchTui(options: TuiLaunchOptions): Promise<void> {
  let resolveDone: (() => void) | null = null
  const done = new Promise<void>((resolve) => {
    resolveDone = resolve
  })

  const renderer = await createCliRenderer({
    exitOnCtrlC: true,
    onDestroy: () => resolveDone?.(),
  })

  createRoot(renderer).render(<TuiApp {...options} />)
  await done
}

function TuiApp({
  client,
  apiUrl,
  initialOrg,
  initialRepo,
  initialEnvironmentName,
  initialIntent = "browse",
  initialSelector = null,
  autoExecute = false,
}: TuiLaunchOptions) {
  const renderer = useRenderer()
  const { width, height } = useTerminalDimensions()

  const [screen, setScreen] = useState<Screen>(() => {
    if (initialOrg && initialRepo && initialEnvironmentName) {
      return {
        kind: "environment",
        intent: initialIntent,
        org: initialOrg,
        repo: initialRepo,
        environmentName: initialEnvironmentName,
        selector: initialSelector,
        autoExecute,
      }
    }

    return {
      kind: "dashboard",
      intent: initialIntent,
    }
  })
  const [toast, setToast] = useState<string | null>(null)

  const [orgs, setOrgs] = useState<OrgInfo[]>([])
  const [orgError, setOrgError] = useState<string | null>(null)
  const [selectedOrgSlug, setSelectedOrgSlug] = useState<string | null>(initialOrg ?? null)

  const [namedItems, setNamedItems] = useState<BrowserEnvironmentItem[]>([])
  const [transientItems, setTransientItems] = useState<BrowserEnvironmentItem[]>([])
  const [browserError, setBrowserError] = useState<string | null>(null)
  const [browserLoading, setBrowserLoading] = useState(true)

  const [activeHomePane, setActiveHomePane] = useState<HomePane>("named")
  const [fullscreenHomePane, setFullscreenHomePane] = useState<HomePane | null>(null)
  const [selectedOrgIndex, setSelectedOrgIndex] = useState(0)
  const [selectedNamedIndex, setSelectedNamedIndex] = useState(0)
  const [selectedTransientIndex, setSelectedTransientIndex] = useState(0)

  const [environmentData, setEnvironmentData] = useState<EnvironmentPreviewGroup | null>(null)
  const [environmentLoading, setEnvironmentLoading] = useState(false)
  const [environmentError, setEnvironmentError] = useState<string | null>(null)
  const [activeEnvironmentPane, setActiveEnvironmentPane] = useState<EnvironmentPane>("graph")
  const [fullscreenEnvironmentPane, setFullscreenEnvironmentPane] = useState<EnvironmentPane | null>(null)
  const [selectedWorkspaceIndex, setSelectedWorkspaceIndex] = useState(0)
  const [markedWorkspacePaths, setMarkedWorkspacePaths] = useState<string[]>([])
  const [selectedTab, setSelectedTab] = useState<DetailTab>("overview")
  const [graphZoom, setGraphZoom] = useState(1)
  const [detailScrollOffset, setDetailScrollOffset] = useState(0)
  const [selectedRunOutput, setSelectedRunOutput] = useState("")
  const [busyAction, setBusyAction] = useState<string | null>(null)
  const [showHelp, setShowHelp] = useState(false)
  const [pendingActionMenu, setPendingActionMenu] = useState<PendingActionMenu | null>(null)
  const [pendingConfirmation, setPendingConfirmation] = useState<PendingActionConfirmation | null>(null)
  const [optimisticStatusOverrides, setOptimisticStatusOverrides] = useState<Record<string, OptimisticStatusOverride>>({})
  const [autoExecuted, setAutoExecuted] = useState(false)

  useEffect(() => {
    if (!toast) {
      return
    }

    const timeout = setTimeout(() => setToast(null), 4_000)
    return () => clearTimeout(timeout)
  }, [toast])

  useEffect(() => {
    let cancelled = false

    const loadOrgs = async (): Promise<void> => {
      try {
        const data = await client.listOrgs()
        if (cancelled) {
          return
        }

        setOrgs(data)
        setOrgError(null)

        if (!selectedOrgSlug) {
          const nextSlug = data[0]?.slug ?? null
          setSelectedOrgSlug(nextSlug)
        }
      } catch (error) {
        if (!cancelled) {
          setOrgError(error instanceof Error ? error.message : String(error))
        }
      }
    }

    void loadOrgs()
    const interval = setInterval(() => void loadOrgs(), 15_000)
    return () => {
      cancelled = true
      clearInterval(interval)
    }
  }, [client, selectedOrgSlug])

  useEffect(() => {
    const nextIndex = orgs.findIndex((org) => org.slug === selectedOrgSlug)
    if (nextIndex >= 0) {
      setSelectedOrgIndex(nextIndex)
    }
  }, [orgs, selectedOrgSlug])

  useEffect(() => {
    if (!selectedOrgSlug) {
      return
    }

    let cancelled = false

    const loadBrowser = async (): Promise<void> => {
      try {
        setBrowserLoading(true)
        const [named, overview] = await Promise.all([
          client.listEnvironments({ org: selectedOrgSlug, view: "dag" }),
          client.getPreviewOverview({ org: selectedOrgSlug, limit: 250 }),
        ])

        if (cancelled) {
          return
        }

        setNamedItems(buildNamedItems(named))
        setTransientItems(buildTransientItems(overview))
        setBrowserError(null)
      } catch (error) {
        if (!cancelled) {
          setBrowserError(error instanceof Error ? error.message : String(error))
        }
      } finally {
        if (!cancelled) {
          setBrowserLoading(false)
        }
      }
    }

    void loadBrowser()
    const interval = setInterval(() => void loadBrowser(), 10_000)

    return () => {
      cancelled = true
      clearInterval(interval)
    }
  }, [client, selectedOrgSlug])

  useEffect(() => {
    if (screen.kind !== "environment") {
      setEnvironmentData(null)
      setEnvironmentError(null)
      setSelectedRunOutput("")
      setAutoExecuted(false)
      setShowHelp(false)
      setFullscreenEnvironmentPane(null)
      setPendingActionMenu(null)
      setPendingConfirmation(null)
      setOptimisticStatusOverrides({})
      return
    }

    setAutoExecuted(false)
    setSelectedWorkspaceIndex(0)
    setMarkedWorkspacePaths([])
    setActiveEnvironmentPane("graph")
    setSelectedTab("overview")
    setDetailScrollOffset(0)
    setGraphZoom(1)
    setShowHelp(false)
    setFullscreenEnvironmentPane(null)
    setPendingActionMenu(null)
    setPendingConfirmation(null)
    setOptimisticStatusOverrides({})

    let cancelled = false

    const loadEnvironment = async (): Promise<void> => {
      try {
        setEnvironmentLoading(true)
        const next = await client.getEnvironment({
          org: screen.org,
          repo: screen.repo,
          environmentName: screen.environmentName,
          view: "full",
        })

        if (cancelled) {
          return
        }

        setEnvironmentData(next)
        setEnvironmentError(null)
      } catch (error) {
        if (!cancelled) {
          setEnvironmentError(error instanceof Error ? error.message : String(error))
        }
      } finally {
        if (!cancelled) {
          setEnvironmentLoading(false)
        }
      }
    }

    void loadEnvironment()
    const interval = setInterval(() => void loadEnvironment(), 2_000)

    return () => {
      cancelled = true
      clearInterval(interval)
    }
  }, [client, screen])

  const latestRunGroup = useMemo(() => environmentData?.runGroups[0] ?? null, [environmentData])
  const dependencyGraph = useMemo(
    () => latestRunGroup?.dependencyGraph ?? environmentData?.runGroups.find((runGroup) => runGroup.dependencyGraph)?.dependencyGraph ?? null,
    [environmentData, latestRunGroup],
  )

  const workspaceList = useMemo(() => {
    if (!environmentData) {
      return []
    }

    const byPath = new Map(environmentData.workspaces.map((workspace) => [workspace.preview.workspacePath, workspace]))
    const ordered = dependencyGraph?.workspaces
      ? dependencyGraph.workspaces
          .map((workspacePath) => byPath.get(workspacePath))
          .filter((workspace): workspace is WorkspaceWithRuns => Boolean(workspace))
      : environmentData.workspaces

    return ordered
  }, [dependencyGraph, environmentData])

  const selectorMatchPaths = useMemo(() => {
    if (screen.kind !== "environment" || !screen.selector) {
      return null
    }

    try {
      const selector = parseSelector(screen.selector)
      return resolveSelector(
        selector,
        dependencyGraph,
        workspaceList.map((workspace) => workspace.preview.workspacePath),
      )
    } catch {
      return []
    }
  }, [dependencyGraph, screen, workspaceList])

  const visibleWorkspaces = useMemo(() => {
    if (!selectorMatchPaths || selectorMatchPaths.length === 0) {
      return selectorMatchPaths ? [] : workspaceList
    }

    const allowed = new Set(selectorMatchPaths)
    return workspaceList.filter((workspace) => allowed.has(workspace.preview.workspacePath))
  }, [selectorMatchPaths, workspaceList])

  const effectiveVisibleWorkspaces = useMemo(
    () => visibleWorkspaces.map((workspace) => applyOptimisticWorkspaceStatus(workspace, optimisticStatusOverrides)),
    [optimisticStatusOverrides, visibleWorkspaces],
  )

  useEffect(() => {
    const visibleSet = new Set(effectiveVisibleWorkspaces.map((workspace) => workspace.preview.workspacePath))
    setMarkedWorkspacePaths((current) => {
      const next = current.filter((workspacePath) => visibleSet.has(workspacePath))
      return next.length === current.length && next.every((workspacePath, index) => workspacePath === current[index])
        ? current
        : next
    })
  }, [effectiveVisibleWorkspaces])

  useEffect(() => {
    if (!environmentData) {
      return
    }

    const actualStatusByPath = new Map(
      environmentData.workspaces.map((workspace) => [workspace.preview.workspacePath, workspace.preview.status]),
    )

    setOptimisticStatusOverrides((current) => {
      let changed = false
      const nextEntries = Object.entries(current).filter(([workspacePath, override]) => {
        const actualStatus = actualStatusByPath.get(workspacePath)
        const keep = actualStatus != null && actualStatus === override.previousStatus
        if (!keep) {
          changed = true
        }
        return keep
      })

      if (!changed) {
        return current
      }

      return Object.fromEntries(nextEntries)
    })
  }, [environmentData])

  useEffect(() => {
    if (selectedWorkspaceIndex >= effectiveVisibleWorkspaces.length) {
      setSelectedWorkspaceIndex(0)
    }
  }, [selectedWorkspaceIndex, effectiveVisibleWorkspaces.length])

  const selectedWorkspace = effectiveVisibleWorkspaces[selectedWorkspaceIndex] ?? null
  const selectedRun = selectedWorkspace?.runs[0] ?? null
  const isNarrowEnvironmentLayout = width < 120
  const markedWorkspaceSet = useMemo(() => new Set(markedWorkspacePaths), [markedWorkspacePaths])
  const markedVisibleWorkspaces = useMemo(
    () => effectiveVisibleWorkspaces.filter((workspace) => markedWorkspaceSet.has(workspace.preview.workspacePath)),
    [effectiveVisibleWorkspaces, markedWorkspaceSet],
  )
  const graphCanvas = useMemo(
    () => buildWorkspaceGraphCanvas({
      graph: dependencyGraph,
      workspaces: effectiveVisibleWorkspaces,
      selectedWorkspacePath: selectedWorkspace?.preview.workspacePath ?? null,
      markedWorkspacePaths: markedWorkspaceSet,
      zoom: graphZoom,
    }),
    [dependencyGraph, effectiveVisibleWorkspaces, graphZoom, markedWorkspaceSet, selectedWorkspace?.preview.workspacePath],
  )

  useEffect(() => {
    if (!selectedRun) {
      setSelectedRunOutput("")
      return
    }

    let cancelled = false

    const loadRunDetails = async (): Promise<void> => {
      try {
        const output = await client.getRunOutput(selectedRun.id)

        if (cancelled) {
          return
        }

        setSelectedRunOutput(output)
      } catch (error) {
        if (!cancelled) {
          setSelectedRunOutput(`Unable to load run output: ${error instanceof Error ? error.message : String(error)}`)
        }
      }
    }

    void loadRunDetails()
    const shouldPoll = selectedRun.status === "running" || selectedRun.status === "pending"
    const interval = shouldPoll ? setInterval(() => void loadRunDetails(), 1_000) : null

    return () => {
      cancelled = true
      if (interval) {
        clearInterval(interval)
      }
    }
  }, [client, selectedRun?.id, selectedRun?.status])

  useEffect(() => {
    if (screen.kind !== "environment" || !screen.autoExecute || autoExecuted || busyAction) {
      return
    }

    if (screen.intent === "browse" || !selectedWorkspace) {
      return
    }

    if (effectiveVisibleWorkspaces.length !== 1) {
      if (screen.selector && effectiveVisibleWorkspaces.length === 0) {
        setToast(`No workspace matched selector ${screen.selector}`)
        setAutoExecuted(true)
      }
      return
    }

    setAutoExecuted(true)
    if (screen.intent === "plan") {
      void handlePlan([selectedWorkspace])
    } else if (screen.intent === "apply") {
      void handleApply([selectedWorkspace])
    }
  }, [autoExecuted, busyAction, effectiveVisibleWorkspaces.length, screen, selectedWorkspace])

  const workspaceDependencies = useMemo(
    () => selectedWorkspace ? getDirectDependencies(dependencyGraph, selectedWorkspace.preview.workspacePath) : [],
    [dependencyGraph, selectedWorkspace],
  )
  const workspaceDependents = useMemo(
    () => selectedWorkspace ? getDirectDependents(dependencyGraph, selectedWorkspace.preview.workspacePath) : [],
    [dependencyGraph, selectedWorkspace],
  )
  const detailContent = useMemo(
    () => selectedWorkspace
      ? buildDetailContent({
          selectedTab,
          selectedWorkspace,
          selectedRun,
          selectedRunOutput,
          dependencyGraph,
          workspaceDependencies,
          workspaceDependents,
          latestRunGroup: environmentData?.runGroups[0] ?? null,
        })
      : "",
    [
      dependencyGraph,
      environmentData?.runGroups,
      selectedRun,
      selectedRunOutput,
      selectedTab,
      selectedWorkspace,
      workspaceDependencies,
      workspaceDependents,
    ],
  )
  const detailViewportHeight = useMemo(
    () => isNarrowEnvironmentLayout
      ? Math.max(10, Math.floor(height * 0.3))
      : Math.max(12, height - 24),
    [height, isNarrowEnvironmentLayout],
  )
  const detailScrollMax = useMemo(
    () => Math.max(0, splitScrollableLines(detailContent).length - detailViewportHeight),
    [detailContent, detailViewportHeight],
  )

  useEffect(() => {
    setDetailScrollOffset(0)
  }, [selectedTab, selectedWorkspace?.preview.id])

  const namedOptions = useMemo(
    () => namedItems.map((item) => ({
      name: item.label,
      description: `${formatStatus(item.status)} • ${item.workspaceCount} ws • ${shortSha(item.headSha)} • ${formatRelativeTime(item.updatedAt)}`,
      value: item,
    }) satisfies PaneOption<BrowserEnvironmentItem>),
    [namedItems],
  )

  const transientOptions = useMemo(
    () => transientItems.map((item) => ({
      name: item.label,
      description: `${formatStatus(item.status)} • ${item.workspaceCount} ws • ${item.authorLogin ? `@${item.authorLogin} • ` : ""}${formatRelativeTime(item.updatedAt)}`,
      value: item,
    }) satisfies PaneOption<BrowserEnvironmentItem>),
    [transientItems],
  )

  const orgOptions = useMemo(
    () => orgs.map((org) => ({
      name: org.slug,
      description: `${org.role} • ${org.source}`,
      value: org,
    }) satisfies PaneOption<OrgInfo>),
    [orgs],
  )

  const helpSections = useMemo(
    () => buildShortcutSections({
      screen,
      activeHomePane,
      fullscreenHomePane,
      activeEnvironmentPane,
      fullscreenEnvironmentPane,
      pendingActionMenu,
      pendingConfirmation,
      markedWorkspaceCount: markedVisibleWorkspaces.length,
    }),
    [
      activeEnvironmentPane,
      activeHomePane,
      fullscreenEnvironmentPane,
      fullscreenHomePane,
      markedVisibleWorkspaces.length,
      pendingActionMenu,
      pendingConfirmation,
      screen,
    ],
  )
  const footerHint = useMemo(
    () => buildFooterHint({
      screen,
      activeHomePane,
      fullscreenHomePane,
      activeEnvironmentPane,
      fullscreenEnvironmentPane,
      pendingActionMenu,
      pendingConfirmation,
      markedWorkspaceCount: markedVisibleWorkspaces.length,
      toast,
    }),
    [
      activeEnvironmentPane,
      activeHomePane,
      fullscreenEnvironmentPane,
      fullscreenHomePane,
      markedVisibleWorkspaces.length,
      pendingActionMenu,
      pendingConfirmation,
      screen,
      toast,
    ],
  )

  useKeyboard((key) => {
    if (isHelpKey(key)) {
      setShowHelp((current) => !current)
      return
    }

    if (showHelp) {
      if (key.name === "escape" || key.name === "q" || key.name === "n") {
        setShowHelp(false)
      }
      return
    }

    if (pendingActionMenu) {
      if (key.name === "escape" || key.name === "n" || key.name === "q") {
        setPendingActionMenu(null)
        return
      }

      if (key.name === "p" || isEnterKey(key)) {
        const nextMenu = pendingActionMenu
        setPendingActionMenu(null)
        setPendingConfirmation({ action: "plan", workspaces: nextMenu.workspaces })
        return
      }

      if (key.name === "a") {
        const nextMenu = pendingActionMenu
        setPendingActionMenu(null)
        setPendingConfirmation({ action: "apply", workspaces: nextMenu.workspaces })
        return
      }
    }

    if (pendingConfirmation) {
      if (key.name === "escape" || key.name === "n" || key.name === "q") {
        setPendingConfirmation(null)
        return
      }

      if (isEnterKey(key) || key.name === "y") {
        const confirmation = pendingConfirmation
        setPendingConfirmation(null)
        if (confirmation.action === "plan") {
          void handlePlan(confirmation.workspaces)
        } else {
          void handleApply(confirmation.workspaces)
        }
        return
      }
    }

    if (key.name === "escape") {
      if (screen.kind === "environment" && fullscreenEnvironmentPane) {
        setFullscreenEnvironmentPane(null)
        return
      }

      if (screen.kind === "dashboard" && fullscreenHomePane) {
        setFullscreenHomePane(null)
        return
      }

      if (screen.kind === "environment") {
        setScreen({ kind: "dashboard", intent: screen.intent })
        return
      }

      renderer.destroy()
      return
    }

    if (key.name === "q" || (key.ctrl && key.name === "c")) {
      renderer.destroy()
      return
    }

    if (screen.kind === "dashboard") {
      if (key.name === "f") {
        setFullscreenHomePane((current) => current === activeHomePane ? null : activeHomePane)
        return
      }

      if (key.name === "tab") {
        setActiveHomePane((current) => {
          if (current === "orgs") return "named"
          if (current === "named") return "transient"
          return "orgs"
        })
        return
      }

      if (key.name === "n") {
        setActiveHomePane("named")
        return
      }

      if (key.name === "p") {
        setActiveHomePane("transient")
        return
      }

      return
    }

    if (key.name === "b") {
      setScreen({ kind: "dashboard", intent: screen.intent })
      return
    }

    if (key.name === "tab") {
      setActiveEnvironmentPane((current) => {
        if (current === "graph") {
          return "details"
        }

        return "graph"
      })
      return
    }

    if (key.name === "f") {
      setFullscreenEnvironmentPane((current) => current === activeEnvironmentPane ? null : activeEnvironmentPane)
      return
    }

    if (!selectedWorkspace || busyAction) {
      return
    }

    if (activeEnvironmentPane === "graph") {
      if (key.name === "space") {
        toggleMarkedWorkspace(selectedWorkspace.preview.workspacePath)
        return
      }

      if (key.name === "+" || key.name === "=") {
        setGraphZoom((current) => clamp(current + 1, 0, 2))
        return
      }

      if (key.name === "-") {
        setGraphZoom((current) => clamp(current - 1, 0, 2))
        return
      }

      if (key.name === "j" || key.name === "down") {
        const nextIndex = getWorkspaceIndexForDirection({
          canvas: graphCanvas,
          currentWorkspacePath: selectedWorkspace?.preview.workspacePath ?? null,
          direction: "down",
          workspaces: effectiveVisibleWorkspaces,
        })
        if (nextIndex != null) {
          setSelectedWorkspaceIndex(nextIndex)
        }
        return
      }

      if (key.name === "k" || key.name === "up") {
        const nextIndex = getWorkspaceIndexForDirection({
          canvas: graphCanvas,
          currentWorkspacePath: selectedWorkspace?.preview.workspacePath ?? null,
          direction: "up",
          workspaces: effectiveVisibleWorkspaces,
        })
        if (nextIndex != null) {
          setSelectedWorkspaceIndex(nextIndex)
        }
        return
      }

      if (key.name === "h" || key.name === "left") {
        const nextIndex = getWorkspaceIndexForDirection({
          canvas: graphCanvas,
          currentWorkspacePath: selectedWorkspace?.preview.workspacePath ?? null,
          direction: "left",
          workspaces: effectiveVisibleWorkspaces,
        })
        if (nextIndex != null) {
          setSelectedWorkspaceIndex(nextIndex)
        }
        return
      }

      if (key.name === "l" || key.name === "right") {
        const nextIndex = getWorkspaceIndexForDirection({
          canvas: graphCanvas,
          currentWorkspacePath: selectedWorkspace?.preview.workspacePath ?? null,
          direction: "right",
          workspaces: effectiveVisibleWorkspaces,
        })
        if (nextIndex != null) {
          setSelectedWorkspaceIndex(nextIndex)
        }
        return
      }

      if (isEnterKey(key)) {
        if (screen.intent === "browse") {
          openActionMenu()
        } else if (screen.intent === "plan") {
          openConfirmation("plan")
        } else if (screen.intent === "apply") {
          openConfirmation("apply")
        }
        return
      }

      if (key.name === "r") {
        openConfirmation("plan")
        return
      }

      if (key.name === "a") {
        openConfirmation("apply")
        return
      }
    }

    if (activeEnvironmentPane === "details") {
      if (key.name === "j" || key.name === "down") {
        setDetailScrollOffset((current) => clamp(current + 1, 0, detailScrollMax))
        return
      }

      if (key.name === "k" || key.name === "up") {
        setDetailScrollOffset((current) => clamp(current - 1, 0, detailScrollMax))
        return
      }

      if (key.name === "h" || key.name === "left") {
        setSelectedTab((current) => nextTab(current, true))
        return
      }

      if (key.name === "l" || key.name === "right") {
        setSelectedTab((current) => nextTab(current, false))
        return
      }

      if (key.name === "pageup") {
        setDetailScrollOffset((current) => clamp(current - Math.max(3, detailViewportHeight - 2), 0, detailScrollMax))
        return
      }

      if (key.name === "pagedown") {
        setDetailScrollOffset((current) => clamp(current + Math.max(3, detailViewportHeight - 2), 0, detailScrollMax))
        return
      }

      if (key.name === "g" && !key.shift) {
        setDetailScrollOffset(0)
        return
      }

      if (key.name === "g" && key.shift) {
        setDetailScrollOffset(detailScrollMax)
        return
      }

      if (key.name === "c" && selectedRun?.status === "running") {
        void handleCancel(selectedRun)
        return
      }
    }
  })

  function toggleMarkedWorkspace(workspacePath: string): void {
    setMarkedWorkspacePaths((current) => {
      if (current.includes(workspacePath)) {
        return current.filter((path) => path !== workspacePath)
      }

      return [...current, workspacePath]
    })
  }

  function getActionTargets(): WorkspaceWithRuns[] {
    return markedVisibleWorkspaces
  }

  function openActionMenu(): void {
    const workspaces = getActionTargets()
    if (workspaces.length === 0) {
      setToast("Mark one or more workspaces with space before queueing an action")
      return
    }

    setPendingActionMenu({ workspaces })
  }

  function openConfirmation(action: "plan" | "apply"): void {
    const workspaces = getActionTargets()
    if (workspaces.length === 0) {
      setToast(`Mark one or more workspaces with space before queueing ${action}`)
      return
    }

    setPendingConfirmation({ action, workspaces })
  }

  async function handlePlan(workspaces: WorkspaceWithRuns[]): Promise<void> {
    await queueWorkspaceBatch({
      action: "plan",
      workspaces,
      queue: (workspace) => client.rerunPreview(workspace.preview.id),
    })
  }

  async function handleApply(workspaces: WorkspaceWithRuns[]): Promise<void> {
    await queueWorkspaceBatch({
      action: "apply",
      workspaces,
      queue: (workspace) => client.triggerApply(workspace.preview.id),
    })
  }

  async function queueWorkspaceBatch(params: {
    action: "plan" | "apply"
    workspaces: WorkspaceWithRuns[]
    queue: (workspace: WorkspaceWithRuns) => Promise<{ jobId: string } | { rerunQueued: boolean; runGroupId: string; jobId: string } | { applyStarted: boolean; jobId: string }>
  }): Promise<void> {
    if (params.workspaces.length === 0) {
      return
    }

    setBusyAction(
      params.workspaces.length === 1
        ? `Queueing ${params.action} for ${params.workspaces[0]!.preview.workspacePath}`
        : `Queueing ${params.action} for ${params.workspaces.length} workspaces`,
    )

    const successes: string[] = []
    const failures: Array<{ workspacePath: string; message: string }> = []

    try {
      for (const workspace of params.workspaces) {
        try {
          await params.queue(workspace)
          successes.push(workspace.preview.workspacePath)
        } catch (error) {
          failures.push({
            workspacePath: workspace.preview.workspacePath,
            message: error instanceof Error ? error.message : String(error),
          })
        }
      }

      if (successes.length > 0) {
        const successSet = new Set(successes)
        setOptimisticStatusOverrides((current) => ({
          ...current,
          ...Object.fromEntries(
            params.workspaces
              .filter((workspace) => successSet.has(workspace.preview.workspacePath))
              .map((workspace) => [
                workspace.preview.workspacePath,
                {
                  status: params.action === "plan" ? "pending" : "applying",
                  previousStatus: workspace.preview.status,
                } satisfies OptimisticStatusOverride,
              ]),
          ),
        }))
        setMarkedWorkspacePaths((current) => current.filter((path) => !successes.includes(path)))
      }

      if (successes.length === 1 && failures.length === 0) {
        setToast(`${capitalize(params.action)} queued for ${successes[0]}`)
        setActiveEnvironmentPane("details")
        setSelectedTab("logs")
        return
      }

      if (successes.length > 0 && failures.length === 0) {
        setToast(`${capitalize(params.action)} queued for ${successes.length} workspaces`)
        return
      }

      if (successes.length > 0 && failures.length > 0) {
        setToast(`${capitalize(params.action)} queued for ${successes.length}, ${failures.length} failed`)
        return
      }

      setToast(failures[0]?.message ?? `${capitalize(params.action)} failed`)
    } finally {
      setBusyAction(null)
    }
  }

  async function handleCancel(run: Run): Promise<void> {
    setBusyAction(`Cancelling ${run.runType} run`)
    try {
      await client.cancelRun(run.id)
      setToast(`Cancelled run ${run.id}`)
      setActiveEnvironmentPane("details")
      setSelectedTab("logs")
    } catch (error) {
      setToast(error instanceof Error ? error.message : String(error))
    } finally {
      setBusyAction(null)
    }
  }

  const title = screen.kind === "dashboard"
    ? screen.intent === "browse" ? "Operator browser" : `${capitalize(screen.intent)} workspace`
    : `${screen.repo} • ${screen.environmentName}`

  return (
    <box
      position="relative"
      width="100%"
      height="100%"
      flexDirection="column"
      backgroundColor={THEME.background}
      padding={1}
      gap={1}
    >
      <box border borderStyle="rounded" borderColor={THEME.borderStrong} backgroundColor={THEME.surfaceRaised} paddingX={1} paddingY={0}>
        <box width="100%" flexDirection="column">
          <text>
            <strong>Yaffle</strong>
            <span fg={THEME.textDim}> // terminal control plane</span>
          </text>
          <text fg={THEME.textDim}>
            {title}
            {selectedOrgSlug ? ` • org ${selectedOrgSlug}` : ""}
            {apiUrl ? ` • ${apiUrl}` : ""}
            {busyAction ? ` • ${busyAction}` : ""}
          </text>
        </box>
      </box>

      {screen.kind === "dashboard"
        ? renderDashboard({
            width,
            browserLoading,
            browserError,
            orgError,
            activeHomePane,
            fullscreenHomePane,
            orgOptions,
            namedOptions,
            transientOptions,
            selectedOrgIndex,
            selectedNamedIndex,
            selectedTransientIndex,
            onSelectOrg: (index) => {
              setSelectedOrgIndex(index)
              setSelectedOrgSlug(orgs[index]?.slug ?? null)
            },
            onBrowseOrg: (index) => {
              setSelectedOrgIndex(index)
              setSelectedOrgSlug(orgs[index]?.slug ?? null)
            },
            onChangeNamed: (index) => setSelectedNamedIndex(index),
            onChangeTransient: (index) => setSelectedTransientIndex(index),
            onOpenNamed: (index) => {
              const next = namedItems[index]
              if (!next || !selectedOrgSlug) {
                return
              }
              setScreen({
                kind: "environment",
                intent: screen.intent,
                org: selectedOrgSlug,
                repo: next.repo,
                environmentName: next.environmentName,
                selector: null,
                autoExecute: false,
              })
            },
            onOpenTransient: (index) => {
              const next = transientItems[index]
              if (!next || !selectedOrgSlug) {
                return
              }
              setScreen({
                kind: "environment",
                intent: screen.intent,
                org: selectedOrgSlug,
                repo: next.repo,
                environmentName: next.environmentName,
                selector: null,
                autoExecute: false,
              })
            },
          })
        : renderEnvironment({
            width,
            height,
            screen,
            environmentData,
            environmentLoading,
            environmentError,
            activeEnvironmentPane,
            fullscreenEnvironmentPane,
            visibleWorkspaces,
            markedWorkspaceCount: markedVisibleWorkspaces.length,
            selectedWorkspaceIndex,
            setSelectedWorkspaceIndex,
            selectedWorkspace,
            selectedRun,
            graphCanvas,
            detailContent,
            detailScrollOffset,
            selectedTab,
            selectorMatchPaths,
          })}

      {pendingActionMenu ? renderActionMenuModal({
        width,
        height,
        menu: pendingActionMenu,
      }) : null}

      {pendingConfirmation ? renderConfirmationModal({
        width,
        height,
        confirmation: pendingConfirmation,
      }) : null}

      {showHelp ? renderHelpModal({
        width,
        height,
        sections: helpSections,
      }) : null}

      <box border borderStyle="rounded" borderColor={THEME.border} backgroundColor={THEME.surfaceMuted} paddingX={1} paddingY={0}>
        <text fg={THEME.textDim}>{footerHint}</text>
      </box>
    </box>
  )
}

function renderDashboard(props: {
  width: number
  browserLoading: boolean
  browserError: string | null
  orgError: string | null
  activeHomePane: HomePane
  fullscreenHomePane: HomePane | null
  orgOptions: Array<PaneOption<OrgInfo>>
  namedOptions: Array<PaneOption<BrowserEnvironmentItem>>
  transientOptions: Array<PaneOption<BrowserEnvironmentItem>>
  selectedOrgIndex: number
  selectedNamedIndex: number
  selectedTransientIndex: number
  onSelectOrg: (index: number) => void
  onBrowseOrg: (index: number) => void
  onChangeNamed: (index: number) => void
  onChangeTransient: (index: number) => void
  onOpenNamed: (index: number) => void
  onOpenTransient: (index: number) => void
}) {
  const isNarrow = props.width < 120
  const showOrgs = !props.fullscreenHomePane || props.fullscreenHomePane === "orgs"
  const showNamed = !props.fullscreenHomePane || props.fullscreenHomePane === "named"
  const showTransient = !props.fullscreenHomePane || props.fullscreenHomePane === "transient"

  return (
    <box flexDirection={isNarrow ? "column" : "row"} flexGrow={1} gap={1}>
      {showOrgs ? <Panel title={`Organizations${props.fullscreenHomePane === "orgs" ? " • fullscreen" : ""}`} active={props.activeHomePane === "orgs"} width={props.fullscreenHomePane === "orgs" ? "100%" : isNarrow ? "100%" : 28} flexGrow={props.fullscreenHomePane === "orgs" ? 1 : undefined}>
        {props.orgError
          ? <ErrorText message={props.orgError} />
          : props.orgOptions.length === 0
            ? <EmptyState message="No organizations visible." />
            : (
              <select
                width="100%"
                height="100%"
                options={props.orgOptions}
                selectedIndex={props.selectedOrgIndex}
                onChange={(index) => props.onSelectOrg(index)}
                onSelect={(index) => props.onBrowseOrg(index)}
                focused={props.activeHomePane === "orgs"}
                selectedBackgroundColor={THEME.selection}
                selectedTextColor={THEME.text}
              />
            )}
      </Panel> : null}

      {showNamed ? <Panel title={`Named environments${props.fullscreenHomePane === "named" ? " • fullscreen" : ""}`} active={props.activeHomePane === "named"} width={props.fullscreenHomePane === "named" ? "100%" : isNarrow ? "100%" : 42} flexGrow={props.fullscreenHomePane === "named" ? 1 : undefined}>
        {props.browserLoading
          ? <LoadingState message="Loading environments…" />
          : props.browserError
            ? <ErrorText message={props.browserError} />
            : props.namedOptions.length === 0
              ? <EmptyState message="No named environments yet." />
              : (
                <select
                  width="100%"
                  height="100%"
                  options={props.namedOptions}
                  selectedIndex={props.selectedNamedIndex}
                  onChange={(index) => props.onChangeNamed(index)}
                  onSelect={(index) => props.onOpenNamed(index)}
                  focused={props.activeHomePane === "named"}
                  selectedBackgroundColor={THEME.selection}
                  selectedTextColor={THEME.text}
                />
              )}
      </Panel> : null}

      {showTransient ? <Panel title={`PR environments${props.fullscreenHomePane === "transient" ? " • fullscreen" : ""}`} active={props.activeHomePane === "transient"} flexGrow={1} width={props.fullscreenHomePane === "transient" ? "100%" : undefined}>
        {props.browserLoading
          ? <LoadingState message="Loading PR run groups…" />
          : props.browserError
            ? <ErrorText message={props.browserError} />
            : props.transientOptions.length === 0
              ? <EmptyState message="No transient environments in flight." />
              : (
                <select
                  width="100%"
                  height="100%"
                  options={props.transientOptions}
                  selectedIndex={props.selectedTransientIndex}
                  onChange={(index) => props.onChangeTransient(index)}
                  onSelect={(index) => props.onOpenTransient(index)}
                  focused={props.activeHomePane === "transient"}
                  selectedBackgroundColor={THEME.selection}
                  selectedTextColor={THEME.text}
                />
              )}
      </Panel> : null}
    </box>
  )
}

function renderEnvironment(props: {
  width: number
  height: number
  screen: Extract<Screen, { kind: "environment" }>
  environmentData: EnvironmentPreviewGroup | null
  environmentLoading: boolean
  environmentError: string | null
  activeEnvironmentPane: EnvironmentPane
  fullscreenEnvironmentPane: EnvironmentPane | null
  visibleWorkspaces: WorkspaceWithRuns[]
  markedWorkspaceCount: number
  selectedWorkspaceIndex: number
  setSelectedWorkspaceIndex: (index: number) => void
  selectedWorkspace: WorkspaceWithRuns | null
  selectedRun: Run | null
  graphCanvas: GraphCanvasLayout
  detailContent: string
  detailScrollOffset: number
  selectedTab: DetailTab
  selectorMatchPaths: string[] | null
}) {
  const isNarrow = props.width < 120
  const latestRunGroup = props.environmentData?.runGroups[0] ?? null
  const showGraphPane = !props.fullscreenEnvironmentPane || props.fullscreenEnvironmentPane === "graph"
  const showDetailsPane = !props.fullscreenEnvironmentPane || props.fullscreenEnvironmentPane === "details"
  const graphPaneWidth = props.fullscreenEnvironmentPane === "graph"
    ? Math.max(48, props.width - 4)
    : isNarrow
      ? Math.max(48, props.width - 6)
      : 56
  const graphViewportWidth = Math.max(32, graphPaneWidth - 4)
  const graphViewportHeight = props.fullscreenEnvironmentPane === "graph"
    ? Math.max(18, props.height - 9)
    : isNarrow
      ? Math.max(12, Math.floor(props.height * 0.35))
      : Math.max(16, props.height - 12)
  const detailViewportHeight = isNarrow
    ? Math.max(10, Math.floor(props.height * 0.3))
    : Math.max(12, props.height - 24)
  const graphOffset = computeGraphViewportOffset({
    canvas: props.graphCanvas,
    selectedWorkspacePath: props.selectedWorkspace?.preview.workspacePath ?? null,
    viewportWidth: graphViewportWidth,
    viewportHeight: graphViewportHeight,
  })

  return (
    <box flexDirection={isNarrow ? "column" : "row"} flexGrow={1} gap={1}>
      {showGraphPane ? <Panel
        title={`Workspace DAG${props.selectorMatchPaths ? ` (${props.visibleWorkspaces.length})` : ""}${props.fullscreenEnvironmentPane === "graph" ? " • fullscreen" : ""}`}
        active={props.activeEnvironmentPane === "graph"}
        width={props.fullscreenEnvironmentPane === "graph" ? "100%" : isNarrow ? "100%" : graphPaneWidth}
        flexGrow={props.fullscreenEnvironmentPane === "graph" ? 1 : undefined}
      >
        {props.environmentLoading && !props.environmentData
          ? <LoadingState message="Loading workspace graph…" />
          : props.environmentError
            ? <ErrorText message={props.environmentError} />
            : props.visibleWorkspaces.length === 0
              ? <EmptyState message={props.selectorMatchPaths ? "No workspace matched this selector." : "No workspaces found in this environment."} />
              : (
                <box width="100%" height="100%" flexDirection="column" gap={1}>
                  <box
                    width={graphViewportWidth}
                    height={graphViewportHeight}
                    overflow="hidden"
                    border
                    borderStyle="single"
                    borderColor={props.activeEnvironmentPane === "graph" ? THEME.borderStrong : THEME.border}
                    backgroundColor={THEME.surfaceMuted}
                  >
                    <box
                      position="relative"
                      left={-graphOffset.x}
                      top={-graphOffset.y}
                      width={props.graphCanvas.width}
                      height={props.graphCanvas.height}
                    >
                      {props.graphCanvas.edges.map((segment) => (
                        <box key={segment.key} position="absolute" left={segment.x} top={segment.y}>
                          <text fg={segment.tone === "selected" ? THEME.accent : segment.tone === "marked" ? THEME.warning : THEME.border} content={segment.text} />
                        </box>
                      ))}
                      {props.graphCanvas.nodes.map((node) => (
                        <box
                          key={node.workspace.preview.id}
                          position="absolute"
                          left={node.x}
                          top={node.y}
                          width={node.width}
                          height={node.height}
                          border
                          borderStyle="rounded"
                          borderColor={node.selected ? THEME.borderStrong : node.marked ? THEME.warning : THEME.border}
                          backgroundColor={node.selected ? THEME.selection : node.marked ? THEME.surface : THEME.surfaceRaised}
                          paddingLeft={1}
                          paddingRight={1}
                          justifyContent="center"
                        >
                          <box flexDirection="column">
                            <text>
                              <span fg={node.marked ? THEME.warning : node.selected ? THEME.accent : THEME.textDim}>{node.marked ? "[x]" : "[ ]"}</span>
                              <span fg={statusColor(node.workspace.preview.status)}>{statusSymbol(node.workspace.preview.status)}</span>
                              <span fg={THEME.text}> {node.title}</span>
                            </text>
                            <text fg={THEME.textDim}>{node.subtitle}</text>
                          </box>
                        </box>
                      ))}
                    </box>
                  </box>
                  <text fg={THEME.textDim}>
                    selected {props.selectedWorkspaceIndex + 1}/{props.visibleWorkspaces.length}
                    {props.markedWorkspaceCount > 0 ? ` • marked ${props.markedWorkspaceCount}` : ""}
                    {props.selectedWorkspace ? ` • ${props.selectedWorkspace.preview.workspacePath}` : ""}
                    {` • zoom ${props.graphCanvas.zoomLabel}`}
                    {props.screen.intent !== "browse" ? ` • enter to ${props.screen.intent}` : ""}
                  </text>
                </box>
              )}
      </Panel> : null}

      {showDetailsPane ? <box flexDirection="column" flexGrow={1} gap={1} width={props.fullscreenEnvironmentPane === "details" ? "100%" : undefined}>
        <Panel
          title={`${props.environmentData?.repo ?? props.screen.repo} • ${props.environmentData?.environmentName ?? props.screen.environmentName}${props.fullscreenEnvironmentPane === "details" ? " • fullscreen" : ""}`}
          active={props.activeEnvironmentPane === "details"}
          height={isNarrow ? undefined : 16}
        >
          <box flexDirection="column" gap={0}>
            <text>
              <strong>Status:</strong>
              <span fg={statusColor(latestRunGroup?.status ?? props.selectedWorkspace?.preview.status ?? "pending")}> {formatStatus(latestRunGroup?.status ?? props.selectedWorkspace?.preview.status ?? "pending")}</span>
              <span fg={THEME.textDim}> • {props.environmentData?.workspaces.length ?? 0} workspaces</span>
            </text>
            <text fg={THEME.textDim}>
              {props.environmentData?.ref ?? ""}
              {props.environmentData?.headSha ? ` • ${shortSha(props.environmentData.headSha)}` : ""}
              {props.environmentData?.authorLogin ? ` • @${props.environmentData.authorLogin}` : ""}
            </text>
            <text fg={THEME.textDim}>
              intent {props.screen.intent}
              {props.screen.selector ? ` • selector ${props.screen.selector}` : ""}
            </text>
            <text>
              <strong>Tabs:</strong>
              <span fg={props.selectedTab === "overview" ? THEME.accent : THEME.textDim}> overview</span>
              <span fg={props.selectedTab === "logs" ? THEME.accent : THEME.textDim}> • logs</span>
              <span fg={props.selectedTab === "outputs" ? THEME.accent : THEME.textDim}> • outputs</span>
            </text>
            <text fg={THEME.textDim}>pane {props.activeEnvironmentPane}</text>
            {props.markedWorkspaceCount > 0 ? <text fg={THEME.warning}>marked {props.markedWorkspaceCount} workspace{props.markedWorkspaceCount === 1 ? "" : "s"}</text> : null}
            {props.selectedWorkspace ? (
              <text>
                <strong>Selected:</strong>
                <span fg={THEME.accent}> {props.selectedWorkspace.preview.workspacePath}</span>
                <span fg={statusColor(props.selectedWorkspace.preview.status)}> • {formatStatus(props.selectedWorkspace.preview.status)}</span>
              </text>
            ) : null}
          </box>
        </Panel>

        <Panel title={detailTitle(props.selectedTab)} active={props.activeEnvironmentPane === "details"} flexGrow={1}>
          {props.selectedWorkspace
            ? renderDetailBody({
                selectedTab: props.selectedTab,
                selectedWorkspace: props.selectedWorkspace,
                content: props.detailContent,
                scrollOffset: props.detailScrollOffset,
                viewportHeight: detailViewportHeight,
              })
            : <EmptyState message="Select a workspace to inspect run history, logs, and outputs." />}
        </Panel>
      </box> : null}
    </box>
  )
}

function renderConfirmationModal(props: {
  width: number
  height: number
  confirmation: PendingActionConfirmation
}) {
  const modalWidth = Math.min(72, Math.max(42, props.width - 12))
  const modalHeight = Math.min(16, Math.max(10, props.height - 10))
  const sample = props.confirmation.workspaces
    .slice(0, 5)
    .map((workspace) => workspace.preview.workspacePath)
  const remaining = props.confirmation.workspaces.length - sample.length

  return (
    <box
      position="absolute"
      left={0}
      top={0}
      width="100%"
      height="100%"
      justifyContent="center"
      alignItems="center"
      backgroundColor="rgba(0,0,0,0.45)"
      zIndex={10}
    >
      <box
        width={modalWidth}
        height={modalHeight}
        border
        borderStyle="double"
        borderColor={props.confirmation.action === "apply" ? THEME.warning : THEME.borderStrong}
        backgroundColor={THEME.surfaceRaised}
        padding={1}
        flexDirection="column"
        gap={1}
      >
        <text>
          <strong>{capitalize(props.confirmation.action)}</strong>
          <span fg={THEME.textDim}> {props.confirmation.workspaces.length} workspace{props.confirmation.workspaces.length === 1 ? "" : "s"}</span>
        </text>
        <text fg={THEME.textDim}>
          Confirm remote {props.confirmation.action} for the selected workspaces.
        </text>
        <box flexDirection="column" border borderStyle="rounded" borderColor={THEME.border} padding={1} flexGrow={1}>
          {sample.map((workspacePath) => (
            <text key={workspacePath}>{workspacePath}</text>
          ))}
          {remaining > 0 ? <text fg={THEME.textDim}>… and {remaining} more</text> : null}
        </box>
        <text fg={THEME.textDim}>enter / y confirm • esc / n cancel</text>
      </box>
    </box>
  )
}

function renderActionMenuModal(props: {
  width: number
  height: number
  menu: PendingActionMenu
}) {
  const modalWidth = Math.min(56, Math.max(38, props.width - 18))
  const sample = props.menu.workspaces
    .slice(0, 4)
    .map((workspace) => workspace.preview.workspacePath)
  const remaining = props.menu.workspaces.length - sample.length

  return (
    <box
      position="absolute"
      left={0}
      top={0}
      width="100%"
      height="100%"
      justifyContent="center"
      alignItems="center"
      backgroundColor="rgba(0,0,0,0.45)"
      zIndex={9}
    >
      <box
        width={modalWidth}
        border
        borderStyle="double"
        borderColor={THEME.borderStrong}
        backgroundColor={THEME.surfaceRaised}
        padding={1}
        flexDirection="column"
        gap={1}
      >
        <text>
          <strong>Queue action</strong>
          <span fg={THEME.textDim}> • {props.menu.workspaces.length} workspace{props.menu.workspaces.length === 1 ? "" : "s"}</span>
        </text>
        <box flexDirection="column" border borderStyle="rounded" borderColor={THEME.border} padding={1}>
          {sample.map((workspacePath) => (
            <text key={workspacePath}>{workspacePath}</text>
          ))}
          {remaining > 0 ? <text fg={THEME.textDim}>… and {remaining} more</text> : null}
        </box>
        <text fg={THEME.textDim}>p / enter plan • a apply • esc cancel</text>
      </box>
    </box>
  )
}

function renderHelpModal(props: {
  width: number
  height: number
  sections: ShortcutSection[]
}) {
  const modalWidth = Math.min(84, Math.max(52, props.width - 10))
  const modalHeight = Math.min(28, Math.max(14, props.height - 6))

  return (
    <box
      position="absolute"
      left={0}
      top={0}
      width="100%"
      height="100%"
      justifyContent="center"
      alignItems="center"
      backgroundColor="rgba(0,0,0,0.45)"
      zIndex={11}
    >
      <box
        width={modalWidth}
        height={modalHeight}
        border
        borderStyle="double"
        borderColor={THEME.borderStrong}
        backgroundColor={THEME.surfaceRaised}
        padding={1}
        flexDirection="column"
        gap={1}
      >
        <text>
          <strong>Keyboard shortcuts</strong>
          <span fg={THEME.textDim}> • context aware</span>
        </text>
        <scrollbox width="100%" height="100%">
          <box flexDirection="column" gap={1}>
            {props.sections.map((section) => (
              <box key={section.title} flexDirection="column" border borderStyle="rounded" borderColor={THEME.border} padding={1}>
                <text fg={THEME.accent}><strong>{section.title}</strong></text>
                {section.items.map((item) => (
                  <text key={`${section.title}:${item.keys}`}>
                    <span fg={THEME.warning}>{item.keys.padEnd(18, " ")}</span>
                    <span fg={THEME.text}> {item.description}</span>
                  </text>
                ))}
              </box>
            ))}
          </box>
        </scrollbox>
        <text fg={THEME.textDim}>? / esc close</text>
      </box>
    </box>
  )
}

function buildDetailContent(props: {
  selectedTab: DetailTab
  selectedWorkspace: WorkspaceWithRuns
  selectedRun: Run | null
  selectedRunOutput: string
  dependencyGraph: DependencyGraph | null
  workspaceDependencies: string[]
  workspaceDependents: string[]
  latestRunGroup: EnvironmentPreviewGroup["runGroups"][number] | null
}): string {
  if (props.selectedTab === "logs") {
    return props.selectedRunOutput || "No run output yet."
  }

  if (props.selectedTab === "outputs") {
    return safeJson(props.selectedWorkspace.outputs)
  }

  return formatOverview(props)
}

function buildStyledDetailLines(tab: DetailTab, content: string): StyledChunk[][] {
  const lines = splitScrollableLines(content)

  if (tab === "outputs") {
    return lines.map(styleJsonLine)
  }

  if (tab === "logs") {
    return lines.map(styleTerraformLogLine)
  }

  return lines.map((line) => [{ text: line || " " }])
}

function styleJsonLine(line: string): StyledChunk[] {
  if (line.length === 0) {
    return []
  }

  const keyMatch = line.match(/^(\s*)("(?:\\.|[^"])*")(\s*:\s*)(.*)$/)
  if (keyMatch) {
    const [, indent, key, separator, rest] = keyMatch
    return [
      { text: indent },
      { text: key, fg: THEME.accent },
      { text: separator, fg: THEME.textDim },
      ...styleJsonValue(rest),
    ]
  }

  return styleJsonValue(line)
}

function styleJsonValue(input: string): StyledChunk[] {
  const chunks: StyledChunk[] = []
  let remaining = input

  while (remaining.length > 0) {
    const punctuationMatch = remaining.match(/^[\[\]{}:,]+/)
    if (punctuationMatch) {
      chunks.push({ text: punctuationMatch[0], fg: THEME.textDim })
      remaining = remaining.slice(punctuationMatch[0].length)
      continue
    }

    const whitespaceMatch = remaining.match(/^\s+/)
    if (whitespaceMatch) {
      chunks.push({ text: whitespaceMatch[0] })
      remaining = remaining.slice(whitespaceMatch[0].length)
      continue
    }

    const stringMatch = remaining.match(/^"(?:\\.|[^"])*"/)
    if (stringMatch) {
      chunks.push({ text: stringMatch[0], fg: THEME.warning })
      remaining = remaining.slice(stringMatch[0].length)
      continue
    }

    const numberMatch = remaining.match(/^-?(?:0|[1-9]\d*)(?:\.\d+)?(?:[eE][+-]?\d+)?/)
    if (numberMatch) {
      chunks.push({ text: numberMatch[0], fg: THEME.info })
      remaining = remaining.slice(numberMatch[0].length)
      continue
    }

    const keywordMatch = remaining.match(/^(true|false|null)/)
    if (keywordMatch) {
      chunks.push({ text: keywordMatch[0], fg: THEME.textMuted })
      remaining = remaining.slice(keywordMatch[0].length)
      continue
    }

    chunks.push({ text: remaining[0] })
    remaining = remaining.slice(1)
  }

  return chunks
}

function styleTerraformLogLine(line: string): StyledChunk[] {
  if (line.length === 0) {
    return []
  }

  if (/^\s*Error:/.test(line)) {
    return [{ text: line, fg: THEME.danger }]
  }

  if (/^\s*Warning:/.test(line)) {
    return [{ text: line, fg: THEME.warning }]
  }

  if (/^\s*(No changes\.|Apply complete!|Destroy complete!)/.test(line)) {
    return [{ text: line, fg: THEME.success }]
  }

  if (/^\s*(Plan:|Changes to Outputs:|Outputs:|Terraform will perform the following actions:)/.test(line)) {
    return [{ text: line, fg: THEME.accent }]
  }

  if (/^\s*# /.test(line)) {
    return [{ text: line, fg: THEME.info }]
  }

  if (/^- /.test(line)) {
    return [{ text: line, fg: THEME.textDim }]
  }

  const planActionMatch = line.match(/^(\s{2,})([+\-~])(\s.*)$/)
  if (planActionMatch) {
    const [, indent, action, rest] = planActionMatch
    const color = action === "+" ? THEME.statusApplying : action === "-" ? THEME.danger : THEME.warning

    return [
      { text: indent },
      { text: action, fg: color },
      { text: rest, fg: color },
    ]
  }

  if (/^\s*(module\.|data\.|resource\.|output\.|var\.)/.test(line)) {
    return [{ text: line, fg: THEME.info }]
  }

  if (/^\s*(Reading\.\.\.|Refreshing state\.\.\.|╷|╵|│|Note:|Saved the plan to:)/.test(line)) {
    return [{ text: line, fg: THEME.textDim }]
  }

  return [{ text: line }]
}

function renderDetailBody(props: {
  selectedTab: DetailTab
  selectedWorkspace: WorkspaceWithRuns
  content: string
  scrollOffset: number
  viewportHeight: number
}) {
  const renderedLines = buildStyledDetailLines(props.selectedTab, props.content)
  const visibleLines = renderedLines.slice(props.scrollOffset, props.scrollOffset + props.viewportHeight)

  return (
    <box width="100%" height="100%" flexDirection="column" gap={1}>
      <box flexGrow={1} border borderStyle="single" borderColor={THEME.border} backgroundColor={THEME.surfaceMuted} padding={1}>
        <box flexDirection="column" width="100%">
          {visibleLines.map((line, index) => (
            <text key={`${props.scrollOffset + index}:${line.map((chunk) => chunk.text).join("")}`}>
              {line.length > 0
                ? line.map((chunk, chunkIndex) => (
                    <span key={`${index}:${chunkIndex}`} fg={chunk.fg}>
                      {chunk.text}
                    </span>
                  ))
                : " "}
            </text>
          ))}
        </box>
      </box>
      <text fg={THEME.textDim}>
        scroll {props.scrollOffset + 1}/{Math.max(1, splitScrollableLines(props.content).length)}
      </text>
    </box>
  )
}

function Panel({
  title,
  active,
  width,
  height,
  flexGrow,
  children,
}: {
  title: string
  active: boolean
  width?: Dimension
  height?: Dimension
  flexGrow?: number
  children: ReactNode
}) {
  return (
    <box
      border
      borderStyle="rounded"
      borderColor={active ? THEME.borderStrong : THEME.border}
      backgroundColor={THEME.surface}
      title={title}
      padding={1}
      width={width}
      height={height}
      flexGrow={flexGrow}
    >
      {children}
    </box>
  )
}

function LoadingState({ message }: { message: string }) {
  return <text fg={THEME.textDim}>{message}</text>
}

function EmptyState({ message }: { message: string }) {
  return <text fg={THEME.textMuted}>{message}</text>
}

function ErrorText({ message }: { message: string }) {
  return <text fg={THEME.danger}>{message}</text>
}

function buildNamedItems(groups: EnvironmentGroup[]): BrowserEnvironmentItem[] {
  return groups
    .map((group) => ({
      key: `${group.repo}:${group.environmentName}`,
      kind: "named" as const,
      repo: group.repo,
      environmentName: group.environmentName,
      label: `${group.repo} / ${group.environmentName}`,
      ref: group.ref,
      headSha: group.headSha,
      status: group.status,
      workspaceCount: group.workspaces.length,
      updatedAt: group.updatedAt,
      prNumber: null,
      authorLogin: null,
    }))
    .sort((a, b) => b.updatedAt.localeCompare(a.updatedAt))
}

function buildTransientItems(overview: PreviewOverviewResponse): BrowserEnvironmentItem[] {
  const groups = new Map<string, BrowserEnvironmentItem>()
  const workspaceCounts = new Map<string, number>()
  const statuses = new Map<string, string[]>()

  for (const preview of overview.data) {
    if (preview.environmentKind !== "transient") {
      continue
    }

    const environmentName = preview.environmentName ?? (preview.prNumber ? `pr-${preview.prNumber}` : "preview")
    const key = `${preview.repo}:${environmentName}`
    workspaceCounts.set(key, (workspaceCounts.get(key) ?? 0) + 1)
    statuses.set(key, [...(statuses.get(key) ?? []), preview.status])

    const existing = groups.get(key)
    if (!existing) {
      groups.set(key, {
        key,
        kind: "transient",
        repo: preview.repo,
        environmentName,
        label: `${preview.repo} #${preview.prNumber ?? "?"}`,
        ref: preview.ref ?? "",
        headSha: preview.headSha ?? "",
        status: preview.status,
        workspaceCount: 1,
        updatedAt: preview.createdAt,
        prNumber: preview.prNumber ?? null,
        authorLogin: preview.authorLogin ?? null,
      })
      continue
    }

    if (preview.createdAt > existing.updatedAt) {
      existing.updatedAt = preview.createdAt
      existing.headSha = preview.headSha ?? existing.headSha
      existing.ref = preview.ref ?? existing.ref
      existing.authorLogin = preview.authorLogin ?? existing.authorLogin
    }
  }

  return Array.from(groups.values())
    .map((group) => ({
      ...group,
      workspaceCount: workspaceCounts.get(group.key) ?? group.workspaceCount,
      status: aggregateStatuses(statuses.get(group.key) ?? [group.status]),
    }))
    .sort((a, b) => b.updatedAt.localeCompare(a.updatedAt))
}

function buildWorkspaceGraphCanvas(params: {
  graph: DependencyGraph | null
  workspaces: WorkspaceWithRuns[]
  selectedWorkspacePath: string | null
  markedWorkspacePaths: Set<string>
  zoom: number
}): GraphCanvasLayout {
  const zoomConfig = getGraphZoomConfig(params.zoom)

  if (params.workspaces.length === 0) {
    return {
      width: 1,
      height: 1,
      zoomLabel: zoomConfig.label,
      nodes: [],
      edges: [],
      columns: [],
      positions: new Map(),
    }
  }

  const byPath = new Map(
    params.workspaces.map((workspace) => [workspace.preview.workspacePath, workspace]),
  )
  const paths = orderedVisiblePaths(params.graph, params.workspaces)
  const nodeWidth = clamp(
    Math.round(Math.max(...paths.map((workspacePath) => workspacePath.length), 18) * zoomConfig.nameScale + 6),
    zoomConfig.nodeWidthMin,
    zoomConfig.nodeWidthMax,
  )
  const nodeHeight = zoomConfig.nodeHeight
  const columnGap = zoomConfig.columnGap
  const rowGap = zoomConfig.rowGap
  const paddingX = 2
  const paddingY = 1

  const nodes: GraphNodeLayout[] = []

  if (!params.graph) {
    params.workspaces.forEach((workspace, index) => {
      nodes.push({
        workspace,
        x: paddingX,
        y: paddingY + index * (nodeHeight + rowGap),
        width: nodeWidth,
        height: nodeHeight,
        title: truncateMiddle(workspace.preview.workspacePath, nodeWidth - 4),
        subtitle: buildNodeSubtitle(workspace),
        selected: workspace.preview.workspacePath === params.selectedWorkspacePath,
        marked: params.markedWorkspacePaths.has(workspace.preview.workspacePath),
      })
    })

    return {
      width: nodeWidth + paddingX * 2,
      height: nodes[nodes.length - 1]!.y + nodeHeight + paddingY,
      zoomLabel: zoomConfig.label,
      nodes,
      edges: [],
      columns: [params.workspaces.map((workspace) => workspace.preview.workspacePath)],
      positions: new Map(params.workspaces.map((workspace, index) => [workspace.preview.workspacePath, { column: 0, row: index }])),
    }
  }

  const visibleSet = new Set(paths)
  const depths = computeGraphDepths(params.graph, visibleSet)
  const dependencies = new Map<string, string[]>()
  const dependents = new Map<string, string[]>()

  for (const workspacePath of paths) {
    dependencies.set(workspacePath, [])
    dependents.set(workspacePath, [])
  }

  for (const [source, target] of params.graph.edges) {
    if (!visibleSet.has(source) || !visibleSet.has(target)) {
      continue
    }

    dependencies.set(source, [...(dependencies.get(source) ?? []), target])
    dependents.set(target, [...(dependents.get(target) ?? []), source])
  }

  const maxDepth = Math.max(...Array.from(depths.values()), 0)
  const columns = Array.from({ length: maxDepth + 1 }, () => [] as string[])
  for (const workspacePath of paths) {
    columns[depths.get(workspacePath) ?? 0].push(workspacePath)
  }

  const orderIndex = new Map(paths.map((workspacePath, index) => [workspacePath, index]))
  const rowHints = new Map<string, number>()

  for (let columnIndex = 0; columnIndex < columns.length; columnIndex++) {
    columns[columnIndex].sort((left, right) => {
      const leftHint = averageRowHint(dependencies.get(left) ?? [], rowHints)
      const rightHint = averageRowHint(dependencies.get(right) ?? [], rowHints)

      if (leftHint !== rightHint) {
        return leftHint - rightHint
      }

      return (orderIndex.get(left) ?? 0) - (orderIndex.get(right) ?? 0)
    })

    columns[columnIndex].forEach((workspacePath, rowIndex) => {
      rowHints.set(workspacePath, rowIndex)
    })
  }

  for (let columnIndex = columns.length - 1; columnIndex >= 0; columnIndex--) {
    columns[columnIndex].sort((left, right) => {
      const leftHint = averageRowHint(dependents.get(left) ?? [], rowHints)
      const rightHint = averageRowHint(dependents.get(right) ?? [], rowHints)

      if (leftHint !== rightHint) {
        return leftHint - rightHint
      }

      return (orderIndex.get(left) ?? 0) - (orderIndex.get(right) ?? 0)
    })

    columns[columnIndex].forEach((workspacePath, rowIndex) => {
      rowHints.set(workspacePath, rowIndex)
    })
  }

  const maxRows = Math.max(...columns.map((column) => column.length), 1)
  const positions = new Map<string, { column: number; row: number }>()

  for (let columnIndex = 0; columnIndex < columns.length; columnIndex++) {
    const column = columns[columnIndex]
    const topOffset = Math.floor(((maxRows - column.length) * (nodeHeight + rowGap)) / 2)

    column.forEach((workspacePath, rowIndex) => {
      const workspace = byPath.get(workspacePath)
      if (!workspace) {
        return
      }

      nodes.push({
        workspace,
        x: paddingX + columnIndex * (nodeWidth + columnGap),
        y: paddingY + topOffset + rowIndex * (nodeHeight + rowGap),
        width: nodeWidth,
        height: nodeHeight,
        title: truncateMiddle(workspace.preview.workspacePath, nodeWidth - 4),
        subtitle: buildNodeSubtitle(workspace),
        selected: workspace.preview.workspacePath === params.selectedWorkspacePath,
        marked: params.markedWorkspacePaths.has(workspace.preview.workspacePath),
      })
      positions.set(workspacePath, { column: columnIndex, row: rowIndex })
    })
  }

  const layoutByPath = new Map(nodes.map((node) => [node.workspace.preview.workspacePath, node]))
  const edges: GraphEdgeSegment[] = []

  for (const [source, target] of params.graph.edges) {
    const dependency = layoutByPath.get(target)
    const dependent = layoutByPath.get(source)
    if (!dependency || !dependent) {
      continue
    }

    const tone = source === params.selectedWorkspacePath || target === params.selectedWorkspacePath
      ? "selected"
      : params.markedWorkspacePaths.has(source) || params.markedWorkspacePaths.has(target)
        ? "marked"
        : "normal"
    edges.push(...buildEdgeSegments({
      fromX: dependency.x + dependency.width,
      fromY: dependency.y + Math.floor(dependency.height / 2),
      toX: dependent.x - 1,
      toY: dependent.y + Math.floor(dependent.height / 2),
      tone,
      keyBase: `${target}->${source}`,
    }))
  }

  const maxRight = Math.max(...nodes.map((node) => node.x + node.width), 1)
  const maxBottom = Math.max(...nodes.map((node) => node.y + node.height), 1)

  return {
    width: maxRight + paddingX,
    height: maxBottom + paddingY,
    zoomLabel: zoomConfig.label,
    nodes,
    edges,
    columns,
    positions,
  }
}

function orderedVisiblePaths(
  graph: DependencyGraph | null,
  workspaces: WorkspaceWithRuns[],
): string[] {
  const visible = new Set(workspaces.map((workspace) => workspace.preview.workspacePath))
  if (!graph) {
    return workspaces.map((workspace) => workspace.preview.workspacePath)
  }

  const ordered = graph.workspaces.filter((workspacePath) => visible.has(workspacePath))
  const extras = workspaces
    .map((workspace) => workspace.preview.workspacePath)
    .filter((workspacePath) => !ordered.includes(workspacePath))

  return [...ordered, ...extras]
}

function applyOptimisticWorkspaceStatus(
  workspace: WorkspaceWithRuns,
  overrides: Record<string, OptimisticStatusOverride>,
): WorkspaceWithRuns {
  const override = overrides[workspace.preview.workspacePath]
  if (!override) {
    return workspace
  }

  return {
    ...workspace,
    preview: {
      ...workspace.preview,
      status: override.status,
    },
    runs: workspace.runs.map((run, index) => index === 0 ? { ...run, status: override.status } : run),
  }
}

function computeGraphDepths(
  graph: DependencyGraph,
  visibleSet: Set<string>,
): Map<string, number> {
  const depths = new Map<string, number>()
  const dependencies = new Map<string, Set<string>>()

  for (const workspacePath of visibleSet) {
    depths.set(workspacePath, 0)
  }

  for (const [source, target] of graph.edges) {
    if (!visibleSet.has(source) || !visibleSet.has(target)) {
      continue
    }

    if (!dependencies.has(source)) {
      dependencies.set(source, new Set())
    }

    dependencies.get(source)?.add(target)
  }

  let changed = true
  while (changed) {
    changed = false
    for (const workspacePath of visibleSet) {
      const deps = dependencies.get(workspacePath)
      if (!deps || deps.size === 0) {
        continue
      }

      let maxDepth = 0
      for (const dependency of deps) {
        maxDepth = Math.max(maxDepth, depths.get(dependency) ?? 0)
      }

      const nextDepth = maxDepth + 1
      if (nextDepth !== depths.get(workspacePath)) {
        depths.set(workspacePath, nextDepth)
        changed = true
      }
    }
  }

  return depths
}

function averageRowHint(paths: string[], rowHints: Map<string, number>): number {
  const hints = paths
    .map((workspacePath) => rowHints.get(workspacePath))
    .filter((hint): hint is number => hint != null)

  if (hints.length === 0) {
    return Number.MAX_SAFE_INTEGER
  }

  return hints.reduce((sum, hint) => sum + hint, 0) / hints.length
}

function buildNodeSubtitle(workspace: WorkspaceWithRuns): string {
  const latestRun = workspace.runs[0]
  const parts = [formatStatus(workspace.preview.status)]

  if (latestRun?.runType) {
    parts.push(latestRun.runType)
  }

  if (latestRun?.planSummary) {
    parts.push(latestRun.planSummary)
  } else if (!latestRun) {
    parts.push("no runs")
  }

  return truncateMiddle(parts.join(" • "), 30)
}

function getGraphZoomConfig(zoom: number): {
  label: string
  nameScale: number
  nodeWidthMin: number
  nodeWidthMax: number
  nodeHeight: number
  columnGap: number
  rowGap: number
} {
  if (zoom <= 0) {
    return {
      label: "small",
      nameScale: 0.7,
      nodeWidthMin: 18,
      nodeWidthMax: 26,
      nodeHeight: 4,
      columnGap: 6,
      rowGap: 1,
    }
  }

  if (zoom >= 2) {
    return {
      label: "large",
      nameScale: 1.15,
      nodeWidthMin: 28,
      nodeWidthMax: 42,
      nodeHeight: 6,
      columnGap: 10,
      rowGap: 3,
    }
  }

  return {
    label: "medium",
    nameScale: 0.9,
    nodeWidthMin: 22,
    nodeWidthMax: 34,
    nodeHeight: 5,
    columnGap: 8,
    rowGap: 2,
  }
}

function computeGraphViewportOffset(params: {
  canvas: GraphCanvasLayout
  selectedWorkspacePath: string | null
  viewportWidth: number
  viewportHeight: number
}): { x: number; y: number } {
  const selectedNode = params.selectedWorkspacePath
    ? params.canvas.nodes.find((node) => node.workspace.preview.workspacePath === params.selectedWorkspacePath)
    : null

  if (!selectedNode) {
    return { x: 0, y: 0 }
  }

  const centerX = selectedNode.x + Math.floor(selectedNode.width / 2)
  const centerY = selectedNode.y + Math.floor(selectedNode.height / 2)
  const desiredX = centerX - Math.floor(params.viewportWidth / 2)
  const desiredY = centerY - Math.floor(params.viewportHeight / 2)

  return {
    x: clamp(desiredX, 0, Math.max(0, params.canvas.width - params.viewportWidth)),
    y: clamp(desiredY, 0, Math.max(0, params.canvas.height - params.viewportHeight)),
  }
}

function getWorkspaceIndexForDirection(params: {
  canvas: GraphCanvasLayout
  currentWorkspacePath: string | null
  direction: "left" | "right" | "up" | "down"
  workspaces: WorkspaceWithRuns[]
}): number | null {
  if (!params.currentWorkspacePath) {
    return params.workspaces.length > 0 ? 0 : null
  }

  const currentPosition = params.canvas.positions.get(params.currentWorkspacePath)
  if (!currentPosition) {
    return null
  }

  let nextPath: string | null = null

  if (params.direction === "up" || params.direction === "down") {
    const column = params.canvas.columns[currentPosition.column] ?? []
    const delta = params.direction === "up" ? -1 : 1
    const nextRow = currentPosition.row + delta
    if (nextRow >= 0 && nextRow < column.length) {
      nextPath = column[nextRow] ?? null
    }
  } else {
    const delta = params.direction === "left" ? -1 : 1
    let targetColumnIndex = currentPosition.column + delta
    while (targetColumnIndex >= 0 && targetColumnIndex < params.canvas.columns.length) {
      const column = params.canvas.columns[targetColumnIndex] ?? []
      if (column.length > 0) {
        const nextRow = clamp(currentPosition.row, 0, column.length - 1)
        nextPath = column[nextRow] ?? null
        break
      }
      targetColumnIndex += delta
    }
  }

  if (!nextPath) {
    return null
  }

  const index = params.workspaces.findIndex(
    (workspace) => workspace.preview.workspacePath === nextPath,
  )

  return index >= 0 ? index : null
}

function buildEdgeSegments(params: {
  fromX: number
  fromY: number
  toX: number
  toY: number
  tone: GraphEdgeSegment["tone"]
  keyBase: string
}): GraphEdgeSegment[] {
  const segments: GraphEdgeSegment[] = []

  if (params.toX <= params.fromX) {
    return segments
  }

  if (params.fromY === params.toY) {
    segments.push({
      key: `${params.keyBase}:h`,
      x: params.fromX,
      y: params.fromY,
      text: "─".repeat(params.toX - params.fromX + 1),
      tone: params.tone,
    })
    return segments
  }

  const bendX = Math.max(params.fromX + 2, Math.min(params.toX - 2, Math.floor((params.fromX + params.toX) / 2)))
  const topY = Math.min(params.fromY, params.toY)
  const bottomY = Math.max(params.fromY, params.toY)

  if (bendX > params.fromX) {
    segments.push({
      key: `${params.keyBase}:h1`,
      x: params.fromX,
      y: params.fromY,
      text: "─".repeat(bendX - params.fromX),
      tone: params.tone,
    })
  }

  segments.push({
    key: `${params.keyBase}:corner1`,
    x: bendX,
    y: params.fromY,
    text: params.toY > params.fromY ? "┐" : "┘",
    tone: params.tone,
  })

  if (bottomY - topY > 1) {
    segments.push({
      key: `${params.keyBase}:v`,
      x: bendX,
      y: topY + 1,
      text: Array.from({ length: bottomY - topY - 1 }, () => "│").join("\n"),
      tone: params.tone,
    })
  }

  segments.push({
    key: `${params.keyBase}:corner2`,
    x: bendX,
    y: params.toY,
    text: params.toY > params.fromY ? "└" : "┌",
    tone: params.tone,
  })

  if (params.toX > bendX) {
    segments.push({
      key: `${params.keyBase}:h2`,
      x: bendX + 1,
      y: params.toY,
      text: "─".repeat(params.toX - bendX),
      tone: params.tone,
    })
  }

  return segments
}

function aggregateStatuses(statuses: string[]): string {
  if (statuses.some((status) => status === "failed")) return "failed"
  if (statuses.some((status) => status === "applying")) return "applying"
  if (statuses.some((status) => status === "planning")) return "planning"
  if (statuses.some((status) => status === "awaiting_approval")) return "awaiting_approval"
  if (statuses.some((status) => status === "pending")) return "pending"
  if (statuses.every((status) => status === "ready")) return "ready"
  return statuses[0] ?? "unknown"
}

function detailTitle(tab: DetailTab): string {
  switch (tab) {
    case "logs":
      return "Latest run logs"
    case "outputs":
      return "Current outputs"
    default:
      return "Workspace overview"
  }
}

function nextTab(current: DetailTab, reverse: boolean): DetailTab {
  const index = DETAIL_TABS.indexOf(current)
  const nextIndex = reverse
    ? (index - 1 + DETAIL_TABS.length) % DETAIL_TABS.length
    : (index + 1) % DETAIL_TABS.length

  return DETAIL_TABS[nextIndex]
}

function formatOverview(props: {
  selectedWorkspace: WorkspaceWithRuns
  selectedRun: Run | null
  workspaceDependencies: string[]
  workspaceDependents: string[]
  latestRunGroup: EnvironmentPreviewGroup["runGroups"][number] | null
  dependencyGraph: DependencyGraph | null
}): string {
  const lines = [
    `workspace: ${props.selectedWorkspace.preview.workspacePath}`,
    `status: ${formatStatus(props.selectedWorkspace.preview.status)}`,
    `approval required: ${props.selectedWorkspace.preview.requireApproval ? "yes" : "no"}`,
    `connection status: ${props.selectedWorkspace.preview.connectionStatus}`,
  ]

  if (props.selectedWorkspace.preview.blockedReason) {
    lines.push(`blocked: ${props.selectedWorkspace.preview.blockedReason}`)
  }

  lines.push("")
  lines.push(`latest run: ${props.selectedRun ? `${props.selectedRun.runType} • ${props.selectedRun.status}` : "none"}`)
  if (props.selectedRun?.planSummary) {
    lines.push(`plan summary: ${props.selectedRun.planSummary}`)
  }
  if (props.selectedRun?.errorMessage) {
    lines.push(`error: ${props.selectedRun.errorMessage}`)
  }
  if (props.selectedRun?.createdAt) {
    lines.push(`queued: ${formatRelativeTime(props.selectedRun.createdAt)}`)
  }
  if (props.selectedRun?.completedAt) {
    lines.push(`completed: ${formatRelativeTime(props.selectedRun.completedAt)}`)
  }

  lines.push("")
  lines.push(`current run group: ${props.latestRunGroup ? `${props.latestRunGroup.status} • ${shortSha(props.latestRunGroup.headSha)}` : "none"}`)
  lines.push(`graph nodes: ${props.dependencyGraph?.workspaces.length ?? props.selectedWorkspace.runs.length}`)

  lines.push("")
  lines.push(`depends on: ${props.workspaceDependencies.length > 0 ? props.workspaceDependencies.join(", ") : "none"}`)
  lines.push(`dependents: ${props.workspaceDependents.length > 0 ? props.workspaceDependents.join(", ") : "none"}`)

  if (props.selectedWorkspace.runs.length > 0) {
    lines.push("")
    lines.push("recent runs:")
    for (const run of props.selectedWorkspace.runs.slice(0, 5)) {
      lines.push(`  - ${run.runType} • ${formatStatus(run.status)} • ${formatRelativeTime(run.createdAt)}`)
    }
  }

  return lines.join("\n")
}

function safeJson(value: unknown): string {
  if (value == null) {
    return "No outputs recorded for this workspace yet."
  }

  try {
    return JSON.stringify(value, null, 2)
  } catch {
    return String(value)
  }
}

function tailText(content: string, lineCount: number): string {
  const lines = content.split("\n")
  return lines.slice(-lineCount).join("\n")
}

function splitScrollableLines(content: string): string[] {
  return content.length === 0 ? [""] : content.split("\n")
}

function sliceScrollableText(content: string, offset: number, viewportHeight: number): string {
  const lines = splitScrollableLines(content)
  return lines.slice(offset, offset + viewportHeight).join("\n")
}

function isEnterKey(key: { name: string; sequence?: string }): boolean {
  return key.name === "enter" || key.name === "return" || key.sequence === "\r" || key.sequence === "\n"
}

function isHelpKey(key: { name: string; sequence?: string; shift?: boolean }): boolean {
  return key.name === "?" || key.sequence === "?" || (!!key.shift && key.name === "/")
}

function buildShortcutSections(params: {
  screen: Screen
  activeHomePane: HomePane
  fullscreenHomePane: HomePane | null
  activeEnvironmentPane: EnvironmentPane
  fullscreenEnvironmentPane: EnvironmentPane | null
  pendingActionMenu: PendingActionMenu | null
  pendingConfirmation: PendingActionConfirmation | null
  markedWorkspaceCount: number
}): ShortcutSection[] {
  const global: ShortcutSection = {
    title: "Global",
    items: [
      { keys: "?", description: "Show or hide this help" },
      { keys: "q", description: "Quit the TUI" },
      { keys: "esc", description: params.screen.kind === "environment" ? "Go back to the browser view" : "Close the current overlay" },
    ],
  }

  if (params.pendingConfirmation) {
    return [
      global,
      {
        title: `Confirm ${params.pendingConfirmation.action}`,
        items: [
          { keys: "enter / y", description: `Queue ${params.pendingConfirmation.action} for ${params.pendingConfirmation.workspaces.length} marked workspaces` },
          { keys: "esc / n / q", description: "Cancel without queueing" },
        ],
      },
    ]
  }

  if (params.pendingActionMenu) {
    return [
      global,
      {
        title: "Action menu",
        items: [
          { keys: "p / enter", description: `Open plan confirmation for ${params.pendingActionMenu.workspaces.length} marked workspaces` },
          { keys: "a", description: `Open apply confirmation for ${params.pendingActionMenu.workspaces.length} marked workspaces` },
          { keys: "esc", description: "Close the action menu" },
        ],
      },
    ]
  }

  if (params.screen.kind === "dashboard") {
    const browserSectionByPane: Record<typeof params.activeHomePane, ShortcutSection> = {
      orgs: {
        title: "Organizations (focused)",
        items: [
          { keys: "j / k", description: "Move between organizations" },
          { keys: "enter", description: "Browse the selected organization" },
          { keys: "tab", description: "Move focus to named environments" },
          { keys: "f", description: params.fullscreenHomePane === "orgs" ? "Exit fullscreen" : "Fullscreen the organizations pane" },
          { keys: "n / p", description: "Jump focus to named environments or PR environments" },
        ],
      },
      named: {
        title: "Named environments (focused)",
        items: [
          { keys: "j / k", description: "Move between named environments" },
          { keys: "enter", description: "Open the selected named environment" },
          { keys: "tab", description: "Move focus to PR environments" },
          { keys: "f", description: params.fullscreenHomePane === "named" ? "Exit fullscreen" : "Fullscreen the named environments pane" },
          { keys: "n / p", description: "Jump focus to named environments or PR environments" },
        ],
      },
      transient: {
        title: "PR environments (focused)",
        items: [
          { keys: "j / k", description: "Move between PR environments" },
          { keys: "enter", description: "Open the selected PR environment" },
          { keys: "tab", description: "Move focus to organizations" },
          { keys: "f", description: params.fullscreenHomePane === "transient" ? "Exit fullscreen" : "Fullscreen the PR environments pane" },
          { keys: "n / p", description: "Jump focus to named environments or PR environments" },
        ],
      },
    }

    return [
      global,
      browserSectionByPane[params.activeHomePane],
    ]
  }

  const graphSection: ShortcutSection = {
    title: "Workspace DAG (focused)",
    items: [
      { keys: "tab", description: "Switch focus between graph and details panes" },
      { keys: "h / j / k / l", description: "Move selection across the DAG" },
      { keys: "space", description: params.markedWorkspaceCount > 0 ? `Toggle marked state (currently ${params.markedWorkspaceCount} marked)` : "Mark or unmark the focused workspace" },
      { keys: "+ / -", description: "Zoom the DAG in or out" },
      { keys: "f", description: params.fullscreenEnvironmentPane === "graph" ? "Exit fullscreen" : "Fullscreen the DAG pane" },
      { keys: "enter", description: "Open the action menu for marked workspaces" },
    ],
  }

  const detailsSection: ShortcutSection = {
    title: "Details pane (focused)",
    items: [
      { keys: "tab", description: "Switch focus back to the graph pane" },
      { keys: "h / l", description: "Move between overview, logs, and outputs tabs" },
      { keys: "j / k", description: "Scroll overflowing content (next)" },
      { keys: "g / G", description: "Jump to the top or bottom of the detail content" },
      { keys: "f", description: params.fullscreenEnvironmentPane === "details" ? "Exit fullscreen" : "Fullscreen the details pane" },
      { keys: "c", description: "Cancel the selected running run" },
    ],
  }

  const selectionSection: ShortcutSection = {
    title: "Selection and actions",
    items: [
      { keys: "space -> enter", description: "Mark one or more workspaces, then choose an action" },
      { keys: "r", description: "Open plan confirmation for marked workspaces" },
      { keys: "a", description: "Open apply confirmation for marked workspaces" },
      { keys: "b", description: "Return to the org/environment browser" },
    ],
  }

  return params.activeEnvironmentPane === "graph"
    ? [global, graphSection, selectionSection]
    : [global, detailsSection, selectionSection]
}

function buildFooterHint(params: {
  screen: Screen
  activeHomePane: HomePane
  fullscreenHomePane: HomePane | null
  activeEnvironmentPane: EnvironmentPane
  fullscreenEnvironmentPane: EnvironmentPane | null
  pendingActionMenu: PendingActionMenu | null
  pendingConfirmation: PendingActionConfirmation | null
  markedWorkspaceCount: number
  toast: string | null
}): string {
  let hint: string

  if (params.pendingConfirmation) {
    hint = `confirm ${params.pendingConfirmation.action}: enter/y submit • esc cancel • ? help`
  } else if (params.pendingActionMenu) {
    hint = "action menu: p/enter plan • a apply • esc cancel • ? help"
  } else if (params.screen.kind === "dashboard") {
    const paneLabel = params.fullscreenHomePane ? `${params.activeHomePane} fullscreen` : params.activeHomePane
    hint = `${paneLabel}: j/k move • enter open • tab switch pane • f fullscreen • ? help • q quit`
  } else if (params.activeEnvironmentPane === "graph") {
    hint = `graph${params.fullscreenEnvironmentPane === "graph" ? " fullscreen" : ""}: h/j/k/l move • space mark • enter action • +/- zoom • f fullscreen • ? help`
  } else {
    hint = `details${params.fullscreenEnvironmentPane === "details" ? " fullscreen" : ""}: j/k scroll • h/l tabs • g/G jump • c cancel • f fullscreen • ? help`
  }

  return params.toast ? `${hint} • ${params.toast}` : hint
}

function clamp(value: number, min: number, max: number): number {
  return Math.min(max, Math.max(min, value))
}

function truncateMiddle(value: string, maxLength: number): string {
  if (value.length <= maxLength) {
    return value
  }

  if (maxLength <= 3) {
    return value.slice(0, maxLength)
  }

  const left = Math.ceil((maxLength - 3) / 2)
  const right = Math.floor((maxLength - 3) / 2)
  return `${value.slice(0, left)}...${value.slice(value.length - right)}`
}

function statusSymbol(status: string): string {
  switch (status) {
    case "ready":
    case "success":
    case "planned":
    case "destroyed":
      return "✓"
    case "failed":
    case "cancelled":
      return "✗"
    case "awaiting_approval":
      return "?"
    case "planning":
    case "applying":
    case "destroying":
    case "running":
      return "…"
    case "pending":
    case "queued":
      return "~"
    default:
      return "•"
  }
}

function statusColor(status: string): string {
  switch (status) {
    case "pending":
    case "queued":
      return THEME.statusPending
    case "planning":
    case "awaiting_approval":
      return THEME.statusPlanning
    case "applying":
    case "running":
    case "destroying":
      return THEME.statusApplying
    case "ready":
    case "success":
    case "planned":
    case "awaiting_apply":
      return THEME.statusReady
    case "failed":
    case "cancelled":
    case "system_error":
      return THEME.statusFailed
    case "destroyed":
    case "skipped":
      return THEME.statusDestroyed
    default:
      return THEME.textDim
  }
}

function formatStatus(status: string): string {
  return status.replace(/_/g, " ")
}

function capitalize(value: string): string {
  return value.length === 0 ? value : `${value[0].toUpperCase()}${value.slice(1)}`
}

function shortSha(value: string): string {
  return value ? value.slice(0, 7) : "unknown"
}

function formatRelativeTime(value: string): string {
  const date = new Date(value)
  const diffMs = Date.now() - date.getTime()

  if (!Number.isFinite(diffMs)) {
    return value
  }

  const diffMinutes = Math.floor(diffMs / 60_000)
  if (diffMinutes < 1) return "just now"
  if (diffMinutes < 60) return `${diffMinutes}m ago`

  const diffHours = Math.floor(diffMinutes / 60)
  if (diffHours < 24) return `${diffHours}h ago`

  const diffDays = Math.floor(diffHours / 24)
  return `${diffDays}d ago`
}

function getDirectDependencies(graph: DependencyGraph | null, workspacePath: string): string[] {
  if (!graph) {
    return []
  }

  return graph.edges
    .filter(([source]) => source === workspacePath)
    .map(([, target]) => target)
}

function getDirectDependents(graph: DependencyGraph | null, workspacePath: string): string[] {
  if (!graph) {
    return []
  }

  return graph.edges
    .filter(([, target]) => target === workspacePath)
    .map(([source]) => source)
}
