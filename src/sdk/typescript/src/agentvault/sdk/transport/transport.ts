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
  request<T>(method: 'GET' | 'POST', path: string, body?: unknown): Promise<T>;
}

export interface OwnedVaultTransport extends VaultTransport {
  close(): void | Promise<void>;
}
