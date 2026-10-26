import type { MessageSendParams, SendMessageRequest, Task } from '@a2a-js/sdk';

/** Original carrier plus upstream A2A types; unknown application metadata is preserved. */
export class A2ACall {
  constructor(readonly envelope: SendMessageRequest & { _meta?: Record<string, unknown> }) {}

  static parse(body: Uint8Array, limit: number): A2ACall | undefined {
    if (body.byteLength > limit) throw new Error('A2A request exceeds configured limit');
    let raw: unknown;
    try { raw = JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(body)); }
    catch { return undefined; }
    const value = A2ACall.optionalObject(raw);
    if (!value || value.jsonrpc !== '2.0') return undefined;
    if (['message/stream', 'SendStreamingMessage', 'SendMessage'].includes(String(value.method))) {
      throw new Error('paid A2A supports non-streaming v0.3 message/send');
    }
    if (value.method !== 'message/send') return undefined;
    if (typeof value.id !== 'string' && !(typeof value.id === 'number' && Number.isFinite(value.id))) {
      throw new Error('paid A2A requires a JSON-RPC request id');
    }
    const call = new A2ACall(value as unknown as SendMessageRequest);
    call.params();
    return call;
  }

  get wire(): Record<string, unknown> { return this.envelope as unknown as Record<string, unknown>; }

  params(): MessageSendParams {
    const params = A2ACall.object(this.envelope.params);
    const message = A2ACall.object(params.message);
    if (message.kind !== 'message' || message.role !== 'user' || typeof message.messageId !== 'string' ||
        !message.messageId || !Array.isArray(message.parts) || !message.parts.length) {
      throw new Error('invalid A2A MessageSendParams');
    }
    return this.envelope.params;
  }

  bindTask(task: Task): void {
    const message = this.params().message;
    if (!task.id || !task.contextId || (message.taskId && message.taskId !== task.id) ||
        (message.contextId && message.contextId !== task.contextId)) {
      throw new Error('A2A task/context does not match the request');
    }
    message.taskId = task.id;
    message.contextId = task.contextId;
  }

  body(): string { return A2ACall.canonical(this.envelope); }

  static metadata(message: Record<string, unknown>, root = false): Record<string, unknown> {
    const owner = root ? message : A2ACall.object(A2ACall.object(message.params).message);
    const key = root ? '_meta' : 'metadata';
    owner[key] ??= {};
    return A2ACall.object(owner[key]);
  }

  static task(message: Record<string, unknown>): Task {
    const task = A2ACall.object(message.result);
    const status = A2ACall.object(task.status);
    if (task.kind !== 'task' || typeof task.id !== 'string' || !task.id ||
        typeof task.contextId !== 'string' || !task.contextId ||
        !['submitted', 'working', 'input-required', 'auth-required', 'completed', 'failed', 'canceled', 'rejected', 'unknown'].includes(String(status.state))) {
      throw new Error('invalid A2A Task');
    }
    return task as unknown as Task;
  }

  static response(message: unknown, id: string | number): Record<string, unknown> {
    const value = A2ACall.object(message);
    if (value.jsonrpc !== '2.0' || value.id !== id || (('result' in value) === ('error' in value))) {
      throw new Error('A2A response does not match the request');
    }
    return value;
  }

  static object(value: unknown): Record<string, unknown> {
    const object = A2ACall.optionalObject(value);
    if (!object) throw new Error('A2A wire value must be an object');
    return object;
  }

  static optionalObject(value: unknown): Record<string, unknown> | undefined {
    return value !== null && typeof value === 'object' && !Array.isArray(value)
      ? value as Record<string, unknown> : undefined;
  }

  static canonical(value: unknown): string {
    if (Array.isArray(value)) return `[${value.map(A2ACall.canonical).join(',')}]`;
    const object = A2ACall.optionalObject(value);
    if (object) return `{${Object.keys(object).sort().map((key) => `${JSON.stringify(key)}:${A2ACall.canonical(object[key])}`).join(',')}}`;
    const encoded = JSON.stringify(value);
    if (encoded === undefined) throw new Error('A2A message contains a non-JSON value');
    return encoded;
  }
}
