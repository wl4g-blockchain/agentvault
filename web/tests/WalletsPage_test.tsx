import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from 'vitest'
import { managementApi } from '../src/api/client'
import { WalletsPage } from '../src/pages/WalletsPage'
import type { WalletImportSession } from '../src/api/types'

vi.mock('../src/api/client', () => ({
  managementApi: {
    list: vi.fn(),
    createWalletImportSession: vi.fn(),
    createWallet: vi.fn(),
  },
}))

describe('wallet import UI', () => {
  let session: WalletImportSession
  beforeAll(async () => {
    const keys = await crypto.subtle.generateKey(
      { name: 'RSA-OAEP', modulusLength: 3072, publicExponent: new Uint8Array([1, 0, 1]), hash: 'SHA-256' },
      false, ['encrypt', 'decrypt'],
    ) as CryptoKeyPair
    const publicKey = new Uint8Array(await crypto.subtle.exportKey('spki', keys.publicKey))
    session = {
      session_id: '00000000-0000-4000-8000-000000000002',
      server_public_key: btoa(String.fromCharCode(...publicKey))
        .replaceAll('+', '-').replaceAll('/', '_').replace(/=+$/, ''),
      expires_at: Math.floor(Date.now() / 1_000) + 120,
    }
  })
  beforeEach(() => {
    vi.resetAllMocks()
    vi.mocked(managementApi.list).mockResolvedValue({ items: [], total: 0 })
    vi.mocked(managementApi.createWalletImportSession).mockResolvedValue(session)
    vi.mocked(managementApi.createWallet).mockResolvedValue({
      id: 'wallet-ui', algorithm: 'secp256k1', address: '0x1234', public_key: 'public', created_at: 1,
    })
  })
  afterEach(cleanup)

  async function submit(privateKey: string) {
    render(<WalletsPage />)
    await waitFor(() => expect((screen.getByRole('button', { name: 'Create encrypted Wallet' }) as HTMLButtonElement).disabled).toBe(false))
    fireEvent.change(screen.getByLabelText('Wallet ID'), { target: { value: 'wallet-ui' } })
    const input = screen.getByLabelText('EOA private key') as HTMLInputElement
    fireEvent.change(input, { target: { value: privateKey } })
    fireEvent.submit(screen.getByTestId('wallet-create-form'))
    return input
  }

  it('fetches a public key first and sends only the ciphertext to create the wallet', async () => {
    const privateKey = '11'.repeat(32)
    const input = await submit(privateKey)
    expect(input.value).toBe('')
    await waitFor(() => expect(managementApi.createWallet).toHaveBeenCalledOnce())
    const [request] = vi.mocked(managementApi.createWallet).mock.calls[0]
    expect(Object.keys(request.encrypted_private_key).sort()).toEqual(['ciphertext', 'session_id'])
    expect(request.encrypted_private_key.session_id).toBe(session.session_id)
    expect(request.encrypted_private_key.ciphertext).toHaveLength(512)
    expect(JSON.stringify(request)).not.toContain(privateKey)
    expect(vi.mocked(managementApi.createWalletImportSession).mock.invocationCallOrder[0])
      .toBeLessThan(vi.mocked(managementApi.createWallet).mock.invocationCallOrder[0])
    await screen.findByText('WALLET COMMITTED')
  })

  it('rejects malformed key input before requesting a session', async () => {
    const input = await submit('not-a-private-key')
    await screen.findByText(/exactly 32 hexadecimal bytes/)
    expect(input.value).toBe('')
    expect(managementApi.createWalletImportSession).not.toHaveBeenCalled()
    expect(managementApi.createWallet).not.toHaveBeenCalled()
  })

  it('rejects an expired session before sending a create request', async () => {
    vi.mocked(managementApi.createWalletImportSession).mockResolvedValue({ ...session, expires_at: 0 })
    const input = await submit('22'.repeat(32))
    await screen.findByText('Wallet encryption session expired.')
    expect(input.value).toBe('')
    expect(managementApi.createWallet).not.toHaveBeenCalled()
  })

  it('clears the input and never falls back to plaintext when the session API fails', async () => {
    vi.mocked(managementApi.createWalletImportSession).mockRejectedValue(new Error('Session unavailable'))
    const input = await submit('22'.repeat(32))
    await screen.findByText('Session unavailable')
    expect(input.value).toBe('')
    expect(managementApi.createWallet).not.toHaveBeenCalled()
  })
})
