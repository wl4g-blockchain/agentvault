import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { managementApi } from '../../src/api/client'
import { ClientsPage } from '../../src/pages/ClientsPage'

afterEach(() => {
  cleanup()
  vi.restoreAllMocks()
})

describe('ClientsPage', () => {
  it('shows ten Clients per page and loads only the selected Client’s Scenarios', async () => {
    const clients = Array.from({ length: 12 }, (_, index) => ({
      id: `client-${index}`, name: `Client ${index}`, auth_principal: `principal-${index}`,
      enabled: true, created_at: 1, updated_at: 1,
    }))
    const list = vi.spyOn(managementApi, 'list').mockImplementation(async (resource, parentId, offset = 0, limit = 10) => {
      if (resource === 'clients') return { items: clients.slice(offset, offset + limit), total: clients.length }
      return { items: [{ id: `scenario-${parentId}`, client_id: parentId!, name: `${parentId} Scenario`, enabled: true, created_at: 1, updated_at: 1 }], total: 1 }
    })

    render(<ClientsPage />)
    await waitFor(() => expect(screen.getByText('Client 0')).toBeTruthy())
    expect(screen.getByRole('navigation', { name: 'Clients pagination' }).textContent).toContain('1–10 of 12')
    await waitFor(() => expect(screen.getByText('client-0 Scenario')).toBeTruthy())

    fireEvent.click(screen.getByRole('button', { name: /Client 1/ }))
    await waitFor(() => expect(screen.getByText('client-1 Scenario')).toBeTruthy())
    expect(list).toHaveBeenCalledWith('scenarios', 'client-1', 0, 10)
  })
})
