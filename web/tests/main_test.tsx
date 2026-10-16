import { beforeEach, describe, expect, it, vi } from 'vitest'

const root = vi.hoisted(() => ({ render: vi.fn() }))
const createRoot = vi.hoisted(() => vi.fn(() => root))

vi.mock('react-dom/client', () => ({ createRoot }))
vi.mock('../src/App', () => ({ App: () => null }))

describe('dashboard bootstrap', () => {
  beforeEach(() => {
    vi.resetModules()
    createRoot.mockClear()
    root.render.mockClear()
    document.body.innerHTML = '<div id="root"></div>'
  })

  it('mounts the application into the declared root element', async () => {
    await import('../src/main')

    expect(createRoot).toHaveBeenCalledWith(document.getElementById('root'))
    expect(root.render).toHaveBeenCalledOnce()
  })
})
