import { render, screen } from '@testing-library/react'
import { describe, expect, it } from 'vitest'
import { JsonPanel, Notice, StatusPill } from '../../src/shared/components'

describe('shared dashboard components', () => {
  it('renders status semantics and structured evidence', () => {
    const { container } = render(<><StatusPill value="pending_settlement" /><Notice tone="warning">review</Notice><JsonPanel value={{ id: 'intent-1' }} /></>)

    expect(container.querySelector('.status.pending')).toBeTruthy()
    expect(screen.getByText('pending settlement')).toBeTruthy()
    expect(screen.getByText(/intent-1/)).toBeTruthy()
  })
})
