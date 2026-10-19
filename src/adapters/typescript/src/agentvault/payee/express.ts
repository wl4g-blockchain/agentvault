import { createHash } from 'node:crypto';
import express, { type RequestHandler } from 'express';
import { ApiError } from '../transport/transport.js';
import { InvalidPaymentError, type PayeeMiddleware } from './payee.js';

export interface ExpressPayeeRoute {
  method: string;
  path: string;
  scenarioId: string;
  offerId: string;
  resource: string;
  origin: string;
  protocol?: string;
  mcpTool?: string;
}

export function mcpToolRoute(
  path: string,
  tool: string,
  scenarioId: string,
  offerId: string,
  resource: string,
  origin: string,
): ExpressPayeeRoute {
  return { method: 'POST', path, scenarioId, offerId, resource, origin, protocol: 'x402', mcpTool: tool };
}

/** Express middleware that protects only explicitly registered routes. */
export class ExpressPayeeMiddleware {
  constructor(
    private readonly core: PayeeMiddleware,
    private readonly routes: readonly ExpressPayeeRoute[],
    private readonly maxBodyBytes = 1024 * 1024,
  ) {
    if (maxBodyBytes <= 0) throw new Error('maxBodyBytes must be positive');
  }

  handlers(): RequestHandler[] {
    return [express.raw({ type: '*/*', limit: this.maxBodyBytes }), this.handler()];
  }

  handler(): RequestHandler {
    return async (request, response, next) => {
      const route = this.routes.find((candidate) =>
        candidate.method.toUpperCase() === request.method && candidate.path === request.path);
      if (!route) {
        next();
        return;
      }
      try {
        if (!Buffer.isBuffer(request.body)) throw new Error('Express raw body middleware was not installed');
        if (route.mcpTool !== undefined && !isMcpToolCall(request.body, route.mcpTool)) {
          next();
          return;
        }
        const runId = textHeader(request.headers['x-run-id']);
        if (!runId) {
          response.status(400).json({
            error: {
              code: 'PAYMENT_CONTEXT_REQUIRED',
              message: 'x-run-id is required',
            },
          });
          return;
        }
        const requestHash = createHash('sha256')
          .update('agentvault.payee.http.v1\0')
          .update(request.method)
          .update('\0')
          .update(request.path)
          .update('\0')
          .update(request.body)
          .digest('hex');
        const signature = textHeader(request.headers['payment-signature']);
        const credentialPayload = signature === undefined
          ? undefined
          : decodeCredential(signature, this.maxBodyBytes);
        const outcome = await this.core.protect({
          scenarioId: route.scenarioId,
          runId,
          offerId: route.offerId,
          resource: route.resource,
          origin: route.origin,
          protocol: route.protocol ?? 'x402',
          requestHash,
          credentialPayload,
        });
        if (outcome.outcome === 'payment_required') {
          const challenge = outcome.challenge;
          const payload = challenge.payload;
          const required = payload && typeof payload === 'object'
            ? (payload as { payment_required?: unknown }).payment_required
            : undefined;
          if (required === undefined) throw new Error('AgentVault returned no protocol wire challenge');
          const body = Buffer.from(JSON.stringify(required));
          response.status(402)
            .type('application/json')
            .set('payment-protocol', challenge.protocol)
            .set('payment-expires-at', String(challenge.expires_at))
            .set('payment-required', body.toString('base64'))
            .send(body);
          return;
        }
        response.locals.agentvaultPayment = outcome.payment;
        const receipt = outcome.payment.receipt;
        if (!receipt.tx_hash || !receipt.payer) throw new Error('confirmed receipt omitted transaction or payer');
        const paymentResponse = Buffer.from(JSON.stringify({
          success: true,
          payer: receipt.payer,
          transaction: receipt.tx_hash,
          network: receipt.asset.network,
        })).toString('base64');
        response.setHeader('payment-response', paymentResponse);
        response.once('finish', () => console.info(JSON.stringify({
          event: 'agentvault.sdk.payee.resource_released',
          runId,
          intentId: outcome.payment.intent.id,
        })));
        next();
      } catch (error) {
        const status = error instanceof ApiError
          ? error.status
          : error instanceof InvalidPaymentError
            ? 402
            : error instanceof InvalidCredentialError
              ? 400
              : 502;
        const code = error instanceof ApiError
          ? error.code
          : error instanceof InvalidPaymentError || error instanceof InvalidCredentialError
            ? 'PAYMENT_INVALID'
            : 'AGENTVAULT_UNAVAILABLE';
        response.status(status).json({
          error: {
            code,
            message: error instanceof Error ? error.message : 'payment middleware failed',
          },
        });
      }
    };
  }
}

function isMcpToolCall(body: Buffer, expected: string): boolean {
  try {
    const value = JSON.parse(body.toString('utf8')) as { method?: unknown; params?: { name?: unknown } };
    return value.method === 'tools/call' && value.params?.name === expected;
  } catch {
    return false;
  }
}

function textHeader(value: string | string[] | undefined): string | undefined {
  const result = Array.isArray(value) ? value[0] : value;
  return result?.trim() ? result : undefined;
}

function decodeCredential(value: string, limit: number): unknown {
  if (value.length > Math.ceil(limit * 4 / 3) + 4 || !/^[A-Za-z0-9+/]*={0,2}$/.test(value)) {
    throw new InvalidCredentialError('PAYMENT-SIGNATURE is invalid or oversized');
  }
  const body = Buffer.from(value, 'base64');
  if (body.length > limit || body.toString('base64') !== value) {
    throw new InvalidCredentialError('PAYMENT-SIGNATURE is not canonical base64');
  }
  let payload: unknown;
  try {
    payload = JSON.parse(body.toString('utf8')) as unknown;
  } catch {
    throw new InvalidCredentialError('PAYMENT-SIGNATURE is not valid x402 JSON');
  }
  if (payload === null || typeof payload !== 'object' || Array.isArray(payload)) {
    throw new InvalidCredentialError('PAYMENT-SIGNATURE must be an x402 JSON object');
  }
  return payload;
}

class InvalidCredentialError extends Error {}
