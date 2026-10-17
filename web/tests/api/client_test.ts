import { afterEach, describe, expect, it, vi } from 'vitest'
import { ApiError, approvalApi, managementApi, paymentApi, redactForDisplay } from '../../src/api/client'

afterEach(() => vi.unstubAllGlobals())

describe('same-origin AgentVault API client', () => {
  it('serializes a complete asset identity without adding credentials', async () => {
    const fetchMock = vi.fn().mockResolvedValue(new Response(JSON.stringify({ scenario_id: 's1' }), {
      status: 200,
      headers: { 'content-type': 'application/json' },
    }))
    vi.stubGlobal('fetch', fetchMock)

    await paymentApi.getBalance({
      scenario_id: 'tenant/scenario',
      wallet_id: 'wallet 1',
      network: 'eip155:31337',
      asset: 'USDC',
      token_contract: '0xabc',
      decimals: 6,
    })

    const [url, init] = fetchMock.mock.calls[0] as [string, RequestInit]
    const parsed = new URL(url, 'https://console.example')
    expect(parsed.pathname).toBe('/api/v1/payments/balance')
    expect(parsed.searchParams.get('scenario_id')).toBe('tenant/scenario')
    expect(parsed.searchParams.get('wallet_id')).toBe('wallet 1')
    expect(parsed.searchParams.get('decimals')).toBe('6')
    expect(init.credentials).toBe('same-origin')
    expect(new Headers(init.headers).has('authorization')).toBe(false)
  })

  it('serializes an approval decision with a stable run ID', async () => {
    const fetchMock = vi.fn().mockResolvedValue(new Response(JSON.stringify({ id: 'approval-7' }), {
      status: 200,
      headers: { 'content-type': 'application/json' },
    }))
    vi.stubGlobal('fetch', fetchMock)

    await approvalApi.decide({
      scenarioId: 'scenario-1',
      runId: 'run-9',
      approvalId: 'approval-7',
      decision: 'approve',
      approvedAmount: '50000000',
    })

    const [, init] = fetchMock.mock.calls[0] as [string, RequestInit]
    const body = JSON.parse(String(init.body)) as Record<string, unknown>
    expect(body).toMatchObject({
      scenario_id: 'scenario-1',
      run_id: 'run-9',
      approval_id: 'approval-7',
      decision: 'approve',
      approved_amount: '50000000',
    })
    expect(new Headers(init.headers).has('authorization')).toBe(false)
  })

  it('never includes an arbitrary server message in a thrown error', async () => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(JSON.stringify({
      code: 'FORBIDDEN',
      message: 'Bearer super-secret-value',
      grant_token: 'signed.jwt.value',
    }), { status: 403, headers: { 'content-type': 'application/json' } })))

    const error = await paymentApi.getPayment('scenario-1', 'intent-1').catch(caught => caught)
    expect(error).toBeInstanceOf(ApiError)
    expect(String(error)).toContain('FORBIDDEN')
    expect(String(error)).not.toContain('super-secret-value')
    expect(String(error)).not.toContain('signed.jwt.value')
  })

  it('scopes scenario management reads to an explicit client', async () => {
    const fetchMock = vi.fn().mockResolvedValue(new Response(JSON.stringify({ items: [], total: 0 }), {
      status: 200,
      headers: { 'content-type': 'application/json' },
    }))
    vi.stubGlobal('fetch', fetchMock)

    await managementApi.list('scenarios', 'client-1')

    const [url] = fetchMock.mock.calls[0] as [string, RequestInit]
    const parsed = new URL(url, 'https://console.example')
    expect(parsed.pathname).toBe('/api/v1/admin/scenarios')
    expect(parsed.searchParams.get('client_id')).toBe('client-1')
    expect(parsed.searchParams.get('limit')).toBe('100')
  })
})

describe('display redaction', () => {
  it('redacts nested payment credentials and grant material', () => {
    expect(redactForDisplay({
      intent_id: 'intent-1',
      payload: { signature: '0xdeadbeef' },
      grant_jti: 'jti-1',
      safe: ['visible', { authorization: 'Bearer secret' }],
    })).toEqual({
      intent_id: 'intent-1',
      payload: { signature: '[REDACTED]' },
      grant_jti: '[REDACTED]',
      safe: ['visible', { authorization: '[REDACTED]' }],
    })
  })
})
