import {
  Activity, ArrowDownLeft, ArrowUpRight, BookOpenCheck, Boxes, CircleGauge,
  Landmark, Network, PanelLeftClose, Scale, Users, WalletCards,
} from 'lucide-react'
import { NavLink, Outlet } from 'react-router-dom'

const transactionLinks = [
  { to: '/payer', label: 'Payer transactions', icon: ArrowUpRight },
  { to: '/payee', label: 'Payee transactions', icon: ArrowDownLeft },
  { to: '/approvals', label: 'Approvals', icon: BookOpenCheck },
  { to: '/balances', label: 'Balances & ledger', icon: Scale },
]

const governanceLinks = [
  { to: '/clients', label: 'Clients', icon: Users },
  { to: '/scenarios', label: 'Scenarios', icon: Network },
  { to: '/wallets', label: 'Wallets', icon: WalletCards },
]

const operationsLinks = [
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
        <p>TRANSACTIONS</p>
        {transactionLinks.map(({ to, label, icon: Icon }) => <NavLink key={to} to={to}><Icon />{label}</NavLink>)}
        <p>GOVERNANCE</p>
        {governanceLinks.map(({ to, label, icon: Icon }) => <NavLink key={to} to={to}><Icon />{label}</NavLink>)}
        <p>PLATFORM</p>
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
