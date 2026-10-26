import { FormEvent, useEffect, useState } from 'react'
import { Check, Network, Plus, ShieldCheck, WalletCards } from 'lucide-react'
import { managementApi } from '../api/client'
import type { PaymentClient, PaymentPolicyLimits, PaymentScenario, ScenarioConfiguration, Wallet } from '../api/types'
import { DEFAULT_PAGE_SIZE, Notice, PageHeading, Pagination, StatusPill } from '../shared/components'
import { displayError } from '../shared/format'

export function ScenariosPage() {
  const [clients, setClients] = useState<PaymentClient[]>([])
  const [wallets, setWallets] = useState<Wallet[]>([])
  const [scenarios, setScenarios] = useState<PaymentScenario[]>([])
  const [selectedClient, setSelectedClient] = useState('')
  const [selectedWalletIds, setSelectedWalletIds] = useState<string[]>([])
  const [scenarioTotal, setScenarioTotal] = useState(0)
  const [scenarioOffset, setScenarioOffset] = useState(0)
  const [scenarioBusy, setScenarioBusy] = useState(false)
  const [created, setCreated] = useState<ScenarioConfiguration>()
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')

  useEffect(() => {
    void Promise.all([managementApi.list('clients', undefined, 0, 1_000), managementApi.list('wallets', undefined, 0, 1_000)])
      .then(([clientPage, walletPage]) => {
        const nextClients = clientPage.items as PaymentClient[]
        setClients(nextClients)
        setWallets(walletPage.items as Wallet[])
        if (nextClients[0]) setSelectedClient(nextClients[0].id)
      })
      .catch(caught => setError(displayError(caught)))
  }, [])

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

  useEffect(() => {
    setScenarioOffset(0)
    void loadScenarios(selectedClient, 0)
  }, [selectedClient])

  async function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault()
    const data = new FormData(event.currentTarget)
    const walletIds = data.getAll('wallet').map(String).sort()
    if (walletIds.length === 0) { setError('Select at least one Wallet.'); return }
    const limits = (prefix: 'inbound' | 'outbound'): PaymentPolicyLimits | undefined => {
      if (data.get(`${prefix}_enabled`) !== 'on') return undefined
      return {
        max_per_payment: String(data.get(`${prefix}_max_per_payment`)),
        max_per_day: String(data.get(`${prefix}_max_per_day`)),
        approval_enabled: true,
      }
    }
    const network = String(data.get('network'))
    const asset = String(data.get('asset'))
    const decimals = Number(data.get('decimals'))
    setBusy(true); setError(''); setCreated(undefined)
    try {
      const result = await managementApi.createScenario({
        id: String(data.get('scenario_id')),
        client_id: String(data.get('client_id')),
        name: String(data.get('name')),
        policy: {
          id: String(data.get('policy_id')),
          wallet_ids: walletIds,
          protocols: data.getAll('protocol').map(String).sort(),
          networks: [network],
          assets: [{ network, asset, token_contract: String(data.get('token_contract') || '') || undefined, decimals }],
          budget_unit: { asset, decimals },
          allowed_recipients: splitList(String(data.get('recipients') || '')),
          allowed_origins: splitList(String(data.get('origins') || '')),
          inbound_limits: limits('inbound'),
          outbound_limits: limits('outbound'),
          enabled: true,
        },
      })
      setCreated(result)
      setSelectedClient(result.scenario.client_id)
      await loadScenarios(result.scenario.client_id, 0)
    } catch (caught) { setError(displayError(caught)) } finally { setBusy(false) }
  }

  return <>
    <PageHeading eyebrow="GOVERNANCE / SCENARIOS" title="One Scenario, one policy boundary.">
      A Scenario belongs to one Client. Wallets are selected through its initial Policy, so the same Wallet can participate in multiple Scenarios only under explicit aggregate hard limits.
    </PageHeading>

    <div className="scenario-layout">
      <form className="panel scenario-form" onSubmit={submit} data-testid="scenario-create-form">
        <div className="panel-title"><Plus /><div><small>ATOMIC CONFIGURATION</small><h2>New Scenario + initial Policy</h2></div></div>
        <div className="field-pair">
          <label>Client<select required name="client_id" value={selectedClient} onChange={event => setSelectedClient(event.target.value)} data-testid="scenario-client"><option value="">Select a Client</option>{clients.map(client => <option key={client.id} value={client.id}>{client.name} · {client.id}</option>)}</select></label>
          <label>Scenario ID<input required name="scenario_id" placeholder="scenario-marketpulse" /></label>
        </div>
        <label>Display name<input required name="name" placeholder="MarketPulse subscription" /></label>
        <label>Initial Policy ID<input required name="policy_id" placeholder="policy-marketpulse" /></label>

        <details className="wallet-select" data-testid="wallet-selector">
          <summary>
            <span className="wallet-select-label"><WalletCards /><span><b>Wallet pool</b><small>Select one or more eligible wallets</small></span></span>
            <span className="wallet-selection-count">{selectedWalletIds.length} selected</span>
          </summary>
          <div className="wallet-options" role="group" aria-label="Wallet pool options">
            {wallets.map(wallet => <label className="choice-card" key={wallet.id}>
              <input
                type="checkbox"
                name="wallet"
                value={wallet.id}
                checked={selectedWalletIds.includes(wallet.id)}
                onChange={event => setSelectedWalletIds(current => event.target.checked
                  ? [...new Set([...current, wallet.id])].sort()
                  : current.filter(id => id !== wallet.id))}
              />
              <span><b>{wallet.id}</b><code>{wallet.address}</code></span>
            </label>)}
            {wallets.length === 0 && <p className="placeholder compact">Import or create Wallets before creating a Scenario.</p>}
          </div>
        </details>

        <div className="field-pair">
          <fieldset className="choice-group compact"><legend>Protocols</legend><label className="inline-choice"><input type="checkbox" name="protocol" value="x402" defaultChecked />x402</label><label className="inline-choice"><input type="checkbox" name="protocol" value="mpp" />MPP</label></fieldset>
          <div><label>Network<input required name="network" defaultValue="eip155:31337" /></label><label>Asset<input required name="asset" defaultValue="USDC" /></label></div>
        </div>
        <div className="field-pair"><label>Token contract<input name="token_contract" placeholder="0x…" /></label><label>Decimals<input required name="decimals" type="number" min="0" max="255" defaultValue="6" /></label></div>
        <div className="field-pair"><label>Allowed recipients<textarea required name="recipients" placeholder="one address per line" /></label><label>Allowed origins<textarea required name="origins" placeholder="https://market.example" /></label></div>

        <div className="direction-grid">
          <fieldset className="choice-group compact"><legend><label className="inline-choice"><input type="checkbox" name="outbound_enabled" defaultChecked />Payer / outbound</label></legend><label>Max per payment<input required name="outbound_max_per_payment" defaultValue="1000000" /></label><label>Max per day<input required name="outbound_max_per_day" defaultValue="10000000" /></label></fieldset>
          <fieldset className="choice-group compact"><legend><label className="inline-choice"><input type="checkbox" name="inbound_enabled" defaultChecked />Payee / inbound</label></legend><label>Max per payment<input required name="inbound_max_per_payment" defaultValue="1000000" /></label><label>Max per day<input required name="inbound_max_per_day" defaultValue="10000000" /></label></fieldset>
        </div>
        <button className="primary" disabled={busy || clients.length === 0 || wallets.length === 0}>{busy ? 'Creating…' : 'Create atomically'}</button>
        {error && <p className="error-banner">{error}</p>}
        <Notice>Scenario, Policy and every Wallet membership commit or roll back together. Private keys never enter this form or response.</Notice>
      </form>

      <div>
        {created && <section className="panel creation-success" data-testid="scenario-created"><Check /><div><small>CONFIGURATION COMMITTED</small><h2>{created.scenario.name}</h2><p>{created.policy.wallet_ids.length} Wallets · {created.policy.protocols.join(' + ')}</p></div></section>}
        <section className="panel scenario-list" data-testid="scenario-list">
          <div className="panel-title padded"><Network /><div><small>{scenarioTotal} CONFIGURED</small><h2>Client Scenarios</h2></div></div>
          {scenarios.map(scenario => <article key={scenario.id}><div><b>{scenario.name}</b><code>{scenario.id}</code></div><StatusPill value={scenario.enabled ? 'enabled' : 'disabled'} /></article>)}
          {scenarios.length === 0 && <p className="placeholder">Select a Client to load its Scenario boundaries.</p>}
          <Pagination label="Client scenarios" offset={scenarioOffset} total={scenarioTotal} busy={scenarioBusy} onChange={next => void loadScenarios(selectedClient, next)} />
        </section>
        <section className="panel section-gap governance-note"><ShieldCheck /><div><small>POLICY OWNERSHIP</small><h2>Why Wallets are selected here</h2><p>A Scenario does not own keys. Its Policy authorizes a sorted Wallet pool, assets, origins, recipients and directional limits. AgentVault chooses one eligible Wallet during signing.</p></div></section>
      </div>
    </div>
  </>
}

function splitList(value: string): string[] {
  return [...new Set(value.split(/[\n,]/).map(item => item.trim()).filter(Boolean))].sort()
}
