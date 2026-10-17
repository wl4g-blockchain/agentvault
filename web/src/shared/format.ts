export function formatTime(epochSeconds?: number): string {
  if (epochSeconds === undefined) return '—'
  return new Intl.DateTimeFormat(undefined, {
    dateStyle: 'medium',
    timeStyle: 'medium',
  }).format(new Date(epochSeconds * 1_000))
}

export function compactId(value?: string): string {
  if (!value) return '—'
  return value.length > 24 ? `${value.slice(0, 12)}…${value.slice(-8)}` : value
}

export function displayError(error: unknown): string {
  return error instanceof Error ? error.message : 'AgentVault request failed'
}
