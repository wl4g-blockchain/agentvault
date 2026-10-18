import { fireEvent, render, screen, waitFor } from '@testing-library/react'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { managementApi } from '../../src/api/client'
import type { PaymentIntent } from '../../src/api/types'
import { TransactionsPage } from '../../src/pages/TransactionsPage'

afterEach(() => vi.restoreAllMocks())

describe('TransactionsPage', () => {
  it('renders Payer matrix records in ten-row pages', async () => {
    const items = Array.from({ length: 50 }, (_, index): PaymentIntent => ({
      id: `intent-${index}`,
      scenario_id: `payer-${index % 5}-payee-${index % 5}`,
      run_id: `run-${index}`,
      wallet_id: `wallet-${index % 10}`,
      direction: 'outbound',
      protocol: index < 25 ? 'mqtt' : 'grpc',
      asset: { network: 'eip155:31337', asset: 'USDC', decimals: 6 },
      recipient: '0x2222222222222222222222222222222222222222',
      amount: '1000000',
      settled_amount: '1000000',
      resource: `marketpulse.realtime_news / case ${index + 1}`,
      request_hash: `hash-${index}`,
      status: 'settled',
      reservation_status: 'consumed',
      reserved_amount: '0',
      created_at: 1,
      updated_at: 2,
    }))
    const list = vi.spyOn(managementApi, 'listPayments').mockImplementation(async (input = {}) => ({
      items: items.slice(input.offset ?? 0, (input.offset ?? 0) + (input.limit ?? 10)),
      total: items.length,
    }))

    const { rerender } = render(<TransactionsPage direction="outbound" />)
    await waitFor(() => expect(screen.getByTestId('outbound-transactions').getAttribute('data-total')).toBe('50'))
    expect(screen.getAllByText(/marketpulse\.realtime_news/)).toHaveLength(10)
    expect(screen.getByText('run-0')).toBeTruthy()
    expect(screen.getByRole('navigation', { name: 'Payer transactions pagination' }).textContent).toContain('Page 1 of 5')

    fireEvent.click(screen.getByRole('button', { name: 'Next' }))
    await waitFor(() => expect(screen.getByText('run-10')).toBeTruthy())
    expect(screen.queryByText('run-0')).toBeNull()
    expect(list).toHaveBeenLastCalledWith(expect.objectContaining({ offset: 10, limit: 10 }))

    rerender(<TransactionsPage direction="inbound" />)
    await waitFor(() => expect(screen.getByText('run-0')).toBeTruthy())
    expect(screen.getByRole('navigation', { name: 'Payee transactions pagination' }).textContent).toContain('Page 1 of 5')
  })
})
