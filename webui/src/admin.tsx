import { Fragment, useEffect, useLayoutEffect, useRef, useState, type FormEvent, type KeyboardEvent as ReactKeyboardEvent, type MouseEvent } from 'react'
import { createRoot } from 'react-dom/client'
import { viewNodes, type Job } from './view'
import '../../static/app.css'
import './admin.css'

interface View { html: string }
interface Mutation { location: string; message?: string; job_id?: number; job_ids?: number[] }
interface Cards { html: string; next_cursor: string | null }
interface GridPosition { count: number; selected: Set<string>; channel: string | null }
interface QueueStatus { revision: string }

class RequestError extends Error {
  constructor(public status: number, message: string) { super(message) }
}

async function api<T>(path: string, options: RequestInit = {}): Promise<T> {
  const endpoint = path === '/' || path.startsWith('/?') ? '/dashboard' + path.slice(1) : path
  const response = await fetch('/api/v1' + endpoint, {
    ...options, credentials: 'same-origin', headers: { Accept: 'application/json' },
  })
  const result = await response.json().catch(() => ({})) as T & { error?: string }
  if (!response.ok) throw new RequestError(response.status, result.error || 'Request failed (' + response.status + ')')
  return result
}

const current = () => location.pathname + location.search
const sleep = (ms: number) => new Promise<void>((resolve) => setTimeout(resolve, ms))
const active = (state: string) => state === 'pending' || state === 'running'
const friendly = (error: unknown) => error instanceof Error ? error.message : 'Request failed'
const postPath = /^\/posts\/\d+$/
const queuePath = /^\/channels\/\d+\/queue$/
const route = /^(?:\/$|\/(?:login|posts(?:\/\d+(?:\/delete)?)?|channels(?:\/\d+\/(?:edit|queue|preview))?|jobs|import|users|watches|duplicates|settings)$)/

const grid = (view: Document) => view.querySelector<HTMLElement>('#bulk[data-next-cursor]')
const cursorOf = (view: Document) => grid(view)?.dataset.nextCursor
const cardsIn = (view: Document) => view.querySelectorAll<HTMLElement>('[data-post-card]')

function setCursor(view: Document, cursor: string | null) {
  const posts = grid(view)
  if (posts) posts.dataset.nextCursor = cursor || ''
}

async function appendCards(view: Document, path: string, signal: AbortSignal) {
  const cursor = cursorOf(view)
  const target = grid(view)?.querySelector('.grid')
  if (!cursor || !target) return
  const query = new URL(path, location.origin).searchParams
  query.set('cursor', cursor)
  const batch = await api<Cards>('/posts/cards?' + query, { signal })
  const parsed = new DOMParser().parseFromString(batch.html, 'text/html')
  const ids = new Set(Array.from(cardsIn(view), (card) => card.dataset.postCard))
  for (const card of cardsIn(parsed)) {
    if (!ids.has(card.dataset.postCard)) target.append(card)
  }
  setCursor(view, batch.next_cursor === cursor ? null : batch.next_cursor)
}

function feedback(form: HTMLFormElement, message: string, error = false) {
  const target = form.querySelector<HTMLElement>(':scope > [data-action-feedback]')
  if (!target) return
  target.textContent = message
  target.hidden = message === ''
  target.classList.toggle('action-error', error)
}

function updateSelection() {
  const control = document.querySelector<HTMLElement>('[data-post-selection]')
  if (!control) return
  const picks = Array.from(document.querySelectorAll<HTMLInputElement>('#bulk [data-post-pick]'))
  const count = picks.filter((pick) => pick.checked).length
  control.hidden = false
  const label = control.querySelector('[data-post-selection-count]')
  const all = control.querySelector<HTMLButtonElement>('[data-select-posts]')
  const clear = control.querySelector<HTMLButtonElement>('[data-clear-posts]')
  if (label) label.textContent = count + ' selected'
  if (all) all.disabled = count === picks.length
  if (clear) clear.disabled = count === 0
}

function Admin() {
  const [documentView, setDocumentView] = useState<Document>()
  const [loading, setLoading] = useState(true)
  const [pageError, setPageError] = useState('')
  const [notice, setNotice] = useState('')
  const [jobs, setJobs] = useState(new Map<number, Job>())
  const [queueDisconnected, setQueueDisconnected] = useState(false)
  const [batchState, setBatchState] = useState<'idle' | 'loading' | 'failed'>('idle')
  const appending = useRef<AbortController | null>(null)
  const filterTimer = useRef<number | undefined>(undefined)
  const filterFocus = useRef<{ name: string; selection: number | null } | null>(null)
  const gridPositions = useRef(new Map<string, GridPosition>())
  const cache = useRef(new Map<string, { document: Document; received: number }>())
  const scrollPositions = useRef(new Map<string, number>())
  const scrollNext = useRef<number | null>(null)
  const renderedPath = useRef('')
  const lastGrid = useRef('/posts')
  const request = useRef<AbortController | null>(null)
  const busyForms = useRef(new Set<HTMLFormElement>())
  const polling = useRef(new Set<number>())

  function rememberGrid() {
    const form = grid(document)
    if (!form || !renderedPath.current.startsWith('/posts')) return
    const channel = form.querySelector<HTMLSelectElement>('select[name="channel"]')
    const defaultChannel = channel?.querySelector<HTMLOptionElement>('option[selected]')?.value || ''
    gridPositions.current.set(renderedPath.current, {
      count: cardsIn(document).length,
      selected: new Set(Array.from(form.querySelectorAll<HTMLInputElement>('[data-post-pick]:checked'),
        (pick) => pick.id)),
      channel: channel && channel.value !== defaultChannel ? channel.value : null,
    })
  }

  async function more() {
    if (!documentView || !cursorOf(documentView) || appending.current || loading
        || location.pathname !== '/posts' || current() !== renderedPath.current) return
    const path = current()
    const controller = new AbortController()
    appending.current = controller
    setBatchState('loading')
    try {
      const next = documentView.cloneNode(true) as Document
      await appendCards(next, path, controller.signal)
      if (controller.signal.aborted || current() !== path) return
      rememberGrid()
      cache.current.set(path, { document: next, received: Date.now() })
      setDocumentView(next)
      setBatchState('idle')
    } catch {
      if (!controller.signal.aborted) setBatchState('failed')
    } finally {
      if (appending.current === controller) appending.current = null
    }
  }

  async function load(path: string, allowCache = false, background = false): Promise<boolean> {
    request.current?.abort()
    appending.current?.abort()
    appending.current = null
    setBatchState('idle')
    const controller = new AbortController()
    request.current = controller
    const cached = allowCache ? cache.current.get(path) : undefined
    if (cached && Date.now() - cached.received < 15_000) {
      setDocumentView(cached.document)
      renderedPath.current = path
      setPageError('')
      setLoading(false)
      return true
    }
    if (!background) {
      setLoading(true)
      setPageError('')
    }
    try {
      const result = await api<View>(path, { signal: controller.signal })
      const next = new DOMParser().parseFromString(result.html, 'text/html')
      if (renderedPath.current.split('?')[0] === path.split('?')[0]) {
        document.querySelectorAll<HTMLDetailsElement>('details[id][open]').forEach((element) => {
          next.getElementById(element.id)?.setAttribute('open', '')
        })
      }
      if (controller.signal.aborted) return false
      const screen = new URL(path, location.origin)
      const grid = next.querySelector<HTMLElement>('[data-grid-page]')
      if (grid?.dataset.gridPage && screen.searchParams.has('page')
          && screen.searchParams.get('page') !== grid.dataset.gridPage) {
        screen.searchParams.set('page', grid.dataset.gridPage)
        history.replaceState(null, '', screen)
        path = screen.pathname + screen.search
      }
      const saved = gridPositions.current.get(path)
      if (saved && screen.pathname === '/posts') {
        try {
          while (cursorOf(next) && cardsIn(next).length < saved.count && !controller.signal.aborted) {
            await appendCards(next, path, controller.signal)
          }
        } catch {
          if (!controller.signal.aborted) setBatchState('failed')
        }
      }
      if (controller.signal.aborted) return false
      cache.current.set(path, { document: next, received: Date.now() })
      if (screen.pathname === '/posts' && renderedPath.current === path) scrollNext.current = scrollY
      if (queuePath.test(screen.pathname) && renderedPath.current.split('?')[0] === screen.pathname) {
        scrollNext.current = scrollY
      }
      renderedPath.current = path
      setDocumentView(next)
      return true
    } catch (error) {
      if (controller.signal.aborted) return false
      if (error instanceof RequestError && error.status === 401 && !path.startsWith('/login')) {
        const login = '/login?next=' + encodeURIComponent(path + location.hash)
        history.replaceState(null, '', login)
        cache.current.clear()
        setDocumentView(undefined)
        return load(login)
      }
      if (!background) setPageError(friendly(error))
      return false
    } finally {
      if (!background && !controller.signal.aborted) setLoading(false)
    }
  }

  function navigate(url: URL, replace = false, resetGrid = false) {
    window.clearTimeout(filterTimer.current)
    rememberGrid()
    if (renderedPath.current) scrollPositions.current.set(renderedPath.current, scrollY)
    if (location.pathname === '/posts') lastGrid.current = current()
    const path = url.pathname + url.search
    if (!resetGrid) filterFocus.current = null
    if (resetGrid) {
      gridPositions.current.delete(path)
      scrollPositions.current.delete(path)
      cache.current.delete(path)
    }
    scrollNext.current = scrollPositions.current.get(path) ?? 0
    if (replace) history.replaceState(null, '', url)
    else history.pushState(null, '', url)
    setNotice('')
    void load(path, true)
  }

  async function followJob(id: number, path: string, form?: HTMLFormElement) {
    if (polling.current.has(id)) return
    polling.current.add(id)
    try {
      for (;;) {
        const job = await api<Job>('/jobs/' + id + '/status')
        setJobs((previous) => {
          const next = new Map(previous)
          next.set(job.id, job)
          return next
        })
        if (!active(job.state)) {
          const message = job.message || (job.state === 'failed' ? 'The job failed' : 'Finished')
          if (form?.isConnected) feedback(form, message, job.state === 'failed')
          if (current() === path && (job.state === 'done' || !form)) {
            if (job.kind === 'merge_media') {
              document.querySelectorAll<HTMLInputElement>('input[form="merge-gallery"]').forEach((pick) => { pick.checked = false })
            }
            cache.current.clear()
            if (form) setNotice(message)
            await load(path)
          }
          break
        }
        if (form?.isConnected) {
          const progress = job.kind === 'merge_media' && job.state === 'running'
            ? (job.progress < job.total ? `Stitching ${job.progress} of ${job.total}…` : 'Encoding and saving…')
            : 'Working…'
          feedback(form, job.message || (job.state === 'pending' ? 'Queued…' : progress))
        }
        await sleep(job.kind === 'merge_media' ? 400 : 1800)
      }
    } catch (error) {
      if (form?.isConnected) feedback(form, 'Could not check job status. ' + friendly(error), true)
    } finally {
      polling.current.delete(id)
    }
  }

  useEffect(() => {
    function pop() {
      window.clearTimeout(filterTimer.current)
      filterFocus.current = null
      rememberGrid()
      if (renderedPath.current) scrollPositions.current.set(renderedPath.current, scrollY)
      scrollNext.current = scrollPositions.current.get(current()) ?? 0
      setNotice('')
      void load(current(), true)
    }
    function search(event: KeyboardEvent) {
      const target = event.target
      if (event.key !== '/' || event.altKey || event.ctrlKey || event.metaKey
        || (target instanceof HTMLElement && (target.isContentEditable || /^(INPUT|SELECT|TEXTAREA)$/.test(target.tagName)))) return
      const field = document.querySelector<HTMLInputElement>('form.filters input[name="search"]')
      if (field) { event.preventDefault(); field.focus(); field.select() }
    }
    history.scrollRestoration = 'manual'
    addEventListener('popstate', pop)
    addEventListener('keydown', search)
    void load(current())
    return () => { removeEventListener('popstate', pop); removeEventListener('keydown', search) }
  }, [])

  useLayoutEffect(() => {
    if (!documentView) return
    document.title = documentView.title
    document.body.classList.toggle('login-body', documentView.body.classList.contains('login-body'))
    const saved = gridPositions.current.get(renderedPath.current)
    const form = grid(document)
    if (saved && form) {
      form.querySelectorAll<HTMLInputElement>('[data-post-pick]').forEach((pick) => {
        pick.checked = saved.selected.has(pick.id)
      })
      const channel = form.querySelector<HTMLSelectElement>('select[name="channel"]')
      if (channel && saved.channel !== null && Array.from(channel.options).some((option) => option.value === saved.channel)) {
        channel.value = saved.channel
      }
    }
    if (filterFocus.current && form) {
      const { name, selection } = filterFocus.current
      const control = document.querySelector<HTMLInputElement | HTMLSelectElement>('[data-post-filters] [name="' + CSS.escape(name) + '"]')
      control?.focus({ preventScroll: true })
      if (control instanceof HTMLInputElement && selection !== null) control.setSelectionRange(selection, selection)
    }
    filterFocus.current = null
    updateSelection()
    if (scrollNext.current !== null) {
      scrollTo(0, scrollNext.current)
      scrollNext.current = null
    }
    if (location.hash) document.getElementById(location.hash.slice(1))?.scrollIntoView()
  }, [documentView])

  useEffect(() => {
    document.querySelectorAll<HTMLElement>('[data-job][data-state]').forEach((row) => {
      if (active(row.dataset.state || '')) void followJob(Number(row.dataset.job), current())
    })
  }, [documentView])

  const viewedQueue = renderedPath.current.split('?')[0]
  useEffect(() => {
    setQueueDisconnected(false)
    if (!queuePath.test(viewedQueue)) return
    let timer: number | undefined
    let stopped = false
    let inFlight = false
    const controller = new AbortController()

    async function poll() {
      if (inFlight || stopped || document.hidden || location.pathname !== viewedQueue) return
      inFlight = true
      try {
        const status = await api<QueueStatus>(viewedQueue + '/status', { signal: controller.signal })
        if (stopped || document.hidden || location.pathname !== viewedQueue) return
        const existing = document.querySelector<HTMLElement>('[data-queue-revision]')
        if (existing && existing.dataset.queueRevision !== status.revision && busyForms.current.size === 0) {
          setQueueDisconnected(!(await load(current(), false, true)))
        } else {
          setQueueDisconnected(false)
        }
      } catch (error) {
        if (stopped || location.pathname !== viewedQueue) return
        if (error instanceof RequestError && error.status === 401) {
          void load(current())
        } else {
          setQueueDisconnected(true)
        }
      } finally {
        inFlight = false
        if (!stopped && !document.hidden) timer = window.setTimeout(poll, 2000)
      }
    }

    function checkVisibility() {
      window.clearTimeout(timer)
      if (!document.hidden) void poll()
    }

    void poll()
    document.addEventListener('visibilitychange', checkVisibility)
    return () => {
      stopped = true
      controller.abort()
      window.clearTimeout(timer)
      document.removeEventListener('visibilitychange', checkVisibility)
    }
  }, [viewedQueue])

  useEffect(() => {
    const sentinel = document.querySelector<HTMLElement>('[data-posts-more]')
    if (!sentinel || !documentView) return
    const available = Boolean(cursorOf(documentView))
    sentinel.hidden = !available
    const button = sentinel.querySelector<HTMLButtonElement>('[data-load-more]')
    const message = sentinel.querySelector('[data-batch-message]')
    const canObserve = typeof IntersectionObserver !== 'undefined'
    if (button) button.hidden = canObserve && batchState !== 'failed'
    if (message) message.textContent = batchState === 'loading' ? 'Loading more posts…'
      : batchState === 'failed' ? 'Could not load more posts.' : ''
    if (!available || !canObserve || batchState !== 'idle' || loading) return
    const observer = new IntersectionObserver(([entry]) => {
      if (entry?.isIntersecting) void more()
    }, { rootMargin: '320px 0px' })
    observer.observe(sentinel)
    return () => observer.disconnect()
  }, [documentView, batchState, loading])

  function applyFilters(form: HTMLFormElement, replace = false) {
    window.clearTimeout(filterTimer.current)
    const url = new URL('/posts', location.origin)
    new FormData(form).forEach((value, name) => {
      if (typeof value === 'string' && value.trim() && name !== 'per_page') url.searchParams.set(name, value.trim())
    })
    if (url.pathname + url.search !== current()) {
      const control = document.activeElement
      if ((control instanceof HTMLInputElement || control instanceof HTMLSelectElement) && control.form === form) {
        filterFocus.current = { name: control.name, selection: control instanceof HTMLInputElement ? control.selectionStart : null }
      }
      navigate(url, replace, true)
    }
  }

  function change(event: FormEvent) {
    updateSelection()
    const field = event.target
    if (!(field instanceof HTMLInputElement || field instanceof HTMLSelectElement)
        || !field.form?.hasAttribute('data-post-filters')) return
    const form = field.form
    window.clearTimeout(filterTimer.current)
    if (field instanceof HTMLSelectElement) applyFilters(form)
    else filterTimer.current = window.setTimeout(() => {
      if (form.isConnected && location.pathname === '/posts') applyFilters(form, true)
    }, 300)
  }

  function keyDown(event: ReactKeyboardEvent) {
    const field = event.target
    if (event.key === 'Enter' && !event.nativeEvent.isComposing && field instanceof HTMLInputElement
        && field.form?.hasAttribute('data-post-filters')) {
      event.preventDefault()
      applyFilters(field.form)
    }
  }

  function click(event: MouseEvent) {
    if (!(event.target instanceof Element)) return
    if (event.target.closest('[data-load-more]')) {
      void more()
      return
    }
    const select = event.target.closest('[data-select-posts], [data-clear-posts]')
    if (select) {
      const checked = select.hasAttribute('data-select-posts')
      document.querySelectorAll<HTMLInputElement>('#bulk [data-post-pick]').forEach((pick) => { pick.checked = checked })
      updateSelection()
      return
    }
    const anchor = event.target.closest('a[href]')
    if (!(anchor instanceof HTMLAnchorElement) || anchor.target || anchor.hasAttribute('download')
      || event.button !== 0 || event.ctrlKey || event.metaKey || event.altKey || event.shiftKey) return
    const url = new URL(anchor.hasAttribute('data-return-to-posts') ? lastGrid.current : anchor.href, location.origin)
    if (url.origin !== location.origin || !route.test(url.pathname) || (url.hash && url.pathname + url.search === current())) return
    event.preventDefault()
    navigate(url, false, url.pathname === '/posts' && location.pathname === '/posts')
  }

  async function submit(event: FormEvent) {
    const form = event.target
    if (!(form instanceof HTMLFormElement)) return
    const url = new URL(form.getAttribute('action') || current(), location.origin)
    if (url.origin !== location.origin) return
    event.preventDefault()
    const button = (event.nativeEvent as SubmitEvent).submitter
    const data = new URLSearchParams()
    new FormData(form, button).forEach((value, name) => { if (typeof value === 'string') data.append(name, value) })
    if ((form.getAttribute('method') || 'get').toLowerCase() === 'get') {
      if (form.hasAttribute('data-post-filters')) {
        applyFilters(form)
        return
      }
      url.search = data.toString()
      navigate(url)
      return
    }
    if (busyForms.current.has(form)) return
    const confirmText = (button instanceof HTMLElement ? button.dataset.confirm : '') || form.dataset.confirm
    if (confirmText && !window.confirm(confirmText)) return
    const opened = current()
    busyForms.current.add(form)
    form.setAttribute('aria-busy', 'true')
    const verifying = form.hasAttribute('data-verify-destination') && data.get('tg_id')?.trim() !== form.dataset.verifyDestination
    feedback(form, verifying ? 'Checking Telegram…' : 'Working…')
    try {
      const result = await api<Mutation>(url.pathname + url.search, { method: 'POST', body: data })
      if (current() !== opened) return
      cache.current.clear()
      if (url.pathname === '/import' && (result.job_id || result.job_ids?.length)) {
        const targets = form.querySelector<HTMLTextAreaElement>('textarea[name="targets"]')
        if (targets?.value === data.get('targets')) targets.value = ''
      }
      if (result.job_id && postPath.test(location.pathname) && url.pathname.endsWith('/media')) {
        feedback(form, result.message || 'Queued…')
        await followJob(result.job_id, opened, form)
        return
      }
      const destination = new URL(result.location || opened, location.origin)
      if (destination.pathname === '/posts' && /^\/posts\/\d+\/delete$/.test(url.pathname)) {
        navigate(new URL(lastGrid.current, location.origin))
      } else if (destination.pathname + destination.search !== opened && destination.pathname !== location.pathname) {
        navigate(destination)
      } else if (location.pathname === '/posts' && form.id === 'bulk' && url.pathname === '/posts') {
        rememberGrid()
        await load(opened)
      } else {
        await load(destination.pathname + destination.search)
      }
      feedback(form, '')
      if (result.message) setNotice(result.message)
    } catch (error) {
      if (error instanceof RequestError && error.status === 401) {
        navigate(new URL('/login?next=' + encodeURIComponent(opened), location.origin), true)
      } else {
        feedback(form, friendly(error), true)
      }
    } finally {
      busyForms.current.delete(form)
      form.removeAttribute('aria-busy')
    }
  }

  return <div className="admin-shell" onClick={click} onSubmit={submit} onChange={change} onKeyDown={keyDown}>
    {loading && <div className="navigation-progress" role="status" aria-label="Loading page" />}
    {notice && <div className="admin-notice" role="status">{notice}<button className="small ghost" onClick={() => setNotice('')}>Dismiss</button></div>}
    {queueDisconnected && <div className="queue-connection" role="status">Live queue updates disconnected. Retrying…</div>}
    {pageError && <div className="admin-notice action-error" role="alert">{pageError}<button className="small ghost" onClick={() => void load(current())}>Retry</button></div>}
    {documentView ? <Fragment key={renderedPath.current.startsWith('/posts?') ? renderedPath.current : renderedPath.current.split('?')[0]}>
      {viewNodes(documentView.body, { jobs })}
    </Fragment> : loading && <main>Loading nujijushi…</main>}
  </div>
}

const root = document.getElementById('app')
if (root) createRoot(root).render(<Admin />)
