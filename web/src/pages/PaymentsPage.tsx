import { FormEvent, useState } from 'react'
import { FileCheck2, Search, Waypoints } from 'lucide-react'
import { paymentApi, redactForDisplay } from '../api/client'
import type { PaymentIntent, PaymentReceipt } from '../api/types'
import { JsonPanel, PageHeading, StatusPill } from '../shared/components'
import { compactId, displayError, formatTime } from '../shared/format'

export function PaymentsPage() {
  const [intent, setIntent] = useState<PaymentIntent>()
  const [receipt, setReceipt] = useState<PaymentReceipt>()
  const [error, setError] = useState('')
  const [busy, setBusy] = useState(false)

  async function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault()
    const data = new FormData(event.currentTarget)
    const scenario = String(data.get('scenario'))
    const intentId = String(data.get('intent'))
    setBusy(true); setError(''); setIntent(undefined); setReceipt(undefined)
    try {
      const payment = await paymentApi.getPayment(scenario, intentId)
      setIntent(payment)
      if (payment.status === 'settled') {
        try { setReceipt(await paymentApi.getReceipt(scenario, intentId)) } catch { /* receipt can lag reconciliation */ }
      }
    } catch (caught) { setError(displayError(caught)) } finally { setBusy(false) }
  }

  return <>
    <PageHeading eyebrow="OPERATIONS / PAYMENTS" title="Trace one durable intent.">
      Lookup is explicitly scoped by scenario. The API does not expose cross-tenant payment listing or accept a caller-provided client identity.
    </PageHeading>
    <div className="split-grid">
      <form className="panel query-card" onSubmit={submit}>
        <div className="panel-title"><Search /><div><small>AVAILABLE READ</small><h2>Payment lookup</h2></div></div>
        <label>Scenario ID<input required name="scenario" autoComplete="off" /></label>
        <label>Intent ID<input required name="intent" autoComplete="off" /></label>
        <button className="primary" disabled={busy}>{busy ? 'Loading…' : 'Inspect payment'}</button>
        {error && <p className="error-banner">{error}</p>}
      </form>
      <section className="panel result-card">
        <div className="panel-title"><Waypoints /><div><small>INTENT STATE</small><h2>Lifecycle</h2></div></div>
        {intent ? <>
          <div className="result-summary"><div><small>{intent.direction}</small><b>{compactId(intent.id)}</b></div><StatusPill value={intent.status} /></div>
          <dl className="facts"><div><dt>Amount</dt><dd>{intent.amount} {intent.asset.asset}</dd></div><div><dt>Protocol</dt><dd>{intent.protocol}</dd></div><div><dt>Reservation</dt><dd>{intent.reservation_status}</dd></div><div><dt>Updated</dt><dd>{formatTime(intent.updated_at)}</dd></div></dl>
          <JsonPanel value={redactForDisplay(intent)} />
        </> : <p className="placeholder">A durable intent remains the authority for pending state; confirmed money facts belong to the ledger.</p>}
      </section>
    </div>
    {receipt && <section className="panel section-gap receipt-card"><div className="panel-title"><FileCheck2 /><div><small>CONFIRMED EVIDENCE</small><h2>Settlement receipt</h2></div></div><JsonPanel value={redactForDisplay(receipt)} /></section>}
  </>
}
