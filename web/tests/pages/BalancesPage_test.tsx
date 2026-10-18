import { cleanup, render, screen, waitFor } from '@testing-library/react'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { managementApi } from '../../src/api/client'
import type { PaymentLedgerEntry } from '../../src/api/types'
import { BalancesPage } from '../../src/pages/BalancesPage'

afterEach(() => {
  cleanup()
  vi.restoreAllMocks()
})

describe('BalancesPage', () => {
  it('marks paired confirmed Payer and Payee facts as conserved', async () => {
    const entry = (direction: 'inbound' | 'outbound', id: string): PaymentLedgerEntry => ({
      id,
      intent_id: `intent-${id}`,
      scenario_id: `scenario-${direction}`,
      wallet_id: `wallet-${direction}`,
      entry_type: 'payment',
      direction,
      asset: { network: 'eip155:31337', asset: 'USDC', decimals: 6 },
      amount: '50000000',
      posting_key: `posting-${id}`,
      tx_hash: `0x${'1'.repeat(64)}`,
      effective_at: 2,
      created_at: 2,
    })
    const page: { items: PaymentLedgerEntry[]; total: number } = {
      items: [entry('outbound', 'payer'), entry('inbound', 'payee')],
      total: 2,
    }
    vi.spyOn(managementApi, 'listLedger').mockResolvedValue(page)
    vi.spyOn(managementApi, 'listAllLedger').mockResolvedValue({ ...page, complete: true })

    render(<BalancesPage />)
    await waitFor(() => expect(screen.getByTestId('balance-conservation').getAttribute('data-conserved')).toBe('true'))
    expect(screen.getByText('PASS')).toBeTruthy()
  })

  it('never claims conservation from a truncated ledger page', async () => {
    const page: { items: PaymentLedgerEntry[]; total: number } = {
      items: [{
        id: 'payer', intent_id: 'intent-payer', scenario_id: 'scenario-payer', wallet_id: 'wallet-payer',
        entry_type: 'payment', direction: 'outbound',
        asset: { network: 'eip155:31337', asset: 'USDC', decimals: 6 }, amount: '1',
        posting_key: 'posting-payer', effective_at: 2, created_at: 2,
      }, {
        id: 'payee', intent_id: 'intent-payee', scenario_id: 'scenario-payee', wallet_id: 'wallet-payee',
        entry_type: 'payment', direction: 'inbound',
        asset: { network: 'eip155:31337', asset: 'USDC', decimals: 6 }, amount: '1',
        posting_key: 'posting-payee', effective_at: 2, created_at: 2,
      }],
      total: 3,
    }
    vi.spyOn(managementApi, 'listLedger').mockResolvedValue(page)
    vi.spyOn(managementApi, 'listAllLedger').mockResolvedValue({ ...page, complete: false })

    render(<BalancesPage />)
    await waitFor(() => expect(screen.getByTestId('balance-conservation').getAttribute('data-conserved')).toBe('false'))
    expect(screen.getByText('REVIEW')).toBeTruthy()
    expect(screen.getByText('2 of 3 rows loaded')).toBeTruthy()
  })
})
