import { useCallback, useEffect, useLayoutEffect, useRef, useState, type ReactNode } from 'react'
import { api, expired, friendly, type Job } from './api'
import { Actions, useActions } from './context'

interface ResourceProps<T> {
  path: string
  children: (data: T, reload: (quiet?: boolean) => Promise<T | undefined>) => ReactNode
  initialJobs?: (data: T) => (Job | null)[]
}

export function Resource<T>({ path, children, initialJobs }: ResourceProps<T>) {
  const parent = useActions()
  const [loaded, setLoaded] = useState<{ path: string; data: T }>()
  const [error, setError] = useState('')
  const [pending, setPending] = useState(true)
  const request = useRef<{ path: string; controller: AbortController; promise: Promise<T | undefined> } | null>(null)
  const expireRef = useRef(parent.expire)
  expireRef.current = parent.expire

  const reload = useCallback((quiet = false) => {
    request.current?.controller.abort()
    const controller = new AbortController()
    const newer = () => request.current?.path === path && request.current.controller !== controller
      ? request.current.promise : undefined
    if (!quiet) setPending(true)
    setError('')
    const promise = api<T>(path, { signal: controller.signal }).then((data) => {
      if (controller.signal.aborted) return newer()
      setLoaded({ path, data })
      return data
    }).catch((error) => {
      if (controller.signal.aborted) return newer()
      if (expired(error)) expireRef.current()
      else if (!quiet) setError(friendly(error))
      throw error
    }).finally(() => { if (!controller.signal.aborted) setPending(false) })
    request.current = { path, controller, promise }
    return promise
  }, [path])
  const refresh = useCallback(async () => {
    try { await reload() } catch { /* The resource displays the error. */ }
  }, [reload])
  useEffect(() => {
    void refresh()
    return () => {
      request.current?.controller.abort()
      request.current = null
    }
  }, [refresh])

  useEffect(() => {
    if (loaded?.path !== path) return
    let active = true
    for (const job of initialJobs?.(loaded.data) ?? []) {
      if (job?.active) void parent.follow(job.id).then((result) => { if (active && result) void refresh() })
    }
    // Job completion causes a fresh snapshot. The workspace deduplicates polling and notices.
    return () => { active = false }
  }, [loaded, path])

  return <Actions.Provider value={{ ...parent, refresh }}>
    {pending && <div className="navigation-progress" role="status" aria-label="Loading page" />}
    {error && <div className="admin-notice action-error" role="alert">{error} <button className="small ghost" onClick={refresh}>Retry</button></div>}
    {loaded ? <>{children(loaded.data, reload)}<RestoreScroll ready={loaded.path === path} /></> : pending ? <p className="hint">Loading…</p> : null}
  </Actions.Provider>
}

function RestoreScroll({ ready }: { ready: boolean }) {
  const { onReady, path } = useActions()
  useLayoutEffect(() => { if (ready) onReady?.() }, [ready, path, onReady])
  return null
}
