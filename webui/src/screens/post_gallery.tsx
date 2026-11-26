import { useEffect, useRef, useState, type DragEvent, type ReactNode } from 'react'
import type { ActionFields, Media, PostDetailPage } from '../api'
import { Thumbnail, thumbnailUrl } from '../components/Thumbnail'
import { Badge } from '../components/Badge'
import { useThumbnailsOnly } from '../preferences'
import { appUrl } from '../paths'
import { ActionForm, CompressionOptions, JobProgress, externalUrl } from '../ui'
import type { PostAutosave } from './post_autosave'

export function PostGallery({ data, editor }: { data: PostDetailPage; editor: PostAutosave }) {
  const { post, media, media_total, outgoing_count, merge_job, submitter, gallery_version, deliveries } = data
  const [selected, setSelected] = useState(new Set<number>())
  const [thumbnailsOnly] = useThumbnailsOnly()
  const [open, setOpen] = useState<number>()
  const [busy, setBusy] = useState(false)
  const [dragTarget, setDragTarget] = useState<number>()
  const dragging = useRef<number>(undefined)
  const canonical = new Map(media.map((item) => [item.id, item]))
  const ordered = editor.order?.length === media.length && editor.order.every((id) => canonical.has(id))
    ? editor.order.map((id) => canonical.get(id)!) : media
  const endpoint = '/posts/' + post.id + '/media'
  const selectedItems = ordered.filter((item) => item.can_select && selected.has(item.id))
  const editing = busy || Boolean(merge_job?.active)
  const commandsDisabled = editing || Boolean(editor.order)

  function move(source: number, target: number) {
    const ids = ordered.map((item) => item.id)
    const index = ids.indexOf(source)
    if (editing || target < 0 || target >= ids.length || index < 0 || index === target) return
    ids.splice(index, 1)
    ids.splice(target, 0, source)
    editor.reorder(ids)
  }

  function startDrag(event: DragEvent, id: number) {
    if (editing || (event.target instanceof Element && event.target.closest('[data-media-options]'))) {
      event.preventDefault()
      return
    }
    setOpen(undefined)
    dragging.current = id
    event.dataTransfer.effectAllowed = 'move'
    event.dataTransfer.setData('text/plain', String(id))
  }

  function endDrag() {
    dragging.current = undefined
    setDragTarget(undefined)
  }

  function toggleSelection(id: number) {
    setSelected((previous) => {
      const next = new Set(previous)
      if (!next.delete(id)) next.add(id)
      return next
    })
  }

  function command(name: string, label: string, fields: ActionFields, options?: ReactNode) {
    return <ActionForm action={endpoint} fields={{ ...fields, gallery_version }} onBusyChange={setBusy} feedback="inline">
      {options}
      <button type="submit" name="action" value={name} className="small ghost" disabled={commandsDisabled}>{label}</button>
    </ActionForm>
  }

  return <section className="panel" id="media-tools">
    <div className="post-panel-heading">
      <h2>Artwork <span className="hint">{media.length} {media.length === 1 ? 'item' : 'items'} · {media_total}</span></h2>
      {media.some((item) => item.can_compress) && <details className="gallery-batch">
        <summary>Compress all images</summary>
        {command('compress_all', 'Compress all', {}, <CompressionOptions />)}
      </details>}
    </div>
    {deliveries.length > 0 && <div className="post-deliveries">
      <span className="hint">Posted on Telegram</span>
      {deliveries.map(({ channel_id, channel, url }) => <a key={channel_id} href={url} target="_blank" rel="noopener noreferrer"
        aria-label={'View post in ' + channel + ' on Telegram'}>{channel}<span aria-hidden="true"> ↗</span></a>)}
    </div>}
    {merge_job && <div className="hint post-job-status"><JobProgress initial={merge_job} /></div>}
    {outgoing_count === 0 && media.length > 0 && <p className="hint">No outgoing media</p>}
    {data.can_merge && selectedItems.length > 0 && <ActionForm
      action={endpoint} id="merge-gallery" className="gallery-merge" feedback="inline" onBusyChange={setBusy}
      fields={{ gallery_version, ...Object.fromEntries(selectedItems.map(({ id }) => ['sel_' + id, 'on'])) }}
      onJobDone={() => setSelected(new Set())}>
      <span>{selectedItems.length} selected</span>
      <button type="submit" name="action" value="merge" className="small" disabled={commandsDisabled || selectedItems.length < 2}>Merge side by side</button>
      <button type="button" className="small ghost" onClick={() => setSelected(new Set())} disabled={editing}>Clear selection</button>
      <details className="post-inline-options">
        <summary>Merge output options</summary>
        <CompressionOptions merge />
      </details>
    </ActionForm>}
    {ordered.length === 0 ? <p className="hint">No media archived for this post.</p> :
      <div className="media-grid">
        {ordered.map((item, index) => <figure
          key={item.id}
          id={'media-' + (index + 1)}
          data-media-id={item.id}
          data-hidden={item.hidden || undefined}
          data-selected={selected.has(item.id) || undefined}
          data-drop-target={dragTarget === item.id || undefined}
          draggable={ordered.length > 1 && !editing}
          onDragStart={(event) => startDrag(event, item.id)}
          onDragOver={(event) => {
            if (dragging.current == null || editing) return
            event.preventDefault()
            event.dataTransfer.dropEffect = 'move'
            setDragTarget(item.id)
          }}
          onDrop={(event) => {
            if (dragging.current == null) return
            event.preventDefault()
            move(dragging.current, index)
            endDrag()
          }}
          onDragEnd={endDrag}>
          <div className="artwork-tile">
            {thumbnailsOnly
              ? <a href={appUrl('/media/' + item.hash)} target="_blank" rel="noopener noreferrer" draggable={false}
                aria-label={`Open ${item.is_video ? 'video' : 'image'} ${index + 1} at full size`}>
                <Thumbnail hash={item.thumb} alt="" draggable={false} width={item.width ?? undefined} height={item.height ?? undefined} />
              </a>
              : item.is_video
              ? <video controls preload="none" poster={thumbnailUrl(item.thumb)} src={appUrl('/media/' + item.hash)} />
              : <a href={appUrl('/media/' + item.hash)} target="_blank" rel="noopener noreferrer" draggable={false} aria-label={`Open image ${index + 1} at full size`}>
                <img loading="lazy" decoding="async" src={appUrl('/media/' + item.hash)} alt="" draggable={false}
                  width={item.width ?? undefined} height={item.height ?? undefined} />
              </a>}
            <MediaOptions item={item} position={index + 1} open={open === item.id}
              setOpen={(show) => setOpen((current) => show ? item.id : current === item.id ? undefined : current)}>
              {ordered.length > 1 && <>
                <button type="button" className="small ghost" disabled={editing || index === 0} onClick={() => move(item.id, index - 1)}>Move earlier</button>
                <button type="button" className="small ghost" disabled={editing || index === ordered.length - 1} onClick={() => move(item.id, index + 1)}>Move later</button>
              </>}
              {item.can_select && <label className="check">
                <input type="checkbox" checked={selected.has(item.id)} onChange={() => toggleSelection(item.id)} disabled={editing}
                  aria-label={`Include image ${index + 1} in merge`} />
                Include in merge
              </label>}
              {item.can_compress && <details className="post-inline-options">
                <summary>Compress image</summary>
                {command('compress', 'Compress', { media_id: item.id }, <CompressionOptions />)}
              </details>}
              {command(item.hidden ? 'restore' : 'hide', item.hidden ? 'Restore' : 'Hide from outgoing copies', { media_id: item.id })}
              {item.compressed
                ? command('undo_compression', 'Undo compression', { media_id: item.id })
                : item.undo_id != null && command('undo', 'Undo merge', { merge_id: item.undo_id })}
            </MediaOptions>
          </div>
          <figcaption className="hint">{item.ext} {item.width}×{item.height} · {item.kib} KiB
            {item.hidden && <span className="visually-hidden">Hidden from outgoing copies</span>}
          </figcaption>
          {item.over_photo_limit && <p className="hint">Exceeds the photo upload limits</p>}
        </figure>)}
      </div>}
    <dl className="facts">
      {post.artist && <><dt>Artist</dt><dd>{post.artist_url
        ? <a href={externalUrl(post.artist_url)} target="_blank">{post.artist}</a> : post.artist}</dd></>}
      {post.url && <><dt>Source</dt><dd><a href={externalUrl(post.url)} target="_blank">Open original post</a></dd></>}
      <dt>Rating</dt><dd>{post.rating}{' '}<Badge tone={post.nsfw ? 'red' : 'green'}>{post.nsfw ? 'nsfw' : 'sfw'}</Badge>
        {' '}<span className="hint">override: {post.nsfw_override}</span></dd>
      <dt>Imported</dt><dd className="hint">{post.imported}{submitter && <> · via telegram {submitter.label}
        {!submitter.admin && <> <Badge tone="amber">guest</Badge></>}</>}</dd>
    </dl>
  </section>
}

function MediaOptions({ item, position, open, setOpen, children }: {
  item: Media; position: number; open: boolean; setOpen: (open: boolean) => void; children: ReactNode
}) {
  const container = useRef<HTMLDivElement>(null)
  const button = useRef<HTMLButtonElement>(null)
  useEffect(() => {
    if (!open) return
    function outside(event: PointerEvent) {
      if (event.target instanceof Node && !container.current?.contains(event.target)) setOpen(false)
    }
    document.addEventListener('pointerdown', outside)
    return () => document.removeEventListener('pointerdown', outside)
  }, [open, setOpen])
  const label = (item.is_video ? 'Video ' : 'Image ') + position + ' options'
  return <div ref={container} data-media-options="" className="artwork-options"
    onKeyDown={(event) => {
      if (event.key !== 'Escape' || !open) return
      event.stopPropagation()
      setOpen(false)
      button.current?.focus()
    }}>
    <button ref={button} type="button" className="artwork-options-button" aria-label={label}
      aria-expanded={open} aria-controls={'artwork-options-' + item.id} onClick={() => setOpen(!open)}>
      <span aria-hidden="true">⋯</span>
    </button>
    <div id={'artwork-options-' + item.id} className="artwork-popover" hidden={!open} role="group" aria-label={label}>
      {children}
    </div>
  </div>
}
