import {
  Activity, BadgeDollarSign, BookOpenCheck, Boxes, Building2, CircleGauge,
  Landmark, Network, PanelLeftClose, ReceiptText, ScrollText, ShieldCheck, WalletCards,
} from 'lucide-react'
import { NavLink, Outlet } from 'react-router-dom'

const resourceLinks = [
  { to: '/clients', label: 'Clients', icon: Building2 },
  { to: '/scenarios', label: 'Scenarios', icon: Network },
  { to: '/policies', label: 'Policies', icon: ShieldCheck },
  { to: '/wallets', label: 'Wallets', icon: WalletCards },
  { to: '/offers', label: 'Offers', icon: BadgeDollarSign },
]

const operationsLinks = [
  { to: '/payments', label: 'Payments', icon: ReceiptText },
  { to: '/approvals', label: 'Approvals', icon: BookOpenCheck },
  { to: '/ledger', label: 'Ledger & reconcile', icon: ScrollText },
  { to: '/runtime', label: 'Runtime health', icon: Activity },
]

export function DashboardLayout() {
  return <div className="app-shell">
    <aside className="sidebar">
      <NavLink to="/" className="brand" aria-label="AgentVault overview">
        <span><Landmark /></span>
        <div><b>AgentVault</b><small>PAYMENT OPERATIONS</small></div>
      </NavLink>
      <nav aria-label="Primary navigation">
        <NavLink to="/" end><CircleGauge />Overview</NavLink>
        <p>RESOURCE CONTROL</p>
        {resourceLinks.map(({ to, label, icon: Icon }) => <NavLink key={to} to={to}><Icon />{label}</NavLink>)}
        <p>OPERATIONS</p>
        {operationsLinks.map(({ to, label, icon: Icon }) => <NavLink key={to} to={to}><Icon />{label}</NavLink>)}
      </nav>
      <div className="trust-card"><Boxes /><b>Trust boundary</b><p>Identity is established upstream by AuthGuard + Envoy, or explicitly disabled by deployment policy.</p></div>
    </aside>
    <main>
      <header className="topbar">
        <div className="context"><PanelLeftClose /><span>Enterprise machine-payment middleware</span></div>
        <div className="auth-mode"><i /> same-origin identity</div>
      </header>
      <div className="page"><Outlet /></div>
    </main>
  </div>
}
