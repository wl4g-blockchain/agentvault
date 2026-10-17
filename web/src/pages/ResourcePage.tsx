import { FormEvent, useState, type ReactNode } from 'react'
import { BadgeDollarSign, Building2, Network, Search, ShieldCheck, WalletCards } from 'lucide-react'
import { managementApi, offerApi, redactForDisplay } from '../api/client'
import type { PaymentOffer } from '../api/types'
import { JsonPanel, Notice, PageHeading, StatusPill } from '../shared/components'
import { displayError } from '../shared/format'

type Resource = 'clients' | 'scenarios' | 'policies' | 'wallets' | 'offers'

const copy = {
  clients: { icon: Building2, title: 'Clients', eyebrow: 'RESOURCE CONTROL / CLIENTS', description: 'Business applications bound to a trusted upstream principal.' },
  scenarios: { icon: Network, title: 'Scenarios', eyebrow: 'RESOURCE CONTROL / SCENARIOS', description: 'Workflow and agent-flow boundaries owned by one client.' },
  policies: { icon: ShieldCheck, title: 'Policies', eyebrow: 'RESOURCE CONTROL / POLICIES', description: 'Independent inbound sales caps and outbound spend limits, bound to authorized wallets.' },
  wallets: { icon: WalletCards, title: 'Wallets', eyebrow: 'RESOURCE CONTROL / WALLETS', description: 'EOA metadata, encrypted versioned key custody and aggregate hard limits.' },
  offers: { icon: BadgeDollarSign, title: 'Offers', eyebrow: 'RESOURCE CONTROL / OFFERS', description: 'Fixed-price paid resources with server-resolved recipient wallets.' },
} satisfies Record<Resource, { icon: typeof Building2; title: string; eyebrow: string; description: string }>

export function ResourcePage({ resource }: { resource: Resource }) {
  const details = copy[resource]
  const Icon = details.icon
  return <>
    <PageHeading eyebrow={details.eyebrow} title={details.title}>{details.description}</PageHeading>
    {resource === 'offers' ? <OfferResolver /> : <ResourceBrowser icon={<Icon />} resource={resource} />}
  </>
}

function ResourceBrowser({ icon, resource }: { icon: ReactNode; resource: Exclude<Resource, 'offers'> }) {
  const [items, setItems] = useState<unknown[]>([])
  const [total, setTotal] = useState(0)
  const [error, setError] = useState('')
  const [busy, setBusy] = useState(false)
  const requiresParent = resource === 'scenarios' || resource === 'policies'

  async function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault()
    const data = new FormData(event.currentTarget)
    setBusy(true); setError('')
    try {
      const result = await managementApi.list(resource, String(data.get('parent') || '') || undefined)
      setItems(result.items); setTotal(result.total)
    } catch (caught) {
      setItems([]); setTotal(0); setError(displayError(caught))
    } finally { setBusy(false) }
  }

  return <div className="split-grid">
    <form className="panel query-card" onSubmit={submit}>
      <div className="panel-title">{icon}<div><small>ADMIN READ</small><h2>Browse {resource}</h2></div></div>
      {requiresParent && <label>{resource === 'scenarios' ? 'Client ID' : 'Scenario ID'}<input required name="parent" autoComplete="off" /></label>}
      <button className="primary" disabled={busy}>{busy ? 'Loading…' : `Load ${resource}`}</button>
      {error && <p className="error-banner">{error}</p>}
      <Notice>AuthGuard protects this route by default. AgentVault applies payment ownership and lifecycle invariants.</Notice>
    </form>
    <section className="panel result-card">
      <div className="panel-title">{icon}<div><small>{total} DURABLE RECORDS</small><h2>Authoritative SQL view</h2></div></div>
      {items.length ? <JsonPanel value={redactForDisplay(items)} /> : <p className="placeholder">Run the query to load the current page. Secret key material is never returned.</p>}
    </section>
  </div>
}

function OfferResolver() {
  const [offer, setOffer] = useState<PaymentOffer>()
  const [error, setError] = useState('')
  const [busy, setBusy] = useState(false)

  async function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault()
    const data = new FormData(event.currentTarget)
    setBusy(true); setError(''); setOffer(undefined)
    try {
      setOffer(await offerApi.resolve(String(data.get('scenario')), String(data.get('resource')), String(data.get('protocol') || '') || undefined))
    } catch (caught) { setError(displayError(caught)) } finally { setBusy(false) }
  }

  return <div className="split-grid">
    <form className="panel query-card" onSubmit={submit}>
      <div className="panel-title"><Search /><div><small>AVAILABLE READ</small><h2>Resolve an offer</h2></div></div>
      <label>Scenario ID<input required name="scenario" autoComplete="off" placeholder="scenario-market-feed" /></label>
      <label>Absolute resource URL<input required name="resource" type="url" autoComplete="off" placeholder="https://market.example/mcp/realtime" /></label>
      <label>Protocol (optional)<select name="protocol" defaultValue="x402"><option value="x402">x402</option><option value="mpp">MPP</option><option value="">Any supported protocol</option></select></label>
      <button className="primary" disabled={busy}>{busy ? 'Resolving…' : 'Resolve fixed-price offer'}</button>
      {error && <p className="error-banner">{error}</p>}
    </form>
    <section className="panel result-card">
      <div className="panel-title"><BadgeDollarSign /><div><small>SERVER AUTHORITY</small><h2>Resolved pricing</h2></div></div>
      {offer ? <><div className="result-summary"><div><small>OFFER</small><b>{offer.id}</b></div><StatusPill value={offer.enabled ? 'enabled' : 'disabled'} /></div><JsonPanel value={redactForDisplay(offer)} /></> : <p className="placeholder">Enter an exact resource. The caller cannot override its amount, asset, network or receiving wallet.</p>}
    </section>
  </div>
}
