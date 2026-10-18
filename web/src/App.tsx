import { Navigate, Route, Routes } from 'react-router-dom'
import { DashboardLayout } from './layout/DashboardLayout'
import { ApprovalsPage } from './pages/ApprovalsPage'
import { BalancesPage } from './pages/BalancesPage'
import { ClientsPage } from './pages/ClientsPage'
import { OverviewPage } from './pages/OverviewPage'
import { RuntimePage } from './pages/RuntimePage'
import { ScenariosPage } from './pages/ScenariosPage'
import { TransactionsPage } from './pages/TransactionsPage'

export function App() {
  return <Routes>
    <Route element={<DashboardLayout />}>
      <Route index element={<OverviewPage />} />
      <Route path="payer" element={<TransactionsPage direction="outbound" />} />
      <Route path="payee" element={<TransactionsPage direction="inbound" />} />
      <Route path="approvals" element={<ApprovalsPage />} />
      <Route path="balances" element={<BalancesPage />} />
      <Route path="clients" element={<ClientsPage />} />
      <Route path="scenarios" element={<ScenariosPage />} />
      <Route path="runtime" element={<RuntimePage />} />
    </Route>
    <Route path="*" element={<Navigate to="/" replace />} />
  </Routes>
}
