import { render, screen } from '@testing-library/react'
import { describe, expect, it } from 'vitest'
import { PaymentsPage } from '../../src/pages/PaymentsPage'

describe('PaymentsPage', () => {
  it('scopes payment lookup to a scenario and intent', () => {
    render(<PaymentsPage />)

    expect(screen.getByLabelText('Scenario ID')).toBeTruthy()
    expect(screen.getByLabelText('Intent ID')).toBeTruthy()
    expect(screen.getByRole('button', { name: 'Inspect payment' })).toBeTruthy()
  })
})
