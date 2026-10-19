import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { managementApi } from '../../src/api/client'
import type { ScenarioConfiguration } from '../../src/api/types'
import { ScenariosPage } from '../../src/pages/ScenariosPage'

afterEach(() => {
  cleanup()
  vi.restoreAllMocks()
})

describe('ScenariosPage', () => {
  it('requires and submits a multi-wallet initial Policy', async () => {
    vi.spyOn(managementApi, 'list').mockImplementation(async resource => {
      if (resource === 'clients') return { items: [{ id: 'client-1', name: 'Client', auth_principal: 'p', enabled: true, created_at: 1, updated_at: 1 }], total: 1 }
      if (resource === 'wallets') return { items: Array.from({ length: 12 }, (_, index) => ({
        id: `wallet-${String(index + 1).padStart(2, '0')}`,
        algorithm: 'secp256k1', address: `0x${index + 1}`, public_key: `key-${index + 1}`, created_at: 1,
      })), total: 12 }
      return { items: [], total: 0 }
    })
    const response = {
      scenario: { id: 'scenario-1', client_id: 'client-1', name: 'Scenario', enabled: true, created_at: 1, updated_at: 1 },
      policy: {
        id: 'policy-1', scenario_id: 'scenario-1', wallet_ids: ['wallet-01', 'wallet-02'], protocols: ['x402'], networks: ['eip155:31337'],
        assets: [{ network: 'eip155:31337', asset: 'USDC', decimals: 6 }], budget_unit: { asset: 'USDC', decimals: 6 },
        allowed_recipients: ['0x3'], allowed_origins: ['https://market.example'], outbound_limits: { max_per_payment: '1', max_per_day: '10', approval_enabled: true },
        enabled: true, created_at: 1, updated_at: 1,
      },
    } satisfies ScenarioConfiguration
    const create = vi.spyOn(managementApi, 'createScenario').mockResolvedValue(response)

    render(<ScenariosPage />)
    await waitFor(() => expect(screen.getByLabelText('Client')).toBeTruthy())
    fireEvent.change(screen.getByLabelText('Scenario ID'), { target: { value: 'scenario-1' } })
    fireEvent.change(screen.getByLabelText('Display name'), { target: { value: 'Scenario' } })
    fireEvent.change(screen.getByLabelText('Initial Policy ID'), { target: { value: 'policy-1' } })
    fireEvent.change(screen.getByLabelText('Allowed recipients'), { target: { value: '0x3' } })
    fireEvent.change(screen.getByLabelText('Allowed origins'), { target: { value: 'https://market.example' } })
    const selector = screen.getByTestId('wallet-selector')
    fireEvent.click(within(selector).getByText(/Wallet pool/))
    expect(selector.hasAttribute('open')).toBe(true)
    const walletChoices = within(selector).getAllByRole('checkbox')
    expect(walletChoices).toHaveLength(12)
    walletChoices.slice(0, 2).forEach(box => fireEvent.click(box))
    expect(within(selector).getByText('2 selected')).toBeTruthy()
    fireEvent.click(screen.getByRole('button', { name: 'Create atomically' }))

    await waitFor(() => expect(create).toHaveBeenCalled())
    expect(create.mock.calls[0][0].policy.wallet_ids).toEqual(['wallet-01', 'wallet-02'])
    expect(await screen.findByTestId('scenario-created')).toBeTruthy()
  })

  it('lists a client’s scenarios in ten-row pages', async () => {
    const scenarios = Array.from({ length: 12 }, (_, index) => ({
      id: `scenario-${index}`,
      client_id: 'client-1',
      name: `Scenario ${index}`,
      enabled: true,
      created_at: 1,
      updated_at: 1,
    }))
    vi.spyOn(managementApi, 'list').mockImplementation(async (resource, _parent, offset = 0, limit = 10) => {
      if (resource === 'clients') return { items: [{ id: 'client-1', name: 'Client', auth_principal: 'p', enabled: true, created_at: 1, updated_at: 1 }], total: 1 }
      if (resource === 'wallets') return { items: [], total: 0 }
      return { items: scenarios.slice(offset, offset + limit), total: scenarios.length }
    })

    render(<ScenariosPage />)
    await waitFor(() => expect(screen.getByText('Scenario 0')).toBeTruthy())
    expect(screen.getByRole('navigation', { name: 'Client scenarios pagination' }).textContent).toContain('1–10 of 12')
    expect(screen.queryByText('Scenario 10')).toBeNull()

    fireEvent.click(screen.getByRole('button', { name: 'Next' }))
    await waitFor(() => expect(screen.getByText('Scenario 10')).toBeTruthy())
    expect(screen.queryByText('Scenario 0')).toBeNull()
  })
})
