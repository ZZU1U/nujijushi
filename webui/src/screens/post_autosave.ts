import { useEffect, useLayoutEffect, useReducer, useRef } from 'react'
import { action, expired, friendly, type ActionFields, type PostDetailPage } from '../api'
import { useActions } from '../ui'

type Field = 'title' | 'description' | 'notes' | 'rating' | 'nsfw' | 'state'
type Fields = Partial<Record<Field, string>>
type TagEdit =
  | { action: 'add'; tags: string }
  | { action: 'remove_tag' | 'undo'; tag_id: number }
type Order = { ids: number[]; version: string }
type Edit =
  | { kind: 'fields'; fields: Fields }
  | { kind: 'tags'; fields: TagEdit }
  | { kind: 'order'; order: Order }
type Pending = { edit: Edit; started: boolean; written: boolean }

function serverField(data: PostDetailPage, name: Field) {
  return (name === 'nsfw' ? data.post.nsfw_override : data.post[name]) ?? ''
}

function sameOrder(data: PostDetailPage, ids: number[]) {
  return data.media.length === ids.length && data.media.every((media, index) => media.id === ids[index])
}

export function usePostAutosave(data: PostDetailPage, reload: (quiet?: boolean) => Promise<PostDetailPage | undefined>) {
  const { csrf, expire, guardNavigation } = useActions()
  const server = useRef(data)
  const latest = useRef({ reload, expire })
  latest.current = { reload, expire }
  const [, render] = useReducer((count: number) => count + 1, 0)
  const session = useRef({
    queue: [] as Pending[], buffered: {} as Fields, task: null as Promise<boolean> | null,
    timer: 0, failure: '', orderWarning: '', saved: false, mounted: true,
  }).current
  const endpoint = '/posts/' + data.post.id

  useLayoutEffect(() => { server.current = data }, [data])

  function enqueue(edit: Edit) {
    const tail = session.queue.at(-1)
    if (tail && !tail.started && edit.kind === 'fields' && tail.edit.kind === 'fields') {
      Object.assign(tail.edit.fields, edit.fields)
    } else if (tail && !tail.started && edit.kind === 'order' && tail.edit.kind === 'order') {
      tail.edit.order.ids = edit.order.ids
    } else {
      session.queue.push({ edit, started: false, written: false })
    }
  }

  function flush() {
    window.clearTimeout(session.timer)
    if (Object.keys(session.buffered).length) {
      enqueue({ kind: 'fields', fields: session.buffered })
      session.buffered = {}
    }
    if (session.task) return session.task
    if (session.failure || !session.mounted) return Promise.resolve(false)
    if (session.queue.length === 0) return Promise.resolve(true)
    session.task = drain().finally(() => {
      session.task = null
      if (session.mounted) render()
    })
    render()
    return session.task
  }

  async function drain() {
    let conflict = false
    while (session.mounted && session.queue.length) {
      const entry = session.queue[0]
      const { edit } = entry
      entry.started = true
      let snapshot: PostDetailPage | undefined
      try {
        if (!entry.written) {
          let path = endpoint
          let fields: ActionFields
          if (edit.kind === 'order') {
            path += '/media'
            fields = { action: 'reorder', gallery_version: edit.order.version, media_ids: edit.order.ids.join(',') }
          } else {
            if (edit.kind === 'tags') path += '/tags'
            fields = edit.fields
          }
          await action(path, fields, csrf)
          entry.written = true
        }
        if (session.mounted) snapshot = await latest.current.reload(true)
      } catch (error) {
        if (!session.mounted) return false
        if (expired(error)) {
          latest.current.expire()
          return false
        }
        if (edit.kind === 'order' && !entry.written) {
          // The server may have saved the order before the connection dropped.
          snapshot = await latest.current.reload(true).catch(() => undefined)
        }
        if (!snapshot || edit.kind !== 'order'
          || (snapshot.gallery_version === edit.order.version && !sameOrder(snapshot, edit.order.ids))) {
          session.failure = entry.written ? 'Changes saved. Could not refresh the post.' : friendly(error)
          return false
        }
      }
      if (!session.mounted || !snapshot) return false
      server.current = snapshot
      session.queue.shift()
      if (edit.kind === 'order') {
        if (!sameOrder(snapshot, edit.order.ids)) {
          session.queue = session.queue.filter(({ edit }) => edit.kind !== 'order')
          session.orderWarning = 'The gallery changed. Move the artwork again.'
          conflict = true
        } else {
          for (const next of session.queue) {
            if (next.edit.kind === 'order' && next.edit.order.version === edit.order.version) {
              next.edit.order.version = snapshot.gallery_version
            }
          }
        }
      }
      session.saved = true
      render()
    }
    return !conflict
  }

  const flushRef = useRef(flush)
  flushRef.current = flush
  useEffect(() => {
    session.mounted = true
    const unregister = guardNavigation(() => flushRef.current())
    function beforeUnload(event: BeforeUnloadEvent) {
      if (!session.queue.length && !Object.keys(session.buffered).length) return
      event.preventDefault()
      event.returnValue = ''
    }
    window.addEventListener('beforeunload', beforeUnload)
    return () => {
      session.mounted = false
      window.clearTimeout(session.timer)
      unregister()
      window.removeEventListener('beforeunload', beforeUnload)
    }
  }, [guardNavigation, session])

  const drafts: Fields = {}
  const tagEdits: TagEdit[] = []
  let order: Order | undefined
  for (const { edit } of session.queue) {
    if (edit.kind === 'fields') Object.assign(drafts, edit.fields)
    if (edit.kind === 'order') order = edit.order
    if (edit.kind === 'tags') tagEdits.push(edit.fields)
  }
  const staged = (name: Field) => drafts[name] ?? serverField(data, name)
  const field = (name: Field) => session.buffered[name] ?? staged(name)

  function changeField(name: Field, value: string, immediate = false) {
    if (value === field(name)) return
    if (value === staged(name)) delete session.buffered[name]
    else session.buffered[name] = value
    window.clearTimeout(session.timer)
    if (immediate) void flush()
    else session.timer = window.setTimeout(flush, 500)
    render()
  }

  function changeTags(fields: TagEdit) {
    enqueue({ kind: 'tags', fields })
    void flush()
    render()
  }

  function reorder(ids: number[]) {
    session.orderWarning = ''
    enqueue({ kind: 'order', order: { ids, version: order?.version ?? server.current.gallery_version } })
    void flush()
    render()
  }

  function retry() {
    session.failure = ''
    void flush()
    render()
  }

  function discard() {
    session.queue = []
    session.buffered = {}
    session.failure = ''
    window.clearTimeout(session.timer)
    void latest.current.reload().catch(() => undefined)
    render()
  }

  const pending = session.queue.length > 0 || Object.keys(session.buffered).length > 0
  const status = pending ? (session.task ? 'Saving…' : 'Changes pending') : session.saved ? 'All changes saved' : ''
  return {
    field, changeField, changeTags, tagEdits, order: order?.ids, reorder, flush, retry, discard,
    status, failure: session.failure, failedAfterSave: session.queue[0]?.written ?? false,
    orderWarning: session.orderWarning,
  }
}

export type PostAutosave = ReturnType<typeof usePostAutosave>
