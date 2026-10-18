import { render, screen } from '@testing-library/react'
import { MemoryRouter, Route, Routes } from 'react-router-dom'
import { describe, expect, it } from 'vitest'
import { DashboardLayout } from '../../src/layout/DashboardLayout'

describe('DashboardLayout', () => {
  it('shows the resource and operations navigation inside the AuthGuard trust boundary', () => {
    render(<MemoryRouter><Routes><Route element={<DashboardLayout />}><Route index element={<p>home</p>} /></Route></Routes></MemoryRouter>)

    expect(screen.getByRole('navigation', { name: 'Primary navigation' })).toBeTruthy()
    expect(screen.getByText(/Identity is established upstream by AuthGuard/)).toBeTruthy()
    expect(screen.getByRole('link', { name: 'Payer transactions' })).toBeTruthy()
    expect(screen.getByRole('link', { name: 'Payee transactions' })).toBeTruthy()
    expect(screen.getByRole('link', { name: 'Balances & ledger' })).toBeTruthy()
    expect(screen.getByRole('link', { name: 'Clients' })).toBeTruthy()
  })
})
