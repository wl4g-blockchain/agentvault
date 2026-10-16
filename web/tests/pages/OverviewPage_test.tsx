import { render, screen } from '@testing-library/react'
import { MemoryRouter } from 'react-router-dom'
import { describe, expect, it } from 'vitest'
import { OverviewPage } from '../../src/pages/OverviewPage'

describe('OverviewPage', () => {
  it('describes both payment directions and the authoritative SQL boundary', () => {
    render(<MemoryRouter><OverviewPage /></MemoryRouter>)

    expect(screen.getByText('OUTBOUND / PAYER')).toBeTruthy()
    expect(screen.getByText('INBOUND / PAYEE')).toBeTruthy()
    expect(screen.getByText('SQL + append-only ledger')).toBeTruthy()
  })
})
