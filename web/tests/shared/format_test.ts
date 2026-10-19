import { describe, expect, it } from 'vitest'
import { compactId, displayError, formatTime } from '../../src/shared/format'

describe('dashboard formatting', () => {
  it('compacts identifiers without losing both ends', () => {
    expect(compactId('123456789012345678901234567890')).toBe('123456789012…34567890')
    expect(compactId()).toBe('—')
  })

  it('uses bounded generic error text for non-errors', () => {
    expect(displayError(new Error('failed safely'))).toBe('failed safely')
    expect(displayError({ secret: 'hidden' })).toBe('AgentVault request failed')
    expect(formatTime()).toBe('—')
  })
})
