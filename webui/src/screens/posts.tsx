import { useEffect, useLayoutEffect, useRef, useState } from 'react'
import { api, expired, friendly, type Card, type CardsPage, type PostsPage } from '../api'
import { Actions, ActionForm, useActions, PageLink } from '../ui'
import { Badge } from '../components/Badge'
import { ChannelSelect } from '../components/ChannelSelect'
import { TagAutocomplete } from '../components/TagAutocomplete'
import { Thumbnail } from '../components/Thumbnail'

export interface GridSnapshot {
  page: PostsPage
  appended: Card[]
  cursor: string | null
  selected: Set<number>
  channel: string
}

const filterNames = ['search', 'state', 'source', 'nsfw', 'sent', 'tag'] as const
type FilterName = typeof filterNames[number]

function filtersAt(path: string) {
  const search = new URL(path, location.origin).searchParams
  return Object.fromEntries(filterNames.map((name) => [name, search.get(name) ?? ''])) as Record<FilterName, string>
}

function PostCard({ card, setup, selected, select }: {
  card: Card; setup: boolean; selected: boolean; select: () => void
}) {
  const label = 'Select post #' + card.id
  const inputId = 'post-pick-' + card.id
  return <article data-post-card={card.id} className={'card post' + (card.delivered ? ' pushed' : '')}>
    <input type={setup ? 'radio' : 'checkbox'} id={inputId} className="pick" aria-label={label}
      data-post-pick="" checked={selected} onChange={select} />
    <label className="post-pick-area" htmlFor={inputId} aria-hidden="true" />
    <div className="thumb">
      {card.thumb ? <Thumbnail hash={card.thumb} alt="" /> : <span className="noimg">no media</span>}
      {card.media_count > 1 && <span className="count">{card.media_count}</span>}
      {card.nsfw && <Badge tone="red" className="corner">nsfw</Badge>}
    </div>
    <div className="meta">
      <div className="row">
        <PageLink className="post-link" href={'/posts/' + card.id} aria-label={'Open post #' + card.id}>#{card.id}</PageLink>
        <Badge className={'state-' + card.state}>{card.state}</Badge>
        {card.queue_badges.map((badge) => <Badge key={badge.channel + badge.class} tone={badge.class === 'sent' ? 'green' : badge.class === 'unknown' ? 'amber' : undefined}>
          {badge.label} · {badge.channel}
        </Badge>)}
        <Badge>{card.rating}</Badge>
        {card.submitter_guest && <span className="badge amber" title={'submitted through Telegram by ' + card.submitter + ', who is not an admin'}>via {card.submitter}</span>}
      </div>
      <div className="hint">{card.artist}</div>
      <div className="snippet">{card.snippet}</div>
      {card.tags.length > 0 && <div className="post-card-tags">{card.tags.map((tag) => <PageLink key={tag.name} href={tag.url}>#{tag.name}</PageLink>)}</div>}
      <div className="hint">{card.source} · {card.imported}</div>
    </div>
  </article>
}

export function Posts({ path, snapshots }: { path: string; snapshots: Map<string, GridSnapshot> }) {
  const page = useActions()
  const [filter, setFilter] = useState(() => filtersAt(path))
  const [grid, setGrid] = useState(() => snapshots.get(path))
  const [loadedPath, setLoadedPath] = useState(() => snapshots.has(path) ? path : '')
  const [loading, setLoading] = useState(!snapshots.has(path))
  const [error, setError] = useState('')
  const [batch, setBatch] = useState<'idle' | 'loading' | 'failed'>('idle')
  const [newCount, setNewCount] = useState(0)
  const [revision, change] = useState(0)
  const searchTimer = useRef(0)
  const append = useRef<AbortController | null>(null)
  const sentinel = useRef<HTMLDivElement>(null)
  const searchInput = useRef<HTMLInputElement>(null)
  const latest = useRef(page)
  latest.current = page
  const currentPath = useRef(path)
  currentPath.current = path
  const refresh = () => change((value) => value + 1)
  const previousPath = useRef(path)

  useEffect(() => {
    const keyboard = (event: KeyboardEvent) => {
      if (event.key === '/' && !event.ctrlKey && !event.metaKey && !event.altKey
        && !(event.target instanceof HTMLElement && event.target.matches('input, textarea, select, [contenteditable]'))) {
        event.preventDefault()
        searchInput.current?.focus()
        searchInput.current?.select()
      }
    }
    window.addEventListener('keydown', keyboard)
    return () => window.removeEventListener('keydown', keyboard)
  }, [])

  useEffect(() => {
    window.clearTimeout(searchTimer.current)
    setFilter(filtersAt(path))
    return () => window.clearTimeout(searchTimer.current)
  }, [path])

  useEffect(() => {
    const requested = new AbortController()
    append.current?.abort()
    append.current = null
    setBatch('idle')
    setError('')
    const cached = snapshots.get(path)
    if (previousPath.current !== path) {
      previousPath.current = path
      setGrid(cached)
      setLoadedPath(cached ? path : '')
    }
    setLoading(!cached)
    api<PostsPage>(path, { signal: requested.signal }).then(async (data) => {
      const current = new URL(path, location.origin)
      if (current.searchParams.has('page') && current.searchParams.get('page') !== String(data.page_number)) {
        current.searchParams.set('page', String(data.page_number))
        latest.current.navigate(current.pathname + current.search, true)
        return
      }
      const before = snapshots.get(path)
      const appended: Card[] = []
      let cursor = data.next_cursor
      if (revision > 0 && before?.appended.length) {
        const ids = new Set(data.cards.map((card) => card.id))
        while (cursor && appended.length < before.appended.length && !requested.signal.aborted) {
          const parameters = new URL(path, location.origin).searchParams
          parameters.set('cursor', cursor)
          const result = await api<CardsPage>('/posts/cards?' + parameters, { signal: requested.signal })
          appended.push(...result.cards.filter((card) => {
            if (ids.has(card.id)) return false
            ids.add(card.id)
            return true
          }))
          cursor = result.next_cursor === cursor ? null : result.next_cursor
        }
      }
      if (requested.signal.aborted) return
      const restored = revision === 0 && before?.appended.length
        ? { appended: before.appended, cursor: before.cursor }
        : { appended, cursor }
      const grid: GridSnapshot = {
        page: data, ...restored, selected: before?.selected ?? new Set(),
        channel: before?.channel ?? String(data.default_channel ?? ''),
      }
      snapshots.set(path, grid)
      setGrid(grid)
      setLoadedPath(path)
    }).catch((error) => {
      if (requested.signal.aborted) return
      if (expired(error)) latest.current.expire()
      else setError(friendly(error))
    }).finally(() => { if (!requested.signal.aborted) setLoading(false) })
    return () => { requested.abort(); append.current?.abort(); append.current = null }
  }, [path, revision, snapshots])

  useLayoutEffect(() => {
    if (loadedPath === path && grid) page.onReady?.()
  }, [path, loadedPath, grid, page.onReady])

  function updateGrid(next: GridSnapshot) {
    if (currentPath.current !== path) return
    snapshots.set(path, next)
    setGrid(next)
  }

  async function more() {
    if (!grid?.cursor || append.current || loading || loadedPath !== path) return
    const controller = new AbortController()
    append.current = controller
    setBatch('loading')
    const params = new URL(path, location.origin).searchParams
    params.set('cursor', grid.cursor)
    try {
      const data = await api<CardsPage>('/posts/cards?' + params, { signal: controller.signal })
      if (controller.signal.aborted || path !== currentPath.current) return
      const latest = snapshots.get(path) ?? grid
      const seen = new Set([...latest.page.cards, ...latest.appended].map((card) => card.id))
      const appended = latest.appended.concat(data.cards.filter((card) => {
        if (seen.has(card.id)) return false
        seen.add(card.id)
        return true
      }))
      updateGrid({ ...latest, appended, cursor: data.next_cursor === grid.cursor ? null : data.next_cursor })
      setBatch('idle')
    } catch (error) {
      if (controller.signal.aborted) return
      if (expired(error)) latest.current.expire()
      else setBatch('failed')
    } finally {
      if (append.current === controller) append.current = null
    }
  }

  useEffect(() => {
    if (!sentinel.current || !grid?.cursor || batch !== 'idle' || loading || typeof IntersectionObserver === 'undefined') return
    const observer = new IntersectionObserver(([entry]) => { if (entry.isIntersecting) void more() }, { rootMargin: '320px 0px' })
    observer.observe(sentinel.current)
    return () => observer.disconnect()
  }, [grid, loadedPath, batch, loading])

  function apply(next: Record<FilterName, string>, replace = false) {
    window.clearTimeout(searchTimer.current)
    const params = new URLSearchParams()
    for (const name of filterNames) {
      const value = next[name].trim()
      if (value) params.set(name, value)
    }
    if (new URL(path, location.origin).searchParams.get('setup') === 'true') params.set('setup', 'true')
    const query = params.toString()
    page.navigate('/posts' + (query ? '?' + query : ''), replace)
  }

  function edit(name: FilterName, value: string, immediately = false) {
    const next = { ...filter, [name]: value }
    setFilter(next)
    window.clearTimeout(searchTimer.current)
    if (immediately) apply(next)
    else searchTimer.current = window.setTimeout(() => apply(next, true), 300)
  }

  const data = grid?.page
  const cards = grid ? [...grid.page.cards, ...grid.appended] : []
  const selected = grid?.selected ?? new Set<number>()
  const setup = data?.setup ?? false
  const topId = data?.cards[0]?.id
  const firstPage = data?.page_number === 1

  // Light probe so an import from the bot, a watch, or another tab is visible.
  // Never prepend automatically: it would move cards under the pointer.
  useEffect(() => {
    setNewCount(0)
    if (!topId || !firstPage || setup) return
    const params = new URL(path, location.origin).searchParams
    for (const key of ['page', 'cursor', 'flash']) params.delete(key)
    params.set('after', String(topId))
    const controller = new AbortController()
    let cancelled = false
    const check = async () => {
      if (document.hidden) return
      try {
        const result = await api<{ count: number }>('/posts/new?' + params, { signal: controller.signal })
        if (!cancelled) setNewCount(result.count)
      } catch (error) {
        if (!cancelled && expired(error)) latest.current.expire()
        // A failed probe is not worth an error banner; the next tick retries.
      }
    }
    const timer = window.setInterval(() => void check(), 20_000)
    const onVisible = () => { if (!document.hidden) void check() }
    document.addEventListener('visibilitychange', onVisible)
    window.addEventListener('focus', onVisible)
    void check()
    return () => {
      cancelled = true
      controller.abort()
      window.clearInterval(timer)
      document.removeEventListener('visibilitychange', onVisible)
      window.removeEventListener('focus', onVisible)
    }
  }, [path, topId, firstPage, setup])

  function showNewPosts() {
    const params = new URL(path, location.origin).searchParams
    for (const key of ['page', 'cursor', 'flash']) params.delete(key)
    const query = params.toString()
    page.navigate('/posts' + (query ? '?' + query : ''))
    refresh()
    setNewCount(0)
    window.scrollTo({ top: 0, behavior: 'smooth' })
  }

  const fields = {
    back: data?.grid_url, channel: grid?.channel,
    ...(setup ? { post_id: selected.values().next().value } : Object.fromEntries([...selected].map((id) => ['sel_' + id, 'on']))),
  }
  const setSelected = (selection: Set<number>) => { if (grid) updateGrid({ ...grid, selected: selection }) }

  return <Actions.Provider value={{ ...page, refresh }}>
    {loading && <div className="navigation-progress" role="status" aria-label="Loading page" />}
    {error && <p className="action-error" role="alert">{error} <button onClick={refresh}>Retry</button></p>}
    {setup && <section className="panel">
      <h2>Choose your first post</h2>
      <p>Select one post and its Telegram channel. The post stays on hold until you confirm its preview.</p>
      <PageLink href="/">Back to setup</PageLink>
    </section>}
    {newCount > 0 && <div className="new-posts" role="status">
      <span><b>{newCount}</b> new post{newCount === 1 ? '' : 's'} {newCount === 1 ? 'matches' : 'match'} this view.</span>
      <span className="new-posts-actions">
        <button type="button" className="small" onClick={showNewPosts}>Show new posts</button>
        <button type="button" className="small ghost" onClick={() => setNewCount(0)}>Dismiss</button>
      </span>
    </div>}
    <form className="filters panel" data-post-filters="" onSubmit={(event) => { event.preventDefault(); apply(filter) }}>
      <input ref={searchInput} name="search" aria-label="Search posts" placeholder="search title / description / id"
        value={filter.search} onChange={(event) => edit('search', event.target.value)} />
      <select name="state" aria-label="Post state" value={filter.state} onChange={(event) => edit('state', event.target.value, true)}>
        <option value="">not rejected</option><option value="all">any state</option>
        {data?.states.map((state) => <option key={state} value={state}>{state}</option>)}
      </select>
      <select name="source" aria-label="Source" value={filter.source} onChange={(event) => edit('source', event.target.value, true)}>
        <option value="">any source</option>
        {data?.sources.map((source) => <option key={source} value={source}>{source}</option>)}
      </select>
      <select name="nsfw" aria-label="Sensitivity" value={filter.nsfw} onChange={(event) => edit('nsfw', event.target.value, true)}>
        <option value="">sfw + nsfw</option><option value="yes">nsfw only</option><option value="no">sfw only</option>
      </select>
      <select name="sent" aria-label="Delivery" value={filter.sent} onChange={(event) => edit('sent', event.target.value, true)}>
        <option value="">hide sent</option><option value="not_queued">Not queued</option>
        <option value="all">with sent</option><option value="yes">sent only</option>
      </select>
      <TagAutocomplete name="tag" aria-label="Tag filter" placeholder="Search tags" value={filter.tag}
        onChange={(value) => edit('tag', value)} onPick={(tag) => edit('tag', tag, true)} />
      {data && <span className="hint">{data.total} post(s)</span>}
      <PageLink className="hint post-duplicates-link" href="/duplicates">review duplicates</PageLink>
    </form>
    {data && data.top_tags.length > 0 && <details id="posts-tag-menu" className="panel" key={data.filters.tag ?? ''} open={Boolean(data.filters.tag)}>
      <summary>Tags{data.filters.tag && ' · #' + data.filters.tag}</summary>
      <div className="tagcloud">
        {data.top_tags.map((tag) => <PageLink key={tag.tag} href={tag.url} aria-current={tag.active || undefined} className={tag.active ? 'active' : ''}>
          #{tag.tag} <span className="hint">{tag.count}</span>
        </PageLink>)}
        {data.filters.tag && <PageLink href={data.clear_tag_url}>Clear tag</PageLink>}
      </div>
    </details>}
    {grid && data && <ActionForm action={setup ? '/setup/queue' : '/posts'} fields={fields} id="bulk" data-grid-page={data.page_number}>
      <div className="bulkbar panel">
        {cards.length > 0 && !setup && <div className="bulk-selection">
          <button type="button" className="small ghost" disabled={selected.size === cards.length} onClick={() => setSelected(new Set(cards.map((card) => card.id)))}>Select all loaded posts</button>
          <button type="button" className="small ghost" disabled={selected.size === 0} onClick={() => setSelected(new Set())}>Clear</button>
          <span className="hint" aria-live="polite">{selected.size} selected</span>
        </div>}
        {!setup && <>
          <span className="hint">Select posts, then:</span>
          <button name="action" value="approve" className="small">Approve</button>
          <button name="action" value="reject" className="small ghost">Reject</button>
          <button name="action" value="nsfw_yes" className="small ghost">Mark NSFW</button>
          <button name="action" value="nsfw_no" className="small ghost">Mark SFW</button>
          <button name="action" value="nsfw_auto" className="small ghost">NSFW auto</button>
        </>}
        <ChannelSelect channels={data.channels} aria-label="Destination channel" emptyLabel="choose channel…"
          value={grid.channel} onChange={(event) => updateGrid({ ...grid, channel: event.target.value })} />
        <button name="action" value="queue" className="small">{setup ? 'Hold for preview' : 'Queue'}</button>
        {!setup && <button name="action" value="delete" className="small danger" data-confirm="Delete the selected posts and their media links?">Delete</button>}
      </div>
      {cards.length ? <div className="grid">
        {cards.map((card) => <PostCard key={card.id} card={card} setup={setup} selected={selected.has(card.id)} select={() => {
          const next = new Set(setup ? [] : selected)
          if (selected.has(card.id) && !setup) next.delete(card.id)
          else next.add(card.id)
          setSelected(next)
        }} />)}
      </div> : <p className="hint">Nothing here. <PageLink href="/import">Import some artwork</PageLink>.</p>}
    </ActionForm>}
    {grid?.cursor && <div ref={sentinel} data-posts-more="" className="pager">
      <button type="button" disabled={batch === 'loading'} onClick={() => void more()}>Load more</button>
      <span role="status">{batch === 'loading' ? 'Loading more posts…' : batch === 'failed' ? 'Could not load more posts.' : ''}</span>
    </div>}
  </Actions.Provider>
}
