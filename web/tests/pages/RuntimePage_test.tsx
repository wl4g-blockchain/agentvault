import { render, screen } from '@testing-library/react'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { RuntimePage } from '../../src/pages/RuntimePage'

afterEach(() => vi.unstubAllGlobals())

describe('RuntimePage', () => {
  it('loads liveness and readiness through the same-origin management proxy', async () => {
    const fetchMock = vi.fn().mockImplementation(async () => new Response('ok', { status: 200 }))
    vi.stubGlobal('fetch', fetchMock)
    render(<RuntimePage />)

    expect((await screen.findAllByText('200 · ok')).length).toBe(2)
    expect(fetchMock).toHaveBeenCalledTimes(2)
  })
})
