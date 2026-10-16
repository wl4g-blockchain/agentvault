import { describe, expect, it } from 'vitest'
import type { AssetIdentity, PaymentIntent } from '../../src/api/types'

describe('dashboard API types', () => {
  it('requires an exact asset identity on a payment intent', () => {
    const asset: AssetIdentity = { network: 'eip155:31337', asset: 'USDC', decimals: 6 }
    const intent: Pick<PaymentIntent, 'asset' | 'direction' | 'status'> = {
      asset,
      direction: 'outbound',
      status: 'authorized',
    }

    expect(intent).toEqual({ asset, direction: 'outbound', status: 'authorized' })
  })
})
