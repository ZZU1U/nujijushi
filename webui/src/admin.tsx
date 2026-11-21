import { useCallback, useEffect, useRef, useState, type FormEvent, type MouseEvent } from 'react'
import { createRoot } from 'react-dom/client'
import {
  action, api, expired, friendly, type DashboardPage, type DuplicatesPage, type ImportPage, type Job,
  type JobsPage, type ChannelEditPage, type ChannelsPage, type PostDeletePage, type PostDetailPage,
  type PreviewPage, type QueuePage, type QueueStatus, type Session, type SettingsPage, type UsersPage, type WatchesPage
} from './api'
import { Actions, ActionForm, useActions } from './ui'
import { Resource } from './resource'
import { Channels } from './screens/channels'
import { ChannelEdit } from './screens/channel_edit'
import { Dashboard } from './screens/dashboard'
import { Duplicates } from './screens/duplicates'
import { Import } from './screens/import'
import { Jobs } from './screens/jobs'
import { PostDelete } from './screens/post_delete'
import { PostDetail } from './screens/post_detail'
import { Posts, type GridSnapshot } from './screens/posts'
import { Preview } from './screens/preview'
import { Queue } from './screens/queue'
import { Settings } from './screens/settings'
import { Users } from './screens/users'
import { Watches } from './screens/watches'
import './theme.css'
import './admin.css'

const nav = [
  ['/', 'dashboard'], ['/posts', 'posts'], ['/channels', 'channels'], ['/import', 'import'],
  ['/watches', 'watched sources'], ['/users', 'users'], ['/settings', 'settings'], ['/jobs', 'jobs'],
]
const localRoute = /^(?:\/|\/(?:login|posts(?:\/\d+(?:\/delete)?)?|channels(?:\/\d+\/(?:edit|queue|preview))?|jobs|import|users|watches|duplicates|settings))$/
const current = () => location.pathname + location.search + location.hash
const sleep = (ms: number) => new Promise<void>((resolve) => setTimeout(resolve, ms))
const loginUrl = (path: string) => '/login?next=' + encodeURIComponent(path)

function Login({ onLogin }: { onLogin: (session: Session, path: string) => void }) {
  const [challenge, setChallenge] = useState<string>()
  const [token, setToken] = useState('')
  const [error, setError] = useState('')
  const [busy, setBusy] = useState(false)
  const next = new URLSearchParams(location.search).get('next') ?? '/'
  useEffect(() => {
    const controller = new AbortController()
    api<Session>('/login', { signal: controller.signal }).then(({ csrf }) => setChallenge(csrf))
      .catch((error) => { if (!controller.signal.aborted) setError(friendly(error)) })
    return () => controller.abort()
  }, [])
  async function submit(event: FormEvent) {
    event.preventDefault()
    if (busy) return
    setBusy(true)
    setError('')
    try {
      const csrf = challenge ?? (await api<Session>('/login')).csrf
      const result = await action('/login', { token, next }, csrf)
      onLogin(await api<Session>('/session'), result.location || '/')
    } catch (error) {
      setChallenge(undefined)
      setError(friendly(error))
    } finally {
      setBusy(false)
    }
  }
  return <main className="login">
    <h1><span className="dot" /> nujijushi</h1>
    <p className="hint">Use the UI token from your nujijushi configuration.</p>
    <form className="card" onSubmit={submit}>
      <label>ui token <input name="token" type="password" value={token} onChange={(event) => setToken(event.target.value)} autoFocus required /></label>
      <button type="submit" disabled={busy}>unlock</button>
      {error && <p className="action-error" role="alert">{error}</p>}
    </form>
  </main>
}

function LiveQueue({ data }: { data: QueuePage }) {
  const { refresh, expire } = useActions()
  const [disconnected, setDisconnected] = useState(false)
  useEffect(() => {
    let timer = 0
    let inFlight = false
    const controller = new AbortController()
    async function poll() {
      if (inFlight || document.hidden || controller.signal.aborted) return
      inFlight = true
      try {
        const next = await api<QueueStatus>('/channels/' + data.channel.id + '/queue/status', { signal: controller.signal })
        if (controller.signal.aborted) return
        setDisconnected(false)
        if (next.revision !== data.activity.revision) refresh()
      } catch (error) {
        if (controller.signal.aborted) return
        if (expired(error)) expire()
        else setDisconnected(true)
      } finally {
        inFlight = false
        if (!controller.signal.aborted && !document.hidden) timer = window.setTimeout(poll, 2000)
      }
    }
    function visible() {
      window.clearTimeout(timer)
      void poll()
    }
    timer = window.setTimeout(poll, 2000)
    document.addEventListener('visibilitychange', visible)
    return () => {
      controller.abort()
      window.clearTimeout(timer)
      document.removeEventListener('visibilitychange', visible)
    }
  }, [data.channel.id, data.activity.revision, refresh, expire])
  return <>{disconnected && <div className="queue-connection" role="status">Live queue updates disconnected. Retrying…</div>}<Queue data={data} /></>
}

function Screen({ path, grids }: { path: string; grids: Map<string, GridSnapshot> }) {
  const url = new URL(path, location.origin)
  const route = url.pathname
  const endpoint = (route === '/' ? '/dashboard' : route) + url.search
  if (route === '/') return <Resource<DashboardPage> path={endpoint} initialJobs={(data) => data.jobs}>{(data) => <Dashboard data={data} />}</Resource>
  if (route === '/posts') return <Posts path={route + url.search} snapshots={grids} />
  if (/^\/posts\/\d+\/delete$/.test(route)) return <Resource<PostDeletePage> path={endpoint}>{(data) => <PostDelete data={data} />}</Resource>
  if (/^\/posts\/\d+$/.test(route)) return <Resource<PostDetailPage> path={endpoint} initialJobs={(data) => [data.merge_job, data.tag_review?.job ?? null]}>{(data, reload) => <PostDetail data={data} reload={reload} />}</Resource>
  if (route === '/channels') return <Resource<ChannelsPage> path={endpoint}>{(data) => <Channels data={data} />}</Resource>
  if (/^\/channels\/\d+\/edit$/.test(route)) return <Resource<ChannelEditPage> path={endpoint}>{(data) => <ChannelEdit data={data} />}</Resource>
  if (/^\/channels\/\d+\/queue$/.test(route)) return <Resource<QueuePage> path={endpoint}>{(data) => <LiveQueue data={data} />}</Resource>
  if (/^\/channels\/\d+\/preview$/.test(route)) return <Resource<PreviewPage> path={endpoint}>{(data) => <Preview data={data} />}</Resource>
  if (route === '/jobs') return <Resource<JobsPage> path={endpoint} initialJobs={(data) => data.jobs}>{(data) => <Jobs data={data} />}</Resource>
  if (route === '/import') return <Resource<ImportPage> path={endpoint} initialJobs={(data) => data.jobs}>{(data) => <Import data={data} />}</Resource>
  if (route === '/watches') return <Resource<WatchesPage> path={endpoint} initialJobs={(data) => data.watches.map((watch) => watch.job)}>{(data) => <Watches data={data} />}</Resource>
  if (route === '/duplicates') return <Resource<DuplicatesPage> path={endpoint}>{(data) => <Duplicates data={data} />}</Resource>
  if (route === '/users') return <Resource<UsersPage> path={endpoint}>{(data) => <Users data={data} />}</Resource>
  if (route === '/settings') return <Resource<SettingsPage> path={endpoint}>{(data) => <Settings data={data} />}</Resource>
  return <p>Page not found.</p>
}

function Workspace({ session, signOut }: { session: Session; signOut: (path: string) => void }) {
  const [path, setPath] = useState(current)
  const [notice, setNotice] = useState('')
  const [jobs, setJobs] = useState(new Map<number, Job>())
  const pathRef = useRef(path)
  const gridBack = useRef('/posts')
  const grids = useRef(new Map<string, GridSnapshot>())
  const positions = useRef(new Map<string, number>())
  const pendingScroll = useRef(true)
  const polling = useRef(new Map<number, { task: Promise<Job | undefined>; controller: AbortController }>())
  const reported = useRef(new Set<number>())
  const mounted = useRef(true)
  const navigationGuard = useRef<(() => Promise<boolean>) | null>(null)
  const navigationAttempt = useRef(0)
  const guardNavigation = useCallback((guard: () => Promise<boolean>) => {
    navigationGuard.current = guard
    return () => { if (navigationGuard.current === guard) navigationGuard.current = null }
  }, [])
  const expire = useCallback(() => signOut(pathRef.current), [signOut])

  const follow = useCallback((id: number) => {
    const existing = polling.current.get(id)
    if (existing) return existing.task
    const controller = new AbortController()
    const task = (async () => {
      try {
        while (mounted.current) {
          if (document.hidden) { await sleep(1000); continue }
          const job = await api<Job>('/jobs/' + id + '/status', { signal: controller.signal })
          if (!mounted.current) return
          setJobs((previous) => new Map(previous).set(id, job))
          if (!job.active) {
            if (!reported.current.has(id)) {
              reported.current.add(id)
              setNotice(job.message || job.kind_label + ' ' + job.state)
            }
            return job
          }
          await sleep(['merge_media', 'compress_media'].includes(job.kind) ? 400 : 1800)
        }
      } catch (error) {
        if (mounted.current) {
          if (expired(error)) expire()
          else setNotice('Could not check job status. ' + friendly(error))
        }
      } finally {
        polling.current.delete(id)
      }
    })()
    polling.current.set(id, { task, controller })
    return task
  }, [expire])

  function change(next: string) {
    if (next === pathRef.current) return
    positions.current.set(pathRef.current, scrollY)
    if (pathRef.current.split(/[?#]/)[0] === '/posts') gridBack.current = pathRef.current
    pendingScroll.current = true
    pathRef.current = next
    setPath(next)
    setNotice('')
  }

  async function navigate(next: string, replace = false) {
    const url = new URL(next, location.origin)
    if (url.origin !== location.origin || !localRoute.test(url.pathname)) return
    next = url.pathname + url.search + url.hash
    if (next === '/posts' && /^\/posts\/\d+\/delete/.test(pathRef.current)) next = gridBack.current
    if (next === pathRef.current) return
    const attempt = ++navigationAttempt.current
    if (navigationGuard.current && !await navigationGuard.current()) return
    if (!mounted.current || attempt !== navigationAttempt.current) return
    change(next)
    if (next !== current()) {
      if (replace) history.replaceState(null, '', next)
      else history.pushState(null, '', next)
    }
  }

  const onReady = useCallback(() => {
    if (!pendingScroll.current) return
    pendingScroll.current = false
    const anchor = location.hash.slice(1)
    if (anchor) document.getElementById(anchor)?.scrollIntoView()
    else scrollTo(0, positions.current.get(pathRef.current) ?? 0)
  }, [])

  useEffect(() => {
    mounted.current = true
    history.scrollRestoration = 'manual'
    const pop = async () => {
      const next = current()
      if (next === pathRef.current) return
      const attempt = ++navigationAttempt.current
      const allowed = !navigationGuard.current || await navigationGuard.current()
      if (!mounted.current || attempt !== navigationAttempt.current) return
      if (allowed) change(next)
      else history.pushState(null, '', pathRef.current)
    }
    window.addEventListener('popstate', pop)
    return () => {
      mounted.current = false
      for (const { controller } of polling.current.values()) controller.abort()
      window.removeEventListener('popstate', pop)
    }
  }, [])

  function click(event: MouseEvent) {
    if (event.defaultPrevented || event.button !== 0 || event.ctrlKey || event.metaKey || event.altKey || event.shiftKey) return
    const anchor = event.target instanceof Element ? event.target.closest('a[href]') : null
    if (!(anchor instanceof HTMLAnchorElement) || anchor.target || anchor.hasAttribute('download')) return
    const url = new URL(anchor.href)
    if (url.origin !== location.origin || !localRoute.test(url.pathname)) return
    if (anchor.getAttribute('href')?.startsWith('#')) return
    event.preventDefault()
    navigate(anchor.hasAttribute('data-return-to-posts') ? gridBack.current : url.pathname + url.search + url.hash)
  }

  const pathname = path.split(/[?#]/)[0]
  const tab = pathname === '/duplicates' ? '/posts' : '/' + (pathname.split('/')[1] ?? '')
  useEffect(() => { document.title = 'nujijushi · ' + (tab === '/' ? 'dashboard' : tab.slice(1)) }, [tab])
  return <Actions.Provider value={{
    csrf: session.csrf, path, navigate, refresh: () => { }, guardNavigation, expire, notice: setNotice, follow, jobs, onReady,
  }}>
    <div className="admin-shell" onClick={click}>
      <header className="top">
        <a className="brand" href="/"><span className="dot" />nujijushi</a>
        <nav>{nav.map(([href, label]) => <a key={href} href={href} className={tab === href ? 'active' : ''}>{label}</a>)}</nav>
        <ActionForm action="/logout" className="inline" beforeSubmit={async () => await navigationGuard.current?.() ?? true}>
          <button className="link" type="submit">log out</button>
        </ActionForm>
      </header>
      {notice && <div className="admin-notice" role="status">{notice}<button className="small ghost" onClick={() => setNotice('')}>Dismiss</button></div>}
      <main key={pathname}><Screen path={path} grids={grids.current} /></main>
      <footer className="foot">nujijushi · local archive · publishes to Telegram via the Bot API</footer>
    </div>
  </Actions.Provider>
}

function Admin() {
  const [session, setSession] = useState<Session>()
  const [loading, setLoading] = useState(location.pathname !== '/login')
  const [error, setError] = useState('')
  const signOut = useCallback((path: string) => {
    history.replaceState(null, '', path === '/login' ? path : loginUrl(path))
    setSession(undefined)
  }, [])
  useEffect(() => {
    if (location.pathname === '/login') return
    const controller = new AbortController()
    api<Session>('/session', { signal: controller.signal }).then(setSession).catch((error) => {
      if (controller.signal.aborted) return
      if (expired(error)) signOut(current())
      else setError(friendly(error))
    }).finally(() => { if (!controller.signal.aborted) setLoading(false) })
    return () => controller.abort()
  }, [signOut])
  useEffect(() => { document.body.classList.toggle('login-body', !session && !loading && !error) }, [session, loading, error])
  if (loading) return <main>Loading nujijushi…</main>
  if (error) return <main role="alert">{error} <button onClick={() => location.reload()}>Retry</button></main>
  if (session) return <Workspace session={session} signOut={signOut} />
  return <Login onLogin={(session, path) => {
    history.replaceState(null, '', path.split('?')[0] === '/login' ? '/' : path)
    setSession(session)
  }} />
}

const root = document.getElementById('app')
if (root) createRoot(root).render(<Admin />)
