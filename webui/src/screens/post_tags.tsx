import { useState, type ClipboardEvent } from 'react'
import { ActionForm, JobProgress, TextField, PageLink } from '../ui'
import { TagAutocomplete } from '../components/TagAutocomplete'
import type { PostDetailPage, TagSuggestions } from '../api'
import type { PostAutosave } from './post_autosave'

export function PostTags({ data, editor }: { data: PostDetailPage; editor: PostAutosave }) {
  const { post, tag_review: review, can_suggest_tags: enabled } = data
  const [draft, setDraft] = useState('')
  const endpoint = '/posts/' + post.id + '/tag-suggestions'
  const pending = editor.tagEdits
  const additions = [...new Set(pending.flatMap((edit) => edit.action === 'add'
    ? edit.tags.split(/[,\n]/).map((tag) => tag.trim().replace(/^#+/, '')).filter(Boolean) : []))]
  const added = new Set([...post.tags.map(({ name }) => name), ...additions])
  const clearing = pending.some((edit) => edit.action === 'remove_all')
  const changing = (id: number) => clearing || pending.some((edit) => 'tag_id' in edit && edit.tag_id === id)

  function add(tags: string) {
    if (tags.split(/[,\n]/).some((tag) => tag.trim())) editor.changeTags({ action: 'add', tags })
  }

  function paste(event: ClipboardEvent<HTMLInputElement>) {
    const value = event.clipboardData.getData('text')
    if (!/[,\n]/.test(value)) return
    event.preventDefault()
    const input = event.currentTarget
    add(input.value.slice(0, input.selectionStart ?? 0) + value + input.value.slice(input.selectionEnd ?? input.value.length))
    setDraft('')
  }

  return <section className="panel" id="post-tags">
    <div className="post-panel-heading">
      <h2>Tags</h2>
      <div className="post-buttons">
        {added.size > 0 && <button type="button" className="small ghost" disabled={clearing} onClick={() => {
          if (window.confirm('Remove all tags from this post?')) editor.changeTags({ action: 'remove_all' })
        }}>Remove all tags</button>}
        {enabled && <ActionForm action={endpoint} beforeSubmit={editor.flush} feedback="inline">
          <button type="submit" name="action" value="generate" className="small ghost" disabled={review?.job.active}>
            {review ? 'Suggest again' : 'Suggest tags'}
          </button>
        </ActionForm>}
      </div>
    </div>
    {(post.tags.length > 0 || additions.length > 0) && <ul className="post-chips" aria-label="Post tags">
      {post.tags.map((tag) => <li key={tag.id} className="post-chip" data-pending={changing(tag.id) || undefined}
        title={tag.source && tag.manual ? 'Source and manual' : tag.source ? 'Source' : 'Manual'}>
        <PageLink href={'/posts?tag=' + encodeURIComponent(tag.name)}>#{tag.name}</PageLink>
        <button type="button" className="chip-action" disabled={changing(tag.id)} aria-label={'Remove tag ' + tag.name}
          onClick={() => editor.changeTags({ action: 'remove_tag', tag_id: tag.id })}>×</button>
      </li>)}
      {additions.filter((name) => !post.tags.some((tag) => tag.name === name)).map((name) =>
        <li className="post-chip" data-pending="" key={name}>#{name}</li>)}
    </ul>}
    <form className="add-tag" onSubmit={(event) => { event.preventDefault(); add(draft); setDraft('') }}>
      <TagAutocomplete aria-label="Add a tag" placeholder="Add a tag…" value={draft} postId={post.id} exclude={added}
        onPick={(tag) => { add(tag); setDraft('') }} onPaste={paste} enterKeyHint="done" onChange={(value) => {
        const lastComma = value.lastIndexOf(',')
        if (lastComma !== -1) add(value.slice(0, lastComma))
        setDraft(value.slice(lastComma + 1))
      }} />
      <button className="small ghost" type="submit" disabled={!draft.trim()}>Add</button>
    </form>
    {post.tag_removals.length > 0 && <details className="removed-tags">
      <summary>Removed source tags <span className="hint">({post.tag_removals.length})</span></summary>
      <ul className="post-chips">
        {post.tag_removals.map((tag) => <li className="post-chip" key={tag.id}>
          <span>#{tag.name}</span>
          <button className="chip-action" type="button" disabled={changing(tag.id)} title="Undo source removal"
            aria-label={'Restore source tag ' + tag.name}
            onClick={() => editor.changeTags({ action: 'undo', tag_id: tag.id })}>↶</button>
        </li>)}
      </ul>
    </details>}
    {review && <TagReview review={review} endpoint={endpoint} editor={editor} />}
  </section>
}

function TagReview({ review, endpoint, editor }: { review: TagSuggestions; endpoint: string; editor: PostAutosave }) {
  const suggestions = !editor.order && !review.stale ? review.suggestions : []
  return <div className="tag-review">
    <div className="post-panel-heading">
      <JobProgress initial={review.job} />
      {suggestions.length > 0 && <ActionForm action={endpoint} beforeSubmit={editor.flush} fields={{ job_id: review.job.id }}>
        <button type="submit" name="action" value="discard_all" className="small ghost">Discard all suggestions</button>
      </ActionForm>}
    </div>
    {review.stale && <p className="hint">The artwork or source metadata changed. Suggest tags again.</p>}
    {suggestions.map((suggestion) => <ActionForm key={suggestion.id} action={endpoint}
      beforeSubmit={editor.flush} className="post-tool-row" fields={{ job_id: review.job.id, suggestion_id: suggestion.id }}>
      <label>{suggestion.kind}
        <TextField name="tag" defaultValue={suggestion.tag} aria-label={'Edit suggested ' + suggestion.kind + ' tag ' + suggestion.tag} />
      </label>
      <p className="hint">
        {suggestion.wiki_url ? <a href={suggestion.wiki_url}>{suggestion.evidence}</a> : suggestion.evidence}
        {suggestion.character_tag_id && <> · ID {suggestion.character_tag_id}</>}
        {suggestion.images.map((position) => <span key={position}> · <a href={'#media-' + position}>Image {position}</a></span>)}
      </p>
      <div className="post-buttons">
        <button type="submit" name="action" value="accept" className="small">Accept tag</button>
        <button type="submit" name="action" value="discard" className="small ghost">Discard</button>
      </div>
    </ActionForm>)}
  </div>
}
