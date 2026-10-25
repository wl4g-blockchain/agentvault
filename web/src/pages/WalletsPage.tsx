import { FormEvent, useEffect, useRef, useState } from 'react'
import { Check, KeyRound, ShieldCheck, WalletCards } from 'lucide-react'
import { managementApi } from '../api/client'
import type { Wallet } from '../api/types'
import { encryptWithPublicKey } from '../security/e2ee'
import { DEFAULT_PAGE_SIZE, Notice, PageHeading, Pagination } from '../shared/components'
import { displayError } from '../shared/format'

export function WalletsPage() {
  const [wallets, setWallets] = useState<Wallet[]>([])
  const [total, setTotal] = useState(0)
  const [offset, setOffset] = useState(0)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')
  const [created, setCreated] = useState<Wallet>()
  const privateKeyInput = useRef<HTMLInputElement>(null)

  async function load(nextOffset: number) {
    setBusy(true); setError('')
    try {
      const page = await managementApi.list('wallets', undefined, nextOffset, DEFAULT_PAGE_SIZE)
      setWallets(page.items as Wallet[]); setTotal(page.total); setOffset(nextOffset)
    } catch (caught) { setWallets([]); setTotal(0); setError(displayError(caught)) } finally { setBusy(false) }
  }

  useEffect(() => { void load(0) }, [])

  async function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault()
    const form = event.currentTarget
    const data = new FormData(form)
    const id = String(data.get('wallet_id') ?? '').trim()
    let privateKey: Uint8Array<ArrayBuffer> | undefined
    setBusy(true); setError(''); setCreated(undefined)
    try {
      if (!id || /[\u0000-\u001f\u007f]/.test(id)) throw new Error('Wallet ID is invalid.')
      privateKey = decodePrivateKey(privateKeyInput.current?.value ?? '')
      if (privateKeyInput.current) privateKeyInput.current.value = ''
      const session = await managementApi.createWalletImportSession()
      if (session.expires_at * 1_000 <= Date.now()) throw new Error('Wallet encryption session expired.')
      const ciphertext = await encryptWithPublicKey(
        session.server_public_key, privateKey,
        `agentvault.wallet-import.v2\0${session.session_id}\0${id}\0secp256k1`,
      )
      const wallet = await managementApi.createWallet({
        id,
        algorithm: 'secp256k1',
        enabled: true,
        encrypted_private_key: { session_id: session.session_id, ciphertext },
      })
      setCreated(wallet)
      form.reset()
      await load(0)
    } catch (caught) { setError(displayError(caught)) } finally {
      privateKey?.fill(0)
      if (privateKeyInput.current) privateKeyInput.current.value = ''
      setBusy(false)
    }
  }

  return <>
    <PageHeading eyebrow="GOVERNANCE / WALLETS" title="Import an EOA without plaintext transport.">
      Your browser encrypts the private key with a temporary public key from AgentVault. Only encrypted key material is submitted and stored.
    </PageHeading>
    <div className="wallet-layout">
      <form className="panel wallet-form" onSubmit={submit} data-testid="wallet-create-form">
        <div className="panel-title"><KeyRound /><div><small>CLIENT-SIDE ENCRYPTED</small><h2>New EOA Wallet</h2></div></div>
        <label>Wallet ID<input required name="wallet_id" maxLength={128} autoComplete="off" placeholder="wallet-settlement-1" /></label>
        <label>Algorithm<input readOnly value="secp256k1" aria-label="Algorithm" /></label>
        <label>EOA private key<input required ref={privateKeyInput} type="password" autoComplete="new-password" spellCheck={false} placeholder="0x + 64 hex characters" /></label>
        <button className="primary" disabled={busy}>{busy ? 'Encrypting and creating…' : 'Create encrypted Wallet'}</button>
        {error && <p className="error-banner">{error}</p>}
        <Notice>The private key is never placed in React state, logs, URL parameters or plaintext JSON. The input is cleared after every attempt.</Notice>
      </form>
      <div>
        {created && <section className="panel creation-success"><Check /><div><small>WALLET COMMITTED</small><h2>{created.id}</h2><p>{created.address}</p></div></section>}
        <section className="panel wallet-list section-gap">
          <div className="panel-title padded"><WalletCards /><div><small>{total} REGISTERED</small><h2>Wallets</h2></div></div>
          {wallets.map(wallet => <article key={wallet.id}><div><b>{wallet.id}</b><code>{wallet.address}</code><small>{wallet.algorithm}</small></div></article>)}
          {wallets.length === 0 && <p className="placeholder">No Wallets are registered.</p>}
          <Pagination label="Wallets" offset={offset} total={total} busy={busy} onChange={next => void load(next)} />
        </section>
        <section className="panel section-gap governance-note"><ShieldCheck /><div><small>ONE-USE SESSION</small><h2>Temporary public-key encryption</h2><p>Each import uses a short-lived key tied to your signed-in identity. An expired or used session cannot import another wallet.</p></div></section>
      </div>
    </div>
  </>
}

function decodePrivateKey(value: string): Uint8Array<ArrayBuffer> {
  const canonical = value.trim().replace(/^0x/i, '')
  if (!/^[0-9a-fA-F]{64}$/.test(canonical)) {
    throw new Error('EOA private key must contain exactly 32 hexadecimal bytes.')
  }
  return Uint8Array.from(canonical.match(/../g)!, byte => Number.parseInt(byte, 16))
}
