import { render, screen } from '@testing-library/react'
import { MemoryRouter } from 'react-router-dom'
import { describe, expect, it } from 'vitest'
import { App } from '../src/App'

describe('dashboard navigation', () => {
  it('exposes every payment operations area', () => {
    render(<MemoryRouter initialEntries={['/']}><App /></MemoryRouter>)
    const expected = [
      ['Overview', '/'],
      ['Clients', '/clients'],
      ['Scenarios', '/scenarios'],
      ['Policies', '/policies'],
      ['Wallets', '/wallets'],
      ['Offers', '/offers'],
      ['Payments', '/payments'],
      ['Approvals', '/approvals'],
      ['Ledger & reconcile', '/ledger'],
      ['Runtime health', '/runtime'],
    ]
    expected.forEach(([name, href]) => expect(screen.getByRole('link', { name }).getAttribute('href')).toBe(href))
  })

  it('exposes read-only management browsing without mutation controls', () => {
    render(<MemoryRouter initialEntries={['/clients']}><App /></MemoryRouter>)
    expect(screen.getByRole('heading', { name: 'Browse clients' })).toBeTruthy()
    expect(screen.getByRole('button', { name: 'Load clients' })).toBeTruthy()
    expect(screen.queryByRole('button', { name: 'New record' })).toBeNull()
  })
})
