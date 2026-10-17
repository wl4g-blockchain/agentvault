import { Navigate, Route, Routes } from 'react-router-dom'
import { DashboardLayout } from './layout/DashboardLayout'
import { ApprovalsPage } from './pages/ApprovalsPage'
import { LedgerPage } from './pages/LedgerPage'
import { OverviewPage } from './pages/OverviewPage'
import { PaymentsPage } from './pages/PaymentsPage'
import { ResourcePage } from './pages/ResourcePage'
import { RuntimePage } from './pages/RuntimePage'

export function App() {
  return <Routes>
    <Route element={<DashboardLayout />}>
      <Route index element={<OverviewPage />} />
      <Route path="clients" element={<ResourcePage resource="clients" />} />
      <Route path="scenarios" element={<ResourcePage resource="scenarios" />} />
      <Route path="policies" element={<ResourcePage resource="policies" />} />
      <Route path="wallets" element={<ResourcePage resource="wallets" />} />
      <Route path="offers" element={<ResourcePage resource="offers" />} />
      <Route path="payments" element={<PaymentsPage />} />
      <Route path="approvals" element={<ApprovalsPage />} />
      <Route path="ledger" element={<LedgerPage />} />
      <Route path="runtime" element={<RuntimePage />} />
    </Route>
    <Route path="*" element={<Navigate to="/" replace />} />
  </Routes>
}
