/**
 * Authentication providers for Yaffle client.
 */

import type { Credentials, DeviceCodeResponse } from "./types.js"

export interface AuthProvider {
  getCredentials(): Promise<Credentials>
  isAuthenticated(): Promise<boolean>
}

export class TokenAuth implements AuthProvider {
  constructor(private token: string) {}

  async getCredentials(): Promise<Credentials> {
    return { accessToken: this.token }
  }

  async isAuthenticated(): Promise<boolean> {
    return this.token.length > 0
  }
}

export class DeviceFlowAuth implements AuthProvider {
  private credentials: Credentials | null = null

  constructor(
    private apiUrl: string,
    private clientId: string,
  ) {}

  async initiate(): Promise<DeviceCodeResponse> {
    const response = await fetch(`${this.apiUrl}/api/auth/device/code`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ client_id: this.clientId }),
    })

    if (!response.ok) {
      const text = await response.text()
      throw new Error(`Failed to initiate device flow: ${response.status} ${text}`)
    }

    return response.json()
  }

  async poll(deviceCode: string): Promise<Credentials | "pending" | "expired"> {
    const response = await fetch(`${this.apiUrl}/api/auth/device/token`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        client_id: this.clientId,
        device_code: deviceCode,
        grant_type: "urn:ietf:params:oauth:grant-type:device_code",
      }),
    })

    if (response.status === 400) {
      const data = await response.json()
      if (data.error === "authorization_pending") {
        return "pending"
      }
      if (data.error === "expired_token") {
        return "expired"
      }
      throw new Error(`Device flow error: ${data.error}`)
    }

    if (!response.ok) {
      const text = await response.text()
      throw new Error(`Failed to poll device token: ${response.status} ${text}`)
    }

    const data = await response.json()
    this.credentials = {
      accessToken: data.access_token,
      refreshToken: data.refresh_token,
      expiresAt: data.expires_in ? Date.now() + data.expires_in * 1000 : undefined,
    }
    return this.credentials
  }

  setCredentials(credentials: Credentials): void {
    this.credentials = credentials
  }

  async getCredentials(): Promise<Credentials> {
    if (!this.credentials) {
      throw new Error("Not authenticated. Run 'yaffle login' first.")
    }

    if (this.credentials.expiresAt && this.credentials.refreshToken) {
      const expiresIn = this.credentials.expiresAt - Date.now()
      if (expiresIn < 60000) {
        await this.refresh()
      }
    }

    return this.credentials
  }

  async isAuthenticated(): Promise<boolean> {
    return this.credentials !== null
  }

  private async refresh(): Promise<void> {
    if (!this.credentials?.refreshToken) {
      throw new Error("No refresh token available")
    }

    const response = await fetch(`${this.apiUrl}/api/auth/token`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        grant_type: "refresh_token",
        refresh_token: this.credentials.refreshToken,
        client_id: this.clientId,
      }),
    })

    if (!response.ok) {
      this.credentials = null
      throw new Error("Token refresh failed. Run 'yaffle login' again.")
    }

    const data = await response.json()
    this.credentials = {
      accessToken: data.access_token,
      refreshToken: data.refresh_token || this.credentials.refreshToken,
      expiresAt: data.expires_in ? Date.now() + data.expires_in * 1000 : undefined,
    }
  }
}

export class GitHubOIDCAuth implements AuthProvider {
  private credentials: Credentials | null = null

  constructor(private apiUrl: string) {}

  async getCredentials(): Promise<Credentials> {
    if (this.credentials && this.credentials.expiresAt && this.credentials.expiresAt > Date.now()) {
      return this.credentials
    }

    const oidcToken = await this.getGitHubOIDCToken()

    const response = await fetch(`${this.apiUrl}/api/auth/oidc/github`, {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
        Authorization: `Bearer ${oidcToken}`,
      },
    })

    if (!response.ok) {
      const text = await response.text()
      throw new Error(`OIDC exchange failed: ${response.status} ${text}`)
    }

    const data = await response.json()
    this.credentials = {
      accessToken: data.access_token,
      expiresAt: data.expires_in ? Date.now() + data.expires_in * 1000 : undefined,
    }
    return this.credentials
  }

  async isAuthenticated(): Promise<boolean> {
    return !!process.env.ACTIONS_ID_TOKEN_REQUEST_URL
  }

  private async getGitHubOIDCToken(): Promise<string> {
    const requestUrl = process.env.ACTIONS_ID_TOKEN_REQUEST_URL
    const requestToken = process.env.ACTIONS_ID_TOKEN_REQUEST_TOKEN

    if (!requestUrl || !requestToken) {
      throw new Error("GitHub OIDC not available. Ensure 'id-token: write' permission is set.")
    }

    const response = await fetch(`${requestUrl}&audience=yaffle`, {
      headers: {
        Authorization: `Bearer ${requestToken}`,
        Accept: "application/json",
      },
    })

    if (!response.ok) {
      throw new Error(`Failed to get GitHub OIDC token: ${response.status}`)
    }

    const data = await response.json()
    return data.value
  }
}
