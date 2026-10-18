import { FormEvent, useEffect, useMemo, useState, type ReactNode } from 'react'
import { Calculator, CircleDollarSign, Equal, RefreshCw, Scale } from 'lucide-react'
import { managementApi, paymentApi } from '../api/client'
import type { PaymentBalance, PaymentLedgerEntry } from '../api/types'
import { DEFAULT_PAGE_SIZE, Notice, PageHeading, Pagination, StatusPill } from '../shared/components'
import { compactId, displayError, formatTime } from '../shared/format'

export function BalancesPage() {
  const [balance, setBalance] = useState<PaymentBalance>()
  const [entries, setEntries] = useState<PaymentLedgerEntry[]>([])
  const [conservationEntries, setConservationEntries] = useState<PaymentLedgerEntry[]>([])
  const [conservationComplete, setConservationComplete] = useState(false)
  const [total, setTotal] = useState(0)
  const [offset, setOffset] = useState(0)
  const [error, setError] = useState('')
  const [busy, setBusy] = useState(false)
  const [ledgerBusy, setLedgerBusy] = useState(false)

  async function loadLedgerPage(nextOffset: number) {
    setLedgerBusy(true); setError('')
    try {
      const page = await managementApi.listLedger({ offset: nextOffset, limit: DEFAULT_PAGE_SIZE })
      setEntries(page.items); setTotal(page.total); setOffset(nextOffset)
    } catch (caught) {
      setEntries([]); setTotal(0); setError(displayError(caught))
    } finally { setLedgerBusy(false) }
  }

  async function refreshLedger() {
    setLedgerBusy(true); setError('')
    try {
      const [page, facts] = await Promise.all([
        managementApi.listLedger({ offset, limit: DEFAULT_PAGE_SIZE }),
        managementApi.listAllLedger(),
      ])
      setEntries(page.items); setTotal(page.total)
      setConservationEntries(facts.items); setConservationComplete(facts.complete)
    } catch (caught) {
      setEntries([]); setTotal(0); setConservationEntries([]); setConservationComplete(false)
      setError(displayError(caught))
    } finally { setLedgerBusy(false) }
  }

  useEffect(() => { void refreshLedger() }, []) // eslint-disable-line react-hooks/exhaustive-deps

  const conservation = useMemo(() => {
    const payments = conservationEntries.filter(entry => entry.entry_type === 'payment')
    const inbound = payments.filter(entry => entry.direction === 'inbound').reduce((sum, entry) => sum + BigInt(entry.amount), 0n)
    const outbound = payments.filter(entry => entry.direction === 'outbound').reduce((sum, entry) => sum + BigInt(entry.amount), 0n)
    const complete = total > 0 && conservationComplete && conservationEntries.length === total
    return { inbound, outbound, conserved: complete && payments.length > 0 && inbound === outbound, complete, facts: payments.length }
  }, [conservationComplete, conservationEntries, total])

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
    <PageHeading eyebrow="FUNDS / BALANCES" title="Conservation backed by confirmed facts.">
      Compare paired Payer and Payee settlement postings, then inspect the authoritative balance of one Scenario wallet. Pending reservations remain separate from confirmed Ledger money.
    </PageHeading>

    <section className="metric-grid conservation-grid" data-testid="balance-conservation" data-conserved={conservation.conserved}>
      <article className="panel"><ArrowMetric icon={<CircleDollarSign />} label="PAYEE / INBOUND" value={conservation.inbound.toString()} detail={`${conservation.facts} confirmed payment facts loaded`} /></article>
      <article className="panel"><ArrowMetric icon={<Scale />} label="PAYER / OUTBOUND" value={conservation.outbound.toString()} detail={`${total} total ledger rows available`} /></article>
      <article className="panel"><ArrowMetric icon={<Equal />} label="PAYMENT CONSERVATION" value={conservation.conserved ? 'PASS' : 'REVIEW'} detail={conservation.complete ? `delta ${conservation.inbound - conservation.outbound}` : `${conservationEntries.length} of ${total} rows loaded`} status={conservation.conserved ? 'settled' : 'pending'} /></article>
    </section>

    <div className="split-grid section-gap">
      <form className="panel query-card" onSubmit={submit}>
        <div className="panel-title"><Calculator /><div><small>AUTHORITATIVE QUERY</small><h2>Scenario wallet balance</h2></div></div>
        <div className="field-pair"><label>Scenario ID<input required name="scenario" data-testid="balance-scenario" /></label><label>Wallet ID<input required name="wallet" data-testid="balance-wallet" /></label></div>
        <div className="field-pair"><label>Network<input required name="network" defaultValue="eip155:31337" /></label><label>Asset<input required name="asset" defaultValue="USDC" /></label></div>
        <div className="field-pair"><label>Token contract<input name="contract" /></label><label>Decimals<input required name="decimals" type="number" min="0" max="255" defaultValue="6" /></label></div>
        <button className="primary" disabled={busy}>{busy ? 'Calculating…' : 'Calculate balance'}</button>
      </form>
      <section className="panel result-card" data-testid="wallet-balance">
        <div className="panel-title"><CircleDollarSign /><div><small>LEDGER AGGREGATE</small><h2>Balance view</h2></div></div>
        {balance ? <><div className="balance-grid"><div><small>INBOUND</small><b>{balance.confirmed_inbound}</b></div><div><small>OUTBOUND</small><b>{balance.confirmed_outbound}</b></div><div><small>RESERVED</small><b>{balance.reserved_outbound}</b></div><div><small>AVAILABLE</small><b>{balance.available ?? '—'}</b></div></div><p className="as-of">{balance.wallet_id} · {balance.asset.asset} · as of {formatTime(balance.as_of)}</p></> : <p className="placeholder">Select one Scenario and Wallet. Asset identity includes network, token contract and decimals.</p>}
      </section>
    </div>
    {error && <p className="error-banner">{error}</p>}

    <section className="panel section-gap transaction-card" data-testid="ledger-records" data-total={total}>
      <div className="panel-title padded"><Scale /><div><small>{total} APPEND-ONLY FACTS</small><h2>Settlement ledger</h2></div><button className="secondary panel-action" onClick={() => void refreshLedger()} disabled={ledgerBusy}><RefreshCw className={ledgerBusy ? 'spin' : ''} />Refresh</button></div>
      <div className="data-table ledger-table">
        <div className="table-row table-head"><span>Entry / intent</span><span>Wallet / scenario</span><span>Direction</span><span>Amount</span><span>Evidence</span><span>Effective</span></div>
        {entries.map(entry => <article className="table-row" key={entry.id}>
          <div><b>{compactId(entry.id)}</b><code>{entry.intent_id ? compactId(entry.intent_id) : entry.entry_type}</code></div>
          <div><b>{entry.wallet_id}</b><code>{entry.scenario_id ?? 'platform'}</code></div>
          <StatusPill value={entry.direction} />
          <div><b>{entry.amount}</b><code>{entry.asset.asset} · {entry.asset.network}</code></div>
          <code>{entry.tx_hash ? compactId(entry.tx_hash) : entry.settlement_ref ?? '—'}</code>
          <time>{formatTime(entry.effective_at)}</time>
        </article>)}
      </div>
      <Pagination label="Settlement ledger" offset={offset} total={total} busy={ledgerBusy} onChange={next => void loadLedgerPage(next)} />
    </section>
    <Notice>Conservation compares confirmed payment postings only. Deposits, fees, transfers and reversals remain visible in the Ledger but are not mislabeled as one Payer↔Payee settlement pair.</Notice>
  </>
}

function ArrowMetric({ icon, label, value, detail, status }: { icon: ReactNode; label: string; value: string; detail: string; status?: string }) {
  return <>{icon}<small>{label}</small><strong>{value}</strong>{status && <StatusPill value={status} />}<p>{detail}</p></>
}
