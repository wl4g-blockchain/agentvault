import { FormEvent, useState } from 'react'
import { Calculator, CircleDollarSign, RefreshCcw } from 'lucide-react'
import { capabilities, paymentApi } from '../api/client'
import type { PaymentBalance } from '../api/types'
import { Notice, PageHeading } from '../shared/components'
import { displayError, formatTime } from '../shared/format'

export function LedgerPage() {
  const [balance, setBalance] = useState<PaymentBalance>()
  const [error, setError] = useState('')
  const [busy, setBusy] = useState(false)

  async function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault()
    const data = new FormData(event.currentTarget)
    setBusy(true); setError(''); setBalance(undefined)
    try {
      setBalance(await paymentApi.getBalance({
        scenario_id: String(data.get('scenario')),
        wallet_id: String(data.get('wallet')),
        network: String(data.get('network')),
        asset: String(data.get('asset')),
        token_contract: String(data.get('contract') || '') || undefined,
        decimals: Number(data.get('decimals')),
      }))
    } catch (caught) { setError(displayError(caught)) } finally { setBusy(false) }
  }

  return <>
    <PageHeading eyebrow="OPERATIONS / LEDGER & RECONCILIATION" title="Confirmed facts, not hopeful states.">
      Balance aggregation is computed from trusted ledger evidence. Pending reservations stay on payment intents and are shown separately.
    </PageHeading>
    <div className="split-grid">
      <form className="panel query-card" onSubmit={submit}>
        <div className="panel-title"><Calculator /><div><small>AVAILABLE READ</small><h2>Wallet activity balance</h2></div></div>
        <div className="field-pair"><label>Scenario ID<input required name="scenario" /></label><label>Wallet ID<input required name="wallet" /></label></div>
        <div className="field-pair"><label>Network<input required name="network" placeholder="eip155:31337" /></label><label>Asset<input required name="asset" defaultValue="USDC" /></label></div>
        <div className="field-pair"><label>Token contract<input name="contract" /></label><label>Decimals<input required name="decimals" type="number" min="0" max="255" defaultValue="6" /></label></div>
        <button className="primary" disabled={busy}>{busy ? 'Calculating…' : 'Calculate confirmed balance'}</button>
        {error && <p className="error-banner">{error}</p>}
      </form>
      <section className="panel result-card">
        <div className="panel-title"><CircleDollarSign /><div><small>LEDGER AGGREGATE</small><h2>Balance view</h2></div></div>
        {balance ? <><div className="balance-grid"><div><small>INBOUND</small><b>{balance.confirmed_inbound}</b></div><div><small>OUTBOUND</small><b>{balance.confirmed_outbound}</b></div><div><small>RESERVED</small><b>{balance.reserved_outbound}</b></div><div><small>AVAILABLE</small><b>{balance.available ?? '—'}</b></div></div><p className="as-of">{balance.asset.asset} · {balance.asset.network} · as of {formatTime(balance.as_of)}</p></> : <p className="placeholder">Asset identity includes network, token contract and decimals. Wallet policy limits intentionally aggregate by denomination.</p>}
      </section>
    </div>
    <section className="panel section-gap locked-feature">
      <RefreshCcw /><div><small>RECONCILIATION CONTROL</small><h2>Worker state is observable through telemetry</h2><p>Manual run, retry and ledger-row listing are disabled because no authenticated management Handler exists for them.</p></div><button disabled>{capabilities.reconciliationControl ? 'Run now' : 'Not exposed'}</button>
    </section>
    <Notice>Ledger entries are append-only. Corrections require a new reversal backed by trusted provider or chain evidence; the dashboard never edits historical rows.</Notice>
  </>
}
