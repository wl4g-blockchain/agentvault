import { render, screen } from '@testing-library/react'
import { describe, expect, it } from 'vitest'
import { ResourcePage } from '../../src/pages/ResourcePage'

describe('ResourcePage', () => {
  it('renders an ownership-scoped scenario browser', () => {
    render(<ResourcePage resource="scenarios" />)

    expect(screen.getByRole('heading', { name: 'Scenarios' })).toBeTruthy()
    expect(screen.getByLabelText('Client ID')).toBeTruthy()
    expect(screen.getByRole('button', { name: 'Load scenarios' })).toBeTruthy()
  })

  it('renders server-authoritative offer resolution', () => {
    render(<ResourcePage resource="offers" />)

    expect(screen.getByLabelText('Absolute resource URL')).toBeTruthy()
    expect(screen.getByRole('button', { name: 'Resolve fixed-price offer' })).toBeTruthy()
  })
})
