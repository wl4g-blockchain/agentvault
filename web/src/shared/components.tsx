import type { ReactNode } from 'react'
import { AlertTriangle, LockKeyhole } from 'lucide-react'

export const DEFAULT_PAGE_SIZE = 10

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

export function Pagination({
  label,
  offset,
  total,
  onChange,
  busy = false,
  limit = DEFAULT_PAGE_SIZE,
}: {
  label: string
  offset: number
  total: number
  onChange: (offset: number) => void
  busy?: boolean
  limit?: number
}) {
  const pageCount = Math.max(1, Math.ceil(total / limit))
  const currentPage = Math.min(pageCount, Math.floor(offset / limit) + 1)
  const first = total === 0 ? 0 : offset + 1
  const last = Math.min(offset + limit, total)

  return <nav className="pagination" aria-label={`${label} pagination`}>
    <button
      className="secondary"
      type="button"
      disabled={busy || offset === 0}
      onClick={() => onChange(Math.max(0, offset - limit))}
    >Previous</button>
    <span><b>{first}–{last}</b> of {total} · Page {currentPage} of {pageCount} · {limit} per page</span>
    <button
      className="secondary"
      type="button"
      disabled={busy || offset + limit >= total}
      onClick={() => onChange(offset + limit)}
    >Next</button>
  </nav>
}
