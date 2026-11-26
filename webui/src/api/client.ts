import type { ActionFields, ActionResult } from '../api'
import { apiUrl } from '../paths'

export class RequestError extends Error {
  constructor(public status: number, message: string) { super(message) }
}

export async function api<T>(path: string, options: RequestInit = {}): Promise<T> {
  const response = await fetch(apiUrl(path), {
    ...options,
    credentials: 'same-origin',
    cache: 'no-store',
    headers: { Accept: 'application/json', ...options.headers },
  })
  const result = await response.json().catch(() => null)
  if (!response.ok) throw new RequestError(response.status, result?.error || 'Request failed (' + response.status + ')')
  return result as T
}

export const friendly = (error: unknown) => error instanceof Error ? error.message : 'Request failed'
export const expired = (error: unknown) => error instanceof RequestError && error.status === 401

export function action(path: string, fields: ActionFields, csrf: string, signal?: AbortSignal) {
  const body = new URLSearchParams({ csrf })
  for (const [name, value] of Object.entries(fields)) {
    if (value != null) body.set(name, String(value))
  }
  return api<ActionResult>(path, { method: 'POST', body, signal })
}
