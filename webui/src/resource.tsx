import { useCallback, useEffect, useLayoutEffect, useRef, useState, type ReactNode } from 'react'
import { api, expired, friendly, type Job } from './api'
import { Actions, useActions } from './context'

interface ResourceProps<T> {
  path: string
  children: (data: T) => ReactNode
  initialJobs?: (data: T) => (Job | null)[]
}

export function Resource<T>({ path, children, initialJobs }: ResourceProps<T>) {
  const parent = useActions()
  const [loaded, setLoaded] = useState<{ path: string; data: T }>()
  const [error, setError] = useState('')
  const [pending, setPending] = useState(true)
  const [revision, change] = useState(0)
  const refresh = useCallback(() => change((revision) => revision + 1), [])
  const expireRef = useRef(parent.expire)
  expireRef.current = parent.expire

  useEffect(() => {
    const controller = new AbortController()
    setPending(true)
    setError('')
    api<T>(path, { signal: controller.signal }).then((data) => {
      if (!controller.signal.aborted) setLoaded({ path, data })
    }).catch((error) => {
      if (controller.signal.aborted) return
      if (expired(error)) expireRef.current()
      else setError(friendly(error))
    }).finally(() => { if (!controller.signal.aborted) setPending(false) })
    return () => controller.abort()
  }, [path, revision])

  useEffect(() => {
    if (loaded?.path !== path) return
    for (const job of initialJobs?.(loaded.data) ?? []) {
      if (job?.active) void parent.follow(job.id).then((result) => { if (result) refresh() })
    }
  }, [loaded, path, initialJobs, parent, refresh])

  return <Actions.Provider value={{ ...parent, refresh }}>
    {pending && <div className="navigation-progress" role="status" aria-label="Loading page" />}
    {error && <div className="admin-notice action-error" role="alert">{error} <button className="small ghost" onClick={refresh}>Retry</button></div>}
    {loaded ? <>{children(loaded.data)}<RestoreScroll ready={loaded.path === path} /></> : pending ? <p className="hint">Loading…</p> : null}
  </Actions.Provider>
}

function RestoreScroll({ ready }: { ready: boolean }) {
  const { onReady, path } = useActions()
  useLayoutEffect(() => { if (ready) onReady?.() }, [ready, path, onReady])
  return null
}
