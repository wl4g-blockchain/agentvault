import { randomUUID } from 'node:crypto';
import type { AgentCard, Task } from '@a2a-js/sdk';
import type { TaskStore, ServerCallContext } from '@a2a-js/sdk/server';
import { A2ACall } from '../model/a2a.js';
import type { PayeeMiddleware } from './payee.js';
import type { PaymentChallenge } from '../model/base.js';
import type { PayeeOperation, SettledIncomingPayment } from '../model/payee.js';
import type { ProtocolRegistry, McpPaymentRequired } from '../protocol/adapter.js';

export class A2APaymentCall {
  constructor(readonly call: A2ACall, readonly task: Task, readonly store: TaskStore,
    readonly credential: unknown, readonly context?: ServerCallContext) {}
  get id(): string | number { return this.call.envelope.id; }
  get canonicalBody(): Buffer { return Buffer.from(this.call.body()); }
}

/** TaskStore adapter; financial authority remains in the payee orchestration core. */
export class A2APayeeMiddleware {
  private static readonly scope = ['agentvault.payment.run_id', 'agentvault.payment.scenario_id', 'agentvault.payment.offer_id'] as const;
  constructor(private readonly core: PayeeMiddleware, private readonly protocols: ProtocolRegistry) {}

  agentCard(card: AgentCard): AgentCard {
    const extensions = card.capabilities.extensions ??= [];
    for (const uri of this.protocols.a2aExtensions().split(',').map((value) => value.trim()).filter(Boolean)) {
      if (!extensions.some((extension) => extension.uri === uri)) extensions.push({uri, required: true});
    }
    return card;
  }

  async prepare(body: Uint8Array, operation: PayeeOperation, store: TaskStore, headers: Headers, context?: ServerCallContext): Promise<A2APaymentCall | undefined> {
    const call = A2ACall.parse(body, this.core.config.maxBodyBytes);
    if (!call) return undefined;
    if (!operation.runId || !store) throw new Error('A2A requires run_id and the application TaskStore');
    const adapter = this.protocols.a2a(operation.protocol);
    if (adapter.a2aExtension() && !headers.get('x-a2a-extensions')?.split(',').map((value) => value.trim()).includes(adapter.a2aExtension())) throw new Error('required A2A extension is not activated');
    const [selected, credential] = this.protocols.a2aCredential(call.wire, this.core.config.maxBodyBytes);
    if (selected && selected !== adapter.name) throw new Error('A2A credential protocol mismatch');
    this.protocols.a2aStripCredentials(call.wire);
    const params = call.params(), taskId = params.message.taskId ?? operation.runId;
    let task = await store.load(taskId, context);
    if (!task) {
      if (credential !== undefined || params.message.taskId) throw new Error('A2A task must exist before credential delivery or continuation');
      task = {kind: 'task', id: taskId, contextId: params.message.contextId ?? randomUUID(), status: {state: 'submitted'}, history: [params.message]};
    }
    task = structuredClone(task);
    task.metadata ??= {};
    const values = [operation.runId, operation.scenarioId, operation.offerId];
    for (const [index, key] of A2APayeeMiddleware.scope.entries()) {
      if ((key in task.metadata && task.metadata[key] !== values[index]) || (!(key in task.metadata) && credential !== undefined)) throw new Error('A2A task belongs to another payment scope');
      task.metadata[key] = values[index];
    }
    call.bindTask(task);
    await store.save(task, context);
    return new A2APaymentCall(call, task, store, credential, context);
  }

  extension(protocol: string): string { return this.protocols.a2a(protocol).a2aExtension(); }

  async challenge(call: A2APaymentCall, challenge: PaymentChallenge, rejected: boolean): Promise<McpPaymentRequired> {
    const wire = this.protocols.a2a(challenge.protocol).a2aPaymentRequired(challenge, call.task, rejected);
    if ('result' in wire) await this.persist(call, A2ACall.task({result: wire.result}));
    return wire;
  }

  async receipt(call: A2APaymentCall, raw: unknown, challenge: PaymentChallenge, payment: SettledIncomingPayment): Promise<Record<string, unknown>> {
    const message = A2ACall.response(raw, call.id);
    if (this.protocols.a2aEvidence(message, this.core.config.maxBodyBytes)[1]) throw new Error('handler attempted to supply payment evidence');
    if ('error' in message) {
      const task = structuredClone(call.task);
      task.status = {state:'failed', message: {kind:'message', role:'agent', messageId:randomUUID(), taskId:task.id, contextId:task.contextId,
        parts:[{kind:'data', data:{jsonrpc_error:message.error}}]}};
      delete message.error;
      message.result = task;
    }
    call.call.bindTask(A2ACall.task(message));
    this.protocols.a2a(payment.receipt.protocol).a2aInjectReceipt(message, challenge, payment);
    await this.persist(call, A2ACall.task(message));
    return message;
  }

  private async persist(call: A2APaymentCall, task: Task): Promise<void> {
    if (task.id !== call.task.id || task.contextId !== call.task.contextId) throw new Error('A2A response changed task/context');
    task.metadata ??= {};
    for (const key of A2APayeeMiddleware.scope) {
      if (key in task.metadata && task.metadata[key] !== call.task.metadata?.[key]) throw new Error('A2A handler changed payment scope');
      task.metadata[key] = call.task.metadata?.[key];
    }
    await call.store.save(task, call.context);
  }
}
