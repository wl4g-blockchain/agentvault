import type { ReactNode } from 'react'
import { AlertTriangle, LockKeyhole } from 'lucide-react'

export function PageHeading({ eyebrow, title, children }: { eyebrow: string; title: string; children: ReactNode }) {
  return <header className="page-heading">
    <div><span className="eyebrow">{eyebrow}</span><h1>{title}</h1><p>{children}</p></div>
  </header>
}

export function Notice({ children, tone = 'neutral' }: { children: ReactNode; tone?: 'neutral' | 'warning' }) {
  return <div className={`notice ${tone}`}>
    {tone === 'warning' ? <AlertTriangle /> : <LockKeyhole />}
    <div>{children}</div>
  </div>
}

export function StatusPill({ value }: { value: string }) {
  const state = /settled|approved|ready|ok|enabled|consumed/i.test(value) ? 'good'
    : /failed|rejected|expired|down|disabled/i.test(value) ? 'bad' : 'pending'
  return <span className={`status ${state}`}>{value.replaceAll('_', ' ')}</span>
}

export function JsonPanel({ value }: { value: unknown }) {
  return <pre className="json-panel">{JSON.stringify(value, null, 2)}</pre>
}
