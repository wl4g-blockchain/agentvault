import { FormEvent, useCallback, useEffect, useState } from 'react'
import { ArrowDownLeft, ArrowUpRight, RefreshCw } from 'lucide-react'
import { managementApi } from '../api/client'
import type { PaymentDirection, PaymentIntent, PaymentStatus } from '../api/types'
import { DEFAULT_PAGE_SIZE, PageHeading, Pagination, StatusPill } from '../shared/components'
import { compactId, displayError, formatTime } from '../shared/format'

const statusOptions: Array<PaymentStatus | ''> = [
  '', 'created', 'pending_approval', 'authorized', 'signed', 'submitted',
  'pending_settlement', 'settled', 'failed', 'expired', 'unknown',
]

export function TransactionsPage({ direction }: { direction: PaymentDirection }) {
  const [items, setItems] = useState<PaymentIntent[]>([])
  const [total, setTotal] = useState(0)
  const [offset, setOffset] = useState(0)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')
  const [filters, setFilters] = useState({ scenarioId: '', runId: '', status: '' as PaymentStatus | '' })
  const payer = direction === 'outbound'
  const Icon = payer ? ArrowUpRight : ArrowDownLeft

  const load = useCallback(async (next = filters, nextOffset = offset) => {
    setBusy(true); setError('')
    try {
      const page = await managementApi.listPayments({
        direction,
        scenario_id: next.scenarioId || undefined,
        run_id: next.runId || undefined,
        status: next.status || undefined,
        offset: nextOffset,
        limit: DEFAULT_PAGE_SIZE,
      })
      setItems(page.items); setTotal(page.total)
    } catch (caught) {
      setItems([]); setTotal(0); setError(displayError(caught))
    } finally { setBusy(false) }
  }, [direction, filters, offset])

  useEffect(() => {
    setOffset(0)
    void load(filters, 0)
  }, [direction]) // eslint-disable-line react-hooks/exhaustive-deps

  function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault()
    const data = new FormData(event.currentTarget)
    const next = {
      scenarioId: String(data.get('scenario') || ''),
      runId: String(data.get('run') || ''),
      status: String(data.get('status') || '') as PaymentStatus | '',
    }
    setFilters(next)
    setOffset(0)
    void load(next, 0)
  }

  function changePage(nextOffset: number) {
    setOffset(nextOffset)
    void load(filters, nextOffset)
  }

  return <>
    <PageHeading
      eyebrow={`TRANSACTIONS / ${payer ? 'PAYER' : 'PAYEE'}`}
      title={payer ? 'Outbound payment runs.' : 'Inbound settlement runs.'}
    >
      {payer
        ? 'Every row follows one business run through 402 interception, policy, wallet-pool selection, signing, paid retry and reconciliation.'
        : 'Every row follows one paid resource through challenge, Facilitator verification, settlement and delivery release.'}
    </PageHeading>

    <form className="panel transaction-filter" onSubmit={submit}>
      <label>Scenario ID<input name="scenario" defaultValue={filters.scenarioId} placeholder="all scenarios" /></label>
      <label>Run ID<input name="run" defaultValue={filters.runId} placeholder="all runs" /></label>
      <label>Status<select name="status" defaultValue={filters.status}>{statusOptions.map(value => <option key={value || 'all'} value={value}>{value ? value.replaceAll('_', ' ') : 'all statuses'}</option>)}</select></label>
      <button className="secondary" disabled={busy}><RefreshCw className={busy ? 'spin' : ''} />Refresh</button>
    </form>
    {error && <p className="error-banner">{error}</p>}

    <section className="panel section-gap transaction-card" data-testid={`${direction}-transactions`} data-total={total}>
      <div className="panel-title padded"><Icon /><div><small>{total} DURABLE {direction.toUpperCase()} INTENTS</small><h2>{payer ? 'Payer transaction record' : 'Payee transaction record'}</h2></div></div>
      <div className="data-table transaction-table">
        <div className="table-row table-head"><span>Run / intent</span><span>Scenario / wallet</span><span>Protocol / resource</span><span>Amount</span><span>Status</span><span>Updated</span></div>
        {items.map(intent => <article className="table-row" key={intent.id} data-run-id={intent.run_id}>
          <div><b>{intent.run_id}</b><code>{compactId(intent.id)}</code></div>
          <div><b>{intent.scenario_id}</b><code>{intent.wallet_id ?? 'wallet pending'}</code></div>
          <div><b>{intent.protocol}</b><code title={intent.resource}>{intent.resource}</code></div>
          <div><b>{intent.amount}</b><code>{intent.asset.asset} · {intent.asset.network}</code></div>
          <StatusPill value={intent.status} />
          <time>{formatTime(intent.updated_at)}</time>
        </article>)}
        {!busy && items.length === 0 && <p className="placeholder">No {direction} payment intents match this scope.</p>}
      </div>
      <Pagination label={`${payer ? 'Payer' : 'Payee'} transactions`} offset={offset} total={total} busy={busy} onChange={changePage} />
    </section>
  </>
}
