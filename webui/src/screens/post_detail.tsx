import { useRef, useState } from 'react'
import { ActionForm, CaptionPreview, capitalize, PageLink } from '../ui'
import { Badge } from '../components/Badge'
import { ChannelSelect } from '../components/ChannelSelect'
import type { PostDetailPage } from '../api'
import { PostGallery } from './post_gallery'
import { PostTags } from './post_tags'
import { usePostAutosave, type PostAutosave } from './post_autosave'

export function PostDetail({ data, reload }: { data: PostDetailPage; reload: (quiet?: boolean) => Promise<PostDetailPage | undefined> }) {
  const { post, outgoing_count, captions, default_channel, channels } = data
  const editor = usePostAutosave(data, reload)
  const endpoint = '/posts/' + post.id
  return <>
    <header className="post-heading">
      <div>
        <PageLink href="/posts" className="hint" data-return-to-posts="">← Posts</PageLink>
        <h1>{post.title || 'Post #' + post.id}</h1>
        <div className="hint">#{post.id} · {post.source} / {post.source_id}</div>
      </div>
      <div className="post-heading-actions">
        <span className="hint" role="status">{editor.failure ? '' : editor.status}</span>
        <PostStateControl data={data} editor={editor} />
        <PageLink className="button small ghost post-delete-link" href={endpoint + '/delete'}>Delete post…</PageLink>
      </div>
    </header>
    {editor.failure && <div className="post-save-error" role="alert">
      <span>{editor.failure}</span>
      <button type="button" className="small ghost" onClick={editor.retry}>Retry</button>
      {!editor.failedAfterSave && <button type="button" className="small ghost" onClick={editor.discard}>Discard pending edits</button>}
    </div>}
    {editor.orderWarning && <p className="post-save-error" role="alert">{editor.orderWarning}</p>}

    <div className="detail">
      <div><PostGallery data={data} editor={editor} /></div>
      <aside>
        {channels.length > 0 && <section className="panel">
          <h2>Add to queue</h2>
          <ActionForm action={endpoint + '/queue'} className="post-buttons" beforeSubmit={editor.flush}>
            <label>Destination
              <ChannelSelect channels={channels} defaultValue={default_channel ?? ''} name="channel" aria-label="Destination" required />
            </label>
            <button type="submit" className="small" disabled={!outgoing_count}>
              {data.guest_review && editor.field('state') === 'new' ? 'Approve and queue' : 'Add to queue'}
            </button>
          </ActionForm>
        </section>}
        <PostFields data={data} editor={editor} />
        <PostTags data={data} editor={editor} />
        <section className="panel post-captions">
          <h2>Caption previews</h2>
          {captions.length ? captions.map((caption, index) => <details key={caption.tg_id} className="caption-box" open={index === 0}>
            <summary>
              {caption.channel}
              <Badge tone={caption.allowed ? 'green' : 'red'}>{caption.allowed ? 'sendable' : 'vetoed'}</Badge>
              {caption.spoiler && <Badge>spoiler</Badge>}
              {caption.eta && <span className="badge accent">est. {caption.eta} · in {caption.eta_in}</span>}
            </summary>
            <CaptionPreview caption={caption.caption} />
            <p className="hint">{caption.length} chars · {caption.messages} message(s) · {caption.verdict}</p>
          </details>) : <p className="hint"><PageLink href="/channels#add-channel">Add a channel</PageLink> to preview a caption.</p>}
        </section>
      </aside>
    </div>
  </>
}

function PostFields({ data, editor }: { data: PostDetailPage; editor: PostAutosave }) {
  const { ratings } = data
  return <section className="panel post-fields">
    <h2>Post details</h2>
    <label>Title
      <input name="title" value={editor.field('title')} onChange={(event) => editor.changeField('title', event.target.value)} onBlur={editor.flush} />
    </label>
    <label>Description
      <textarea name="description" rows={5} value={editor.field('description')} onChange={(event) => editor.changeField('description', event.target.value)} onBlur={editor.flush} />
    </label>
    <div className="post-review-fields">
      <label>Rating
        <select name="rating" value={editor.field('rating')} onChange={(event) => editor.changeField('rating', event.target.value, true)}>
          {ratings.map((rating) => <option value={rating} key={rating}>{rating}</option>)}
        </select>
      </label>
      <label>Sensitivity
        <select name="nsfw" value={editor.field('nsfw')} onChange={(event) => editor.changeField('nsfw', event.target.value, true)}>
          <option value="auto">Automatic</option>
          <option value="yes">Force NSFW</option>
          <option value="no">Force SFW</option>
        </select>
      </label>
    </div>
    <label>Notes
      <input name="notes" value={editor.field('notes')} onChange={(event) => editor.changeField('notes', event.target.value)} onBlur={editor.flush} />
    </label>
  </section>
}

function PostStateControl({ data, editor }: { data: PostDetailPage; editor: PostAutosave }) {
  const menu = useRef<HTMLDetailsElement>(null)
  const [reviewing, setReviewing] = useState(false)
  const current = editor.field('state')
  const options = data.guest_review ? data.states.filter((state) => state === 'archived') : data.states

  function close() {
    if (!menu.current) return
    menu.current.open = false
    menu.current.querySelector('summary')?.focus()
  }

  return <details ref={menu} className="post-state-menu" onKeyDown={(event) => {
    if (event.key === 'Escape' && menu.current?.open) close()
  }}>
    <summary className={'badge post-state-toggle state-' + current} aria-label={'Post state: ' + capitalize(current)}>
      <span>State</span><strong>{capitalize(current)}</strong>
    </summary>
    <div className="post-state-options">
      {data.guest_review && <GuestReview postId={data.post.id} flush={editor.flush} onSaved={close}
        busy={reviewing} onBusyChange={setReviewing} />}
      {options.map((state) => <button key={state} type="button" className="small ghost" disabled={state === current || reviewing}
        onClick={() => { editor.changeField('state', state, true); close() }}>{capitalize(state)}</button>)}
    </div>
  </details>
}

function GuestReview({ postId, flush, onSaved, busy, onBusyChange }: {
  postId: number; flush: () => Promise<boolean>; onSaved: () => void; busy: boolean; onBusyChange: (busy: boolean) => void
}) {
  const [points, setPoints] = useState('')
  const endpoint = '/posts/' + postId
  return <fieldset className="guest-review" disabled={busy}>
    <legend>Guest submission</legend>
    <label>Approval points <span className="hint">(optional)</span>
      <input type="number" min="5" max="20" step="1" placeholder="10" form="approve-guest"
        value={points} onChange={(event) => setPoints(event.target.value)} />
    </label>
    <div className="post-buttons">
      <ActionForm id="approve-guest" action={endpoint} fields={{ state: 'approved', approval_points: points }}
        onSaved={() => { setPoints(''); onSaved() }} beforeSubmit={flush} onBusyChange={onBusyChange} feedback="inline">
        <button type="submit" className="small">Approve</button>
      </ActionForm>
      <ActionForm action={endpoint} fields={{ state: 'rejected' }} onSaved={onSaved} beforeSubmit={flush} onBusyChange={onBusyChange} feedback="inline">
        <button type="submit" className="small ghost">Reject</button>
      </ActionForm>
    </div>
  </fieldset>
}
