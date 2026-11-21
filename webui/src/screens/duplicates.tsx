import { Fragment } from 'react'
import { ActionForm, TextField, text, listKey, externalUrl } from '../ui'
import type { DuplicatesPage, DuplicatePost, DuplicateImage } from '../api'
import { Thumbnail } from '../components/Thumbnail'

function details(post: DuplicatePost) {
  return <>
    <div>
      <h3><a href={text("/posts/", post.id)}>#{post.id}{post.title ? <> · {post.title}</> : null}</a></h3>
      <p>{post.artist} · {post.url ? <><a href={externalUrl(post.url)} target="_blank" rel="noopener noreferrer">{post.source}</a></> : <>{post.source}</>}</p>
      <p><span className="badge">{post.state}</span>
        {post.queues.length ? post.queues.map((queue, index) => <Fragment key={listKey(queue, index)}><a className="badge" href={text("/channels/", queue.channel_id, "/queue")}>{queue.alias} · {queue.state}</a></Fragment>) : <><span className="hint">No queue or delivery history</span></>}
      </p>
    </div>
  </>
}

function thumbnail(item: DuplicateImage, post_id: number) {
  return <>
    <figure>
      {item.thumb ? <>
        <a href={text("/posts/", post_id)}><Thumbnail hash={item.hash} alt={text("Matched image ", item.position)} /></a>
      </> : <>
        <a href={text("/posts/", post_id)}>Open post #{post_id}</a>
      </>}
      <figcaption className="hint">Image {item.position}</figcaption>
    </figure>
  </>
}

export function Duplicates({ data }: { data: DuplicatesPage }) {
  const { pairs, previous, next, indexing } = data
  return <>
    <section className="panel">
      <h1>Duplicate artwork</h1>
      {indexing ? <><p className="hint" data-duplicates-indexing="">Indexing artwork. More matches will appear as indexing finishes.</p></> : null}
      {pairs.length === 0 ? <><p className="hint">No duplicate pairs on this page.</p></> : null}
      {pairs.map((pair, index, all) => <Fragment key={pair.left.id + ":" + pair.right.id}>
        <article className="panel inset">
          <div className="duplicate-columns">{details(pair.left)}{details(pair.right)}</div>
          {pair.matches.map((match, index) => <Fragment key={listKey(match, index)}>
            <p className="hint">{(match.exact ? "Exact file" : "Visually similar")}</p>
            <div className="duplicate-columns">
              {thumbnail(match.left, pair.left.id)}
              {thumbnail(match.right, pair.right.id)}
            </div>
          </Fragment>)}
          <ActionForm action="/duplicates/dismiss">
            <TextField type="hidden" name="left" value={pair.left.id} />
            <TextField type="hidden" name="right" value={pair.right.id} />
            <button className="ghost" type="submit">Dismiss pair</button>
          </ActionForm>
        </article>
      </Fragment>)}
      <nav className="pager">
        {previous ? <><a href={text("/duplicates?page=", previous)}>Previous</a></> : null}
        {next ? <><a href={text("/duplicates?page=", next)}>Next</a></> : null}
      </nav>
    </section>
  </>
}
