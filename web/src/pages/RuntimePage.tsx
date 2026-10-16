import { useEffect, useState } from 'react'
import { Activity, RefreshCw, ServerCog } from 'lucide-react'
import { managementApi } from '../api/client'
import { Notice, PageHeading, StatusPill } from '../shared/components'
import { displayError } from '../shared/format'

type Health = Awaited<ReturnType<typeof managementApi.health>>

export function RuntimePage() {
  const [health, setHealth] = useState<Health>()
  const [error, setError] = useState('')
  const [busy, setBusy] = useState(false)

  async function refresh(signal?: AbortSignal) {
    setBusy(true); setError('')
    try { setHealth(await managementApi.health(signal)) }
    catch (caught) { if (!signal?.aborted) setError(displayError(caught)) }
    finally { if (!signal?.aborted) setBusy(false) }
  }

  useEffect(() => { const controller = new AbortController(); void refresh(controller.signal); return () => controller.abort() }, [])

  return <>
    <PageHeading eyebrow="RUNTIME / HEALTH" title="Two probes. One honest signal.">
      Liveness confirms the process; readiness confirms it can receive traffic. Payment settlement health remains visible through bounded OpenTelemetry metrics and reconciliation alerts.
    </PageHeading>
    <section className="runtime-grid">
      <article className="panel"><Activity /><small>LIVENESS</small><h2>Process loop</h2><StatusPill value={health?.liveness.ok ? 'ok' : error ? 'unavailable' : 'checking'} /><code>{health?.liveness.status ?? '—'} · {health?.liveness.message ?? 'awaiting probe'}</code></article>
      <article className="panel"><ServerCog /><small>READINESS</small><h2>Traffic admission</h2><StatusPill value={health?.readiness.ok ? 'ready' : error ? 'unavailable' : 'checking'} /><code>{health?.readiness.status ?? '—'} · {health?.readiness.message ?? 'awaiting probe'}</code></article>
    </section>
    <button className="secondary refresh-button" disabled={busy} onClick={() => void refresh()}><RefreshCw className={busy ? 'spin' : ''} />Refresh probes</button>
    {error && <p className="error-banner">{error}</p>}
    <Notice>The health proxy is a same-origin deployment route. It carries no bearer credential and does not expose the Prometheus payload through this browser.</Notice>
  </>
}
