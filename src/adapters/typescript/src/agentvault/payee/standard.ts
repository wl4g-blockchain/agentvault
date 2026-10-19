import type { PayeeOperation, ProtectionOutcome } from '../model/payee.js';
import type { PayeeMiddleware } from './payee.js';

/** Framework-neutral entrypoint for custom TypeScript HTTP/MCP servers. */
export class StandardPayeeMiddleware {
  constructor(private readonly core: PayeeMiddleware) {}

  handle(operation: PayeeOperation): Promise<ProtectionOutcome> {
    return this.core.protect(operation);
  }
}
