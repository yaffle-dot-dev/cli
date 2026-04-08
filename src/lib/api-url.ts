export const DEFAULT_API_URL = "https://yaffle.dev"

interface ResolveApiUrlOptions {
  flagValue?: string
  envValue?: string
  configValue?: string
  defaultValue?: string
}

export function normalizeApiUrl(apiUrl: string): string {
  const trimmed = apiUrl.trim().replace(/\/+$/, "")

  if (!trimmed) {
    return trimmed
  }

  if (/^https?:\/\//i.test(trimmed)) {
    return trimmed
  }

  return `https://${trimmed}`
}

export function resolveApiUrl(options: ResolveApiUrlOptions): string {
  const apiUrl = options.flagValue
    || options.envValue
    || options.configValue
    || options.defaultValue
    || DEFAULT_API_URL

  return normalizeApiUrl(apiUrl)
}
