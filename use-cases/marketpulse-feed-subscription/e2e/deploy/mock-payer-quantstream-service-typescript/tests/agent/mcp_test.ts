import assert from 'node:assert/strict';
import test from 'node:test';
import { positive, required, type ToolResult } from '../../src/agent/mcp.js';

test('MCP configuration values are explicit and positive', () => {
  process.env.AGENTVAULT_TEST_POSITIVE = '17';
  try {
    assert.equal(required('AGENTVAULT_TEST_POSITIVE'), '17');
    assert.equal(positive('AGENTVAULT_TEST_POSITIVE'), 17);
  } finally {
    delete process.env.AGENTVAULT_TEST_POSITIVE;
  }
});

test('tool results separate content from payment evidence', () => {
  const result: ToolResult = { body: new TextEncoder().encode('{"paid":true}'), paymentIntentId: 'intent-ts-1' };
  assert.equal(result.paymentIntentId, 'intent-ts-1');
});
