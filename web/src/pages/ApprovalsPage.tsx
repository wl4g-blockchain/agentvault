import { FormEvent, useState } from 'react'
import { Check, ClipboardCheck, RefreshCw, X } from 'lucide-react'
import { approvalApi } from '../api/client'
import type { ApprovalRequest } from '../api/types'
import { Notice, PageHeading, StatusPill } from '../shared/components'
import { compactId, displayError, formatTime } from '../shared/format'

export function ApprovalsPage() {
  const [scenario, setScenario] = useState('')
  const [runId, setRunId] = useState('')
  const [items, setItems] = useState<ApprovalRequest[]>([])
  const [error, setError] = useState('')
  const [busy, setBusy] = useState(false)

  async function load(event?: FormEvent) {
    event?.preventDefault()
    if (!scenario) return
    setBusy(true); setError('')
    try { setItems((await approvalApi.listPending(scenario)).items) }
    catch (caught) { setError(displayError(caught)) }
    finally { setBusy(false) }
  }

  async function decide(approval: ApprovalRequest, decision: 'approve' | 'reject') {
    if (!runId) { setError('Workflow run ID is required to bind the decision to its notification context.'); return }
    setBusy(true); setError('')
    try {
      await approvalApi.decide({
        scenarioId: scenario,
        runId,
        approvalId: approval.id,
        decision,
        approvedAmount: decision === 'approve' ? approval.requested_amount : undefined,
      })
      await load()
    } catch (caught) { setError(displayError(caught)) }
    finally { setBusy(false) }
  }

  return <>
    <PageHeading eyebrow="OPERATIONS / APPROVALS" title="One decision. One grant.">
      Approvals add bounded headroom for one immutable intent. They do not modify policy, bypass wallet hard limits, or expose the internal JWT grant.
    </PageHeading>
    <form className="panel inline-form" onSubmit={load}>
      <label>Scenario ID<input required value={scenario} onChange={event => setScenario(event.target.value)} /></label>
      <label>Workflow run ID<input required value={runId} onChange={event => setRunId(event.target.value)} /></label>
      <button className="primary" disabled={busy}><RefreshCw className={busy ? 'spin' : ''} />Load pending</button>
    </form>
    {error && <p className="error-banner">{error}</p>}
    <section className="panel section-gap">
      <div className="panel-title padded"><ClipboardCheck /><div><small>PENDING QUEUE</small><h2>{items.length} requests</h2></div></div>
      {items.length === 0 ? <div className="empty-state compact"><ClipboardCheck /><p>No pending approval has been loaded for this scenario.</p></div> : <div className="approval-list">
        {items.map(approval => <article key={approval.id}>
          <div><small>APPROVAL</small><b title={approval.id}>{compactId(approval.id)}</b><code>{compactId(approval.intent_id)}</code></div>
          <div><small>REQUESTED</small><strong>{approval.requested_amount}</strong><span>expires {formatTime(approval.expires_at)}</span></div>
          <StatusPill value={approval.status} />
          <div className="row-actions"><button className="approve" disabled={busy} onClick={() => void decide(approval, 'approve')}><Check />Approve</button><button className="reject" disabled={busy} onClick={() => void decide(approval, 'reject')}><X />Reject</button></div>
        </article>)}
      </div>}
    </section>
    <Notice tone="warning">The UI uses a deterministic idempotency key per approval and decision. A changed decision or amount is a different business operation and must not be disguised as a retry.</Notice>
  </>
}
