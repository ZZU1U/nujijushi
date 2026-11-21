import { createContext, useContext } from 'react'
import type { Job } from './api'

export interface PageActions {
  csrf: string
  path: string
  navigate: (path: string, replace?: boolean) => void
  refresh: () => void
  expire: () => void
  notice: (message: string) => void
  follow: (id: number) => Promise<Job | undefined>
  jobs: ReadonlyMap<number, Job>
  onReady?: () => void
}

export const Actions = createContext<PageActions | null>(null)

export function useActions() {
  const context = useContext(Actions)
  if (!context) throw new Error('Missing admin page context')
  return context
}
