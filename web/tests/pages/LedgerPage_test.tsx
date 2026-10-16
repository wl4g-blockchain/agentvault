import { render, screen } from '@testing-library/react'
import { describe, expect, it } from 'vitest'
import { LedgerPage } from '../../src/pages/LedgerPage'

describe('LedgerPage', () => {
  it('collects the complete asset identity used by ledger aggregation', () => {
    render(<LedgerPage />)

    expect(screen.getByLabelText('Network')).toBeTruthy()
    expect(screen.getByLabelText('Token contract')).toBeTruthy()
    expect(screen.getByLabelText('Decimals')).toBeTruthy()
    expect(screen.getByRole('button', { name: 'Not exposed' }).hasAttribute('disabled')).toBe(true)
  })
})
