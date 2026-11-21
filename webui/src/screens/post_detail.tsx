import { Fragment, useState } from 'react'
import { ActionForm, TextField, SelectField, TextArea, JobProgress, CaptionPreview, CompressionOptions, text, listKey, externalUrl } from '../ui'
import { Badge } from '../components/Badge'
import { ChannelSelect } from '../components/ChannelSelect'
import type { Post, PostDetailPage, TagSuggestions } from '../api'

export function PostDetail({ data }: { data: PostDetailPage }) {
  const { post, open_section, guest_review, outgoing_count, tag_review, can_suggest_tags, captions, default_channel, ratings, states, channels } = data
  return <>
    <header className="post-heading">
      <div>
        <a href="/posts" className="hint" data-return-to-posts="">← Posts</a>
        <h1>{(post.title || text("Post #", post.id))}</h1>
        <div className="hint">#{post.id} · {post.source} / {post.source_id}
          <Badge className={'state-' + post.state}>{post.state}</Badge>
        </div>
      </div>
      <a className="button small ghost post-delete-link" href={text("/posts/", post.id, "/delete")}>Delete post…</a>
    </header>

    <div className="detail">
      <Gallery data={data} />

      <aside>
        {channels.length > 0 ? <>
          <section className="panel">
            <h2>Add to queue</h2>
            <ActionForm action={text("/posts/", post.id, "/queue")} className="post-buttons">
              <label>Destination
                <ChannelSelect channels={channels} defaultValue={default_channel ?? ''} name="channel" aria-label="Destination" required />
              </label>
              <button type="submit" className="small" {...(!(outgoing_count) ? { "disabled": true } : {})}>Add to queue</button>
            </ActionForm>
          </section>
        </> : null}
        <PostEditor key={post.id} post={post} />

        <details className="panel post-disclosure" open>
          <summary>Review post</summary>
          <div className="post-disclosure-body">
            <ActionForm action={text("/posts/", post.id)}>
              <div className="post-review-fields">
                <label>Rating
                  <SelectField defaultValue={post.rating} name="rating">
                    {ratings.map((r, index) => <Fragment key={listKey(r, index)}><option value={r}>{r}</option></Fragment>)}
                  </SelectField>
                </label>
                <label>Sensitivity
                  <SelectField defaultValue={post.nsfw_override} name="nsfw">
                    <option value="auto">Automatic</option>
                    <option value="yes">Force NSFW</option>
                    <option value="no">Force SFW</option>
                  </SelectField>
                </label>
                <label>State
                  <SelectField defaultValue={post.state} name="state">
                    {states.map((s, index) => <Fragment key={listKey(s, index)}><option value={s}>{s}</option></Fragment>)}
                  </SelectField>
                </label>
              </div>
              {guest_review ? <>
                <label>Guest approval reward
                  <TextField type="number" name="approval_points" min="5" max="20" step="1" placeholder="10" />
                  <span className="hint">Optional total to split among guests. Select approved to set it.</span>
                </label>
              </> : null}
              <label>Notes
                <TextField name="notes" defaultValue={post.notes ?? undefined} />
              </label>
              <button type="submit" className="small">Save changes</button>
            </ActionForm>
          </div>
        </details>

        <details className="panel post-disclosure" {...((open_section === "tags") ? { "open": true } : {})}>
          <summary>Tags <span className="hint">{post.tags.length}</span></summary>
          <div className="post-disclosure-body">
            <div className="tagcloud">
              {post.tags.map((t, index) => <Fragment key={listKey(t, index)}>
                <div className="post-tag">
                  <a href={text("/posts?tag=", t.name)}>#{t.name}</a>
                  {t.source ? <><span className="badge">source</span></> : null}
                  {t.manual ? <><span className="badge">manual</span></> : null}
                  {t.source ? <>
                    <ActionForm action={text("/posts/", post.id, "/tags")} className="inline">
                      <TextField type="hidden" name="tag_id" value={t.id} />
                      <button type="submit" name="action" value="remove" className="small ghost" aria-label={text("Remove source tag ", t.name)}>remove</button>
                    </ActionForm>
                  </> : null}
                </div>
              </Fragment>)}
            </div>
            {post.tag_removals.length > 0 ? <>
              <h3>Removed source tags</h3>
              <div className="tagcloud">
                {post.tag_removals.map((t, index) => <Fragment key={listKey(t, index)}>
                  <ActionForm action={text("/posts/", post.id, "/tags")} className="inline">
                    <TextField type="hidden" name="tag_id" value={t.id} />
                    <span className="badge">#{t.name}</span>
                    <button type="submit" name="action" value="undo" className="small ghost" aria-label={text("Undo removal of source tag ", t.name)}>undo</button>
                  </ActionForm>
                </Fragment>)}
              </div>
            </> : null}

            <ActionForm action={text("/posts/", post.id)} className="post-tool-row">
              <label>Manual tags (comma separated)
                <TextField name="tags" defaultValue={post.manual_tags} />
              </label>
              <button type="submit" className="small">Save tags</button>
            </ActionForm>
          </div>
        </details>

        <TagReview postId={post.id} review={tag_review} enabled={can_suggest_tags} open={open_section === "suggestions"} />

        <section className="panel post-captions">
          <h2>Caption previews</h2>
          {captions.length ? captions.map((c, index) => <Fragment key={listKey(c, index)}>
            <details className="caption-box" {...(index === 0 ? { "open": true } : {})}>
              <summary>
                {c.channel}
                {c.allowed ? <><span className="badge green">sendable</span></> : <><span className="badge red">vetoed</span></>}
                {c.spoiler ? <><span className="badge">spoiler</span></> : null}
                {c.eta ? <><span className="badge accent" title="when it would go out if nobody interferes">est. {c.eta} · in {c.eta_in}</span></> : null}
              </summary>
              <CaptionPreview caption={c.caption}></CaptionPreview>
              <p className="hint">{c.length} chars · {c.messages} message(s) · {c.verdict}</p>
            </details>
          </Fragment>) : <>
            <p className="hint"><a href="/channels#add-channel">Add a channel</a> to preview a caption.</p>
          </>}
        </section>
      </aside>
    </div>
  </>
}

function PostEditor({ post }: { post: Post }) {
  const [title, setTitle] = useState(post.title ?? '')
  const [description, setDescription] = useState(post.description ?? '')
  return (
    <details className="panel post-disclosure">
      <summary>Edit post details</summary>
      <ActionForm className="post-disclosure-body" fields={{ title, description }} action={text("/posts/", post.id)}>
        <label>Title
          <input name="title" value={title} onChange={(event) => setTitle(event.target.value)} />
        </label>
        <label>Description
          <TextArea name={"description"} rows={5} value={description} onChange={(event) => setDescription(event.target.value)}></TextArea>
        </label>
        <button type="submit">Save details</button>
      </ActionForm>
    </details>

  )
}

function Gallery({ data }: { data: PostDetailPage }) {
  const { post, media, media_total, outgoing_count, submitter, merge_job, has_media_tools, can_merge, gallery_version, open_section } = data
  const [selected, setSelected] = useState(new Set<number>())
  const selection = Object.fromEntries([...selected].map((id) => ['sel_' + id, 'on']))
  function toggle(id: number) {
    setSelected((previous) => {
      const next = new Set(previous)
      if (!next.delete(id)) next.add(id)
      return next
    })
  }
  return (
    <div>
      <section className="panel">
        {!(outgoing_count) ? <><p className="hint">No outgoing media</p></> : null}
        <h2>Artwork <span className="hint">{media.length} item(s) · {media_total}</span></h2>
        {media.length > 0 ? <>
          <div className="media-grid">
            {media.map((m, index) => <Fragment key={listKey(m, index)}>
              <figure id={text("media-", (index + 1))}>
                {m.is_video ? <>
                  <video controls preload="none" poster={text("/thumb/", m.thumb)} src={text("/media/", m.hash)}></video>
                </> : <>
                  <a href={text("/media/", m.hash)} target="_blank"><img loading="lazy" src={text("/media/", m.hash)} alt="" /></a>
                </>}
                {m.hidden ? <><span className="badge amber">Hidden from outgoing copies</span></> : null}
                <figcaption className="hint">{m.ext}{" "}{m.width}×{m.height} · {m.kib} KiB</figcaption>
                {m.over_photo_limit ? <><p className="hint">Exceeds the photo upload limits</p></> : null}
              </figure>
            </Fragment>)}
          </div>
        </> : <>
          <p className="hint">No media archived for this post.</p>
        </>}
        <dl className="facts">
          {post.artist ? <>
            <dt>artist</dt><dd>{post.artist_url ? <><a href={externalUrl(post.artist_url)} target="_blank">{post.artist}</a></> : <>{post.artist}</>}</dd>
          </> : null}
          {post.url ? <><dt>source</dt><dd><a href={externalUrl(post.url)} target="_blank">Open original post</a></dd></> : null}
          <dt>rating</dt><dd>{post.rating}{" "}{post.nsfw ? <><Badge tone="red">nsfw</Badge></> : <><Badge tone="green">sfw</Badge></>}{" "}<span className="hint">override: {post.nsfw_override}</span></dd>
          <dt>imported</dt><dd className="hint">{post.imported}{submitter ? <> · via telegram {submitter.label}{!(submitter.admin) ? <>{" "}<span className="badge amber">not an admin</span></> : null}</> : null}</dd>
          {post.description ? <><dt>description</dt><dd className="prose">{post.description}</dd></> : null}
        </dl>
      </section>

      {merge_job && <div className="hint post-job-status"><JobProgress initial={merge_job} /></div>}

      {has_media_tools ? <>
        <details id="media-tools" className="panel post-disclosure" {...((open_section === "media") ? { "open": true } : {})}>
          <summary>Media tools</summary>
          <div className="post-disclosure-body">
            {(merge_job && merge_job.active) ? <>
              <p className="hint">A gallery edit is in progress.</p>
            </> : <>
              {media.map((m, index) => <Fragment key={listKey(m, index)}>
                <div className="post-tool-row">
                  <a className="post-image-link" href={text("#media-", (index + 1))}><span>Image {(index + 1)}</span></a>
                  <ActionForm action={text("/posts/", post.id, "/media")} className="inline">
                    <TextField type="hidden" name="gallery_version" value={gallery_version} />
                    <TextField type="hidden" name="media_id" value={m.id} />
                    <button type="submit" name="action" value={(m.hidden ? "restore" : "hide")} className="small ghost">{(m.hidden ? "Restore" : "Hide from outgoing copies")}</button>
                  </ActionForm>
                  {m.can_select ? <>
                    <label className="check">
                      <input type="checkbox" checked={selected.has(m.id)} onChange={() => toggle(m.id)} aria-label={text("Include image ", (index + 1), " in merge")} />
                      Include in merge
                    </label>
                  </> : null}
                  {m.can_compress ? <>
                    <details className="post-inline-options">
                      <summary>Compress image</summary>
                      <ActionForm action={text("/posts/", post.id, "/media")} className="post-command">
                        <TextField type="hidden" name="gallery_version" value={gallery_version} />
                        <TextField type="hidden" name="media_id" value={m.id} />
                        <CompressionOptions></CompressionOptions>
                        <button type="submit" name="action" value="compress" className="small">Compress</button>
                      </ActionForm>
                    </details>
                  </> : null}
                  {m.compressed ? <>
                    <ActionForm action={text("/posts/", post.id, "/media")} className="inline">
                      <TextField type="hidden" name="gallery_version" value={gallery_version} />
                      <TextField type="hidden" name="media_id" value={m.id} />
                      <button type="submit" name="action" value="undo_compression" className="small ghost">Undo compression</button>
                    </ActionForm>
                  </> : <>{m.undo_id ? <>
                    <ActionForm action={text("/posts/", post.id, "/media")} className="inline">
                      <TextField type="hidden" name="gallery_version" value={gallery_version} />
                      <TextField type="hidden" name="merge_id" value={m.undo_id ?? undefined} />
                      <button type="submit" name="action" value="undo" className="small ghost">Undo merge</button>
                    </ActionForm>
                  </> : null}</>}
                </div>
              </Fragment>)}
              {can_merge ? <>
                <ActionForm action={text("/posts/", post.id, "/media")} id="merge-gallery" className="post-tool-row" feedback="inline" fields={selection} onJobDone={() => setSelected(new Set())}>
                  <TextField type="hidden" name="gallery_version" value={gallery_version} />
                  <button type="submit" name="action" value="merge" className="small">Merge side by side</button>
                  <span className="hint">Select any still images of the same height. They merge in gallery order.</span>
                  <details className="post-inline-options">
                    <summary>Merge output options</summary>
                    <CompressionOptions merge></CompressionOptions>
                  </details>
                </ActionForm>
              </> : null}
            </>}
          </div>
        </details>
      </> : null}
    </div>
  )
}

function TagReview({ postId, review, enabled, open }: {
  postId: number; review: TagSuggestions | null; enabled: boolean; open: boolean
}) {
  if (!enabled && !review) return null
  return (

    <details className="panel post-disclosure" {...((open || (review && review.job.active)) ? { "open": true } : {})}>
      <summary>Tag suggestions{review && review.suggestions.length > 0 ? <>{" "}<span className="hint">{review.suggestions.length} to review</span></> : null}</summary>
      <div className="post-disclosure-body">
        {review ? <>
          <p className="hint"><JobProgress initial={review.job} /></p>
          {review.stale ? <>
            <p className="hint">The artwork or source metadata changed. Suggest tags again to review the current post.</p>
          </> : null}
          {review.suggestions.map((suggestion, index, all) => <Fragment key={listKey(suggestion, index)}>
            <ActionForm action={text("/posts/", postId, "/tag-suggestions")} className="post-tool-row">
              <TextField type="hidden" name="job_id" value={review.job.id} />
              <TextField type="hidden" name="suggestion_id" value={suggestion.id} />
              <label>{suggestion.kind}
                <TextField name="tag" defaultValue={suggestion.tag} aria-label={text("Edit suggested ", suggestion.kind, " tag ", suggestion.tag)} />
              </label>
              <p className="hint">
                {suggestion.wiki_url ? <><a href={suggestion.wiki_url ?? undefined}>{suggestion.evidence}</a></> : <>{suggestion.evidence}</>}
                {suggestion.character_tag_id ? <> · ID {suggestion.character_tag_id}</> : null}
                {suggestion.images.map((position, index) => <Fragment key={listKey(position, index)}> · <a href={text("#media-", position)}>Image {position}</a></Fragment>)}
              </p>
              <button type="submit" name="action" value="accept" className="small">Save tag</button>
              <button type="submit" name="action" value="discard" className="small ghost">Discard</button>
            </ActionForm>
          </Fragment>)}
        </> : null}
        {Boolean((enabled && (!(review) || !(review.job.active)))) ? <>
          <ActionForm action={text("/posts/", postId, "/tag-suggestions")}>
            <button type="submit" name="action" value="generate" className="small">{(review ? "Suggest again" : "Suggest tags")}</button>
          </ActionForm>
        </> : null}
      </div>
    </details>

  )
}
