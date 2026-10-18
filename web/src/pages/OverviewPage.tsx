import { ArrowDownLeft, ArrowUpRight, DatabaseZap, Fingerprint, RadioTower, Route } from 'lucide-react'
import { Link } from 'react-router-dom'
import { capabilities } from '../api/client'
import { Notice, PageHeading } from '../shared/components'

export function OverviewPage() {
  const liveReads = Object.values(capabilities).filter(Boolean).length
  return <>
    <PageHeading eyebrow="CONTROL PLANE / OVERVIEW" title="Payments with a memory.">
      Observe bidirectional agent payments without moving identity credentials into the browser. Funds remain governed by scenario policy, one-time approvals and confirmed ledger evidence.
    </PageHeading>

    <section className="metric-grid">
      <article><ArrowUpRight /><small>OUTBOUND / PAYER</small><strong>Challenge → policy → signature</strong><p>Agent SDKs retry paid HTTP and MCP calls only after intent-bound authorization.</p></article>
      <article><ArrowDownLeft /><small>INBOUND / PAYEE</small><strong>Offer → settlement → delivery</strong><p>Paid resources execute after external provider evidence is confirmed and persisted.</p></article>
      <article><DatabaseZap /><small>AUTHORITATIVE STATE</small><strong>SQL + append-only ledger</strong><p>Redis and memory cache can accelerate reads, never authorize funds or replace durable facts.</p></article>
    </section>

    <section className="split-grid section-gap">
      <article className="panel flow-card">
        <div className="panel-title"><Route /><div><small>RESOURCE GRAPH</small><h2>Client → Scenario → Policy → Wallet</h2></div></div>
        <div className="flow-line"><span>Client</span><b>1:N</b><span>Scenario</span><b>1:N</b><span>Policy</span><b>1:N</b><span>Wallet</span></div>
        <p>Wallets may be shared through explicit policy associations; wallet-level hard limits aggregate that exposure.</p>
        <Link className="text-link" to="/scenarios">Manage Scenario wallet pools →</Link>
      </article>
      <article className="panel capability-card">
        <div className="panel-title"><RadioTower /><div><small>HTTP SURFACE</small><h2>{liveReads} operational capabilities</h2></div></div>
        <ul>
          <li><span>Payer and Payee transaction listing</span><b>available</b></li>
          <li><span>Pending approval decisions</span><b>available</b></li>
          <li><span>Ledger and balance conservation</span><b>available</b></li>
          <li><span>Scenario + wallet Policy creation</span><b>available</b></li>
        </ul>
      </article>
    </section>

    <Notice><b>No browser token storage.</b> The dashboard sends same-origin cookies and ordinary request metadata only. AuthGuard/Envoy owns HTTP authentication when enabled; disabled-auth installations use the identical UI bundle.</Notice>
  </>
}
