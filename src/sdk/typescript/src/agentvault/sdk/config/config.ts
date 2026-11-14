export interface PayerConfig {
  scenarioId: string;
  approvalTimeoutMs?: number;
  approvalPollIntervalMs?: number;
  deliveryRetries?: number;
  maxChallengeBytes?: number;
  protocols?: readonly string[];
}

export interface PayeeConfig {
  maxBodyBytes?: number;
  protocols?: readonly string[];
}

export class HttpConfig {
  constructor(
    public readonly baseUrl: string,
    public readonly authguardBearer?: string,
    public readonly timeoutMs = 30_000,
    public readonly maxResponseBytes = 2 * 1024 * 1024,
  ) {
    const url = new URL(baseUrl);
    if (
      !["http:", "https:"].includes(url.protocol) ||
      url.username ||
      url.password ||
      url.search ||
      url.hash ||
      url.pathname !== "/" ||
      timeoutMs <= 0 ||
      maxResponseBytes <= 0 ||
      (authguardBearer !== undefined && /[\x00-\x1f\x7f]/.test(authguardBearer))
    )
      throw new Error("invalid AgentVault HTTP configuration");
  }
  [Symbol.for("nodejs.util.inspect.custom")](): string {
    return `HttpConfig { baseUrl: ${JSON.stringify(this.baseUrl)}, authguardBearer: <redacted> }`;
  }
}
export type VaultTransportConfig = { provider: "http"; http: HttpConfig };
