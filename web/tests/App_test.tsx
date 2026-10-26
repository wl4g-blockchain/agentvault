import { render, screen } from '@testing-library/react'
import { MemoryRouter } from 'react-router-dom'
import { describe, expect, it } from 'vitest'
import { App } from '../src/App'

describe('dashboard navigation', () => {
  it('exposes every payment operations area', () => {
    render(<MemoryRouter initialEntries={['/']}><App /></MemoryRouter>)
    const expected = [
      ['Overview', '/'],
      ['Payer transactions', '/payer'],
      ['Payee transactions', '/payee'],
      ['Approvals', '/approvals'],
      ['Balances & ledger', '/balances'],
      ['Clients', '/clients'],
      ['Scenarios', '/scenarios'],
      ['Wallets', '/wallets'],
      ['Runtime health', '/runtime'],
    ]
    expected.forEach(([name, href]) => expect(screen.getByRole('link', { name }).getAttribute('href')).toBe(href))
  })

  it('places Scenario and multi-wallet Policy creation in one governance workflow', () => {
    render(<MemoryRouter initialEntries={['/scenarios']}><App /></MemoryRouter>)
    expect(screen.getByRole('heading', { name: 'New Scenario + initial Policy' })).toBeTruthy()
    expect(screen.getByTestId('wallet-selector')).toBeTruthy()
  })
})
