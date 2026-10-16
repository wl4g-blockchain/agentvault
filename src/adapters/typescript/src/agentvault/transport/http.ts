import { inspect } from 'node:util';

export async function readBounded(response: Response, maximum: number): Promise<Uint8Array> {
  const declared = response.headers.get('content-length');
  if (declared !== null && Number(declared) > maximum) throw new Error('response exceeds configured limit');
  if (!response.body) return new Uint8Array();
  const reader = response.body.getReader();
  const chunks: Uint8Array[] = [];
  let length = 0;
  try {
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      length += value.length;
      if (length > maximum) {
        await reader.cancel();
        throw new Error('response exceeds configured limit');
      }
      chunks.push(value);
    }
  } finally {
    reader.releaseLock();
  }
  const output = new Uint8Array(length);
  let offset = 0;
  for (const chunk of chunks) {
    output.set(chunk, offset);
    offset += chunk.length;
  }
  return output;
}

export class ApiError extends Error {
  constructor(public readonly status: number, public readonly code: string, message: string) {
    super(`AgentVault API error ${status} (${code}): ${message}`);
  }
}

export interface VaultTransport {
  request<T>(method: 'GET' | 'POST', path: string, body?: unknown, idempotencyKey?: string): Promise<T>;
}

export class HttpVaultTransport implements VaultTransport {
  private readonly base: URL;

  constructor(
    baseUrl: string,
    private readonly authguardBearer?: string,
    private readonly timeoutMs = 30_000,
    private readonly maxResponseBytes = 2 * 1024 * 1024,
  ) {
    this.base = new URL(baseUrl);
    if (!['http:', 'https:'].includes(this.base.protocol) || this.base.username || this.base.password ||
        this.base.search || this.base.hash || timeoutMs <= 0 || maxResponseBytes <= 0 ||
        (authguardBearer !== undefined &&
          (!authguardBearer.trim() || /[\x00-\x1f\x7f]/.test(authguardBearer)))) {
      throw new Error('unsafe AgentVault transport configuration');
    }
  }

  [inspect.custom](): string {
    return `HttpVaultTransport { baseUrl: ${JSON.stringify(this.base.origin)}, authguardBearer: ${this.authguardBearer ? '<redacted>' : 'disabled'} }`;
  }

  async request<T>(method: 'GET' | 'POST', path: string, body?: unknown, idempotencyKey?: string): Promise<T> {
    if (!path.startsWith('/') || path.startsWith('//') || path.includes('\\')) {
      throw new Error('path must be origin-relative');
    }
    const target = new URL(path, this.base.origin);
    if (target.origin !== this.base.origin) throw new Error('path changed AgentVault authority');
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), this.timeoutMs);
    try {
      const headers: Record<string, string> = { accept: 'application/json' };
      if (this.authguardBearer) headers.authorization = `Bearer ${this.authguardBearer}`;
      let encoded: string | undefined;
      if (body !== undefined) {
        headers['content-type'] = 'application/json';
        encoded = JSON.stringify(body);
      }
      if (idempotencyKey) headers['idempotency-key'] = idempotencyKey;
      const init: RequestInit = { method, headers, redirect: 'manual', signal: controller.signal };
      if (encoded !== undefined) init.body = encoded;
      const response = await fetch(target, init);
      const raw = await readBounded(response, this.maxResponseBytes);
      let value: unknown;
      try {
        value = JSON.parse(new TextDecoder().decode(raw));
      } catch {
        value = undefined;
      }
      if (!response.ok) {
        const detail = (value as { error?: { code?: string; message?: string } } | undefined)?.error;
        throw new ApiError(
          response.status,
          detail?.code ?? 'HTTP_ERROR',
          detail?.message ?? `AgentVault returned HTTP ${response.status}`,
        );
      }
      return value as T;
    } finally {
      clearTimeout(timer);
    }
  }
}
