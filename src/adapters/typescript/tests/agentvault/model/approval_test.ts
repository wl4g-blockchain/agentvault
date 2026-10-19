import assert from 'node:assert/strict';
import test from 'node:test';
import { validateApprovalDecision } from '../../../src/agentvault/model/approval.js';

test('approval decisions preserve one-time amount semantics', () => {
  assert.doesNotThrow(() => validateApprovalDecision({
    scenario_id: 'scenario', run_id: 'run',
    approval_id: 'approval', decision: 'approve', approved_amount: '100',
  }));
  assert.throws(() => validateApprovalDecision({
    scenario_id: 'scenario', run_id: 'run',
    approval_id: 'approval', decision: 'reject', approved_amount: '100',
  }));
});
