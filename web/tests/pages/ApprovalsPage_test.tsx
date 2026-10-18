import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { approvalApi } from '../../src/api/client'
import type { ApprovalRequest } from '../../src/api/types'
import { ApprovalsPage } from '../../src/pages/ApprovalsPage'

afterEach(() => {
  cleanup()
  vi.restoreAllMocks()
})

describe('ApprovalsPage', () => {
  it('requires both scenario and workflow-run context for decisions', () => {
    render(<ApprovalsPage />)

    expect(screen.getByLabelText('Scenario ID')).toBeTruthy()
    expect(screen.getByLabelText('Business run ID')).toBeTruthy()
    expect(screen.getByRole('button', { name: 'Load pending' })).toBeTruthy()
  })

  it('loads pending approvals in ten-row pages', async () => {
    const approvals = Array.from({ length: 12 }, (_, index): ApprovalRequest => ({
      id: `approval-${index}`,
      intent_id: `intent-${index}`,
      requested_amount: '1000000',
      requested_by: 'principal-1',
      status: 'pending',
      intent_hash: `hash-${index}`,
      expires_at: 100,
      created_at: 1,
    }))
    const list = vi.spyOn(approvalApi, 'listPending').mockImplementation(async (_scenario, offset = 0, limit = 10) => ({
      items: approvals.slice(offset, offset + limit),
      total: approvals.length,
    }))

    render(<ApprovalsPage />)
    fireEvent.change(screen.getByLabelText('Scenario ID'), { target: { value: 'scenario-1' } })
    fireEvent.change(screen.getByLabelText('Business run ID'), { target: { value: 'run-1' } })
    fireEvent.click(screen.getByRole('button', { name: 'Load pending' }))

    await waitFor(() => expect(screen.getByText('approval-0')).toBeTruthy())
    expect(screen.getByRole('navigation', { name: 'Pending approvals pagination' }).textContent).toContain('1–10 of 12')
    fireEvent.click(screen.getByRole('button', { name: 'Next' }))
    await waitFor(() => expect(screen.getByText('approval-10')).toBeTruthy())
    expect(list).toHaveBeenLastCalledWith('scenario-1', 10, 10)
  })
})
