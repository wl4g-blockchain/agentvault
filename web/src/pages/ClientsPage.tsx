import { useEffect, useState } from 'react'
import { Network, ShieldCheck, Users } from 'lucide-react'
import { managementApi } from '../api/client'
import type { PaymentClient, PaymentScenario } from '../api/types'
import { DEFAULT_PAGE_SIZE, Notice, PageHeading, Pagination, StatusPill } from '../shared/components'
import { displayError } from '../shared/format'

export function ClientsPage() {
  const [clients, setClients] = useState<PaymentClient[]>([])
  const [clientTotal, setClientTotal] = useState(0)
  const [clientOffset, setClientOffset] = useState(0)
  const [selectedClientId, setSelectedClientId] = useState('')
  const [scenarios, setScenarios] = useState<PaymentScenario[]>([])
  const [scenarioTotal, setScenarioTotal] = useState(0)
  const [scenarioOffset, setScenarioOffset] = useState(0)
  const [busy, setBusy] = useState(false)
  const [scenarioBusy, setScenarioBusy] = useState(false)
  const [error, setError] = useState('')

  async function loadClients(nextOffset: number) {
    setBusy(true); setError('')
    try {
      const page = await managementApi.list('clients', undefined, nextOffset, DEFAULT_PAGE_SIZE)
      const nextClients = page.items as PaymentClient[]
      setClients(nextClients)
      setClientTotal(page.total)
      setClientOffset(nextOffset)
      setSelectedClientId(current => nextClients.some(client => client.id === current) ? current : (nextClients[0]?.id ?? ''))
    } catch (caught) {
      setClients([]); setClientTotal(0); setSelectedClientId(''); setError(displayError(caught))
    } finally { setBusy(false) }
  }

  async function loadScenarios(clientId: string, nextOffset: number) {
    if (!clientId) { setScenarios([]); setScenarioTotal(0); return }
    setScenarioBusy(true); setError('')
    try {
      const page = await managementApi.list('scenarios', clientId, nextOffset, DEFAULT_PAGE_SIZE)
      setScenarios(page.items as PaymentScenario[])
      setScenarioTotal(page.total)
      setScenarioOffset(nextOffset)
    } catch (caught) {
      setScenarios([]); setScenarioTotal(0); setError(displayError(caught))
    } finally { setScenarioBusy(false) }
  }

  useEffect(() => { void loadClients(0) }, [])
  useEffect(() => { setScenarioOffset(0); void loadScenarios(selectedClientId, 0) }, [selectedClientId])

  const selectedClient = clients.find(client => client.id === selectedClientId)
  return <>
    <PageHeading eyebrow="GOVERNANCE / CLIENTS" title="Identity owns Scenarios, not request bodies.">
      A Client binds one trusted transport identity to its Scenarios. SDK signing commands send only a Scenario ID; AgentVault derives and verifies the owning Client at the transport boundary.
    </PageHeading>

    <div className="client-layout">
      <section className="panel client-list" data-testid="client-list">
        <div className="panel-title padded"><Users /><div><small>{clientTotal} REGISTERED</small><h2>Clients</h2></div></div>
        {clients.map(client => <button
          type="button"
          key={client.id}
          className={`client-row${client.id === selectedClientId ? ' selected' : ''}`}
          aria-pressed={client.id === selectedClientId}
          onClick={() => setSelectedClientId(client.id)}
        >
          <span><b>{client.name}</b><code>{client.id}</code><small>{client.auth_principal}</small></span>
          <StatusPill value={client.enabled ? 'enabled' : 'disabled'} />
        </button>)}
        {clients.length === 0 && <p className="placeholder">No Clients are registered.</p>}
        <Pagination label="Clients" offset={clientOffset} total={clientTotal} busy={busy} onChange={next => void loadClients(next)} />
      </section>

      <section className="panel scenario-list client-scenarios" data-testid="client-scenarios">
        <div className="panel-title padded"><Network /><div><small>{scenarioTotal} ASSOCIATED</small><h2>{selectedClient ? `${selectedClient.name} Scenarios` : 'Client Scenarios'}</h2></div></div>
        {scenarios.map(scenario => <article key={scenario.id}><div><b>{scenario.name}</b><code>{scenario.id}</code></div><StatusPill value={scenario.enabled ? 'enabled' : 'disabled'} /></article>)}
        {selectedClient && scenarios.length === 0 && <p className="placeholder">This Client has no Scenarios.</p>}
        {!selectedClient && <p className="placeholder">Select a Client to inspect its Scenarios.</p>}
        <Pagination label="Associated scenarios" offset={scenarioOffset} total={scenarioTotal} busy={scenarioBusy} onChange={next => void loadScenarios(selectedClientId, next)} />
      </section>
    </div>
    {error && <p className="error-banner">{error}</p>}
    <Notice>Client identity bindings are provisioned through controlled deployment manifests and remain read-only in the Dashboard.</Notice>
    <section className="panel section-gap governance-note"><ShieldCheck /><div><small>TRUSTED OWNERSHIP</small><h2>Why Client remains explicit</h2><p>AuthGuard principals and MQTT client identities resolve to a Client before Scenario authorization. A caller cannot nominate a different Client inside a signing payload.</p></div></section>
  </>
}
