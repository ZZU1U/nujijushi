import { ActionForm, TextField, text } from '../ui'
import type { PostDeletePage } from '../api'

export function PostDelete({ data }: { data: PostDeletePage }) {
  const { id, title, sending, imported_at } = data
  return <>
    <section className="panel confirm-post-delete">
      <h1>Delete post #{id}?</h1>
      {title ? <><p><strong>{title}</strong></p></> : null}
      <p>This permanently removes the post from your archive, including its notes, tags, media links, and queue entries. Copies already published to Telegram or mirrored to storage stay where they are.</p>
      {sending ? <>
        <p className="hint">This post is being sent. Wait for the send to finish, then refresh this page.</p>
        <a className="button ghost" href={text("/posts/", id)}>Back to post</a>
      </> : <>
        <ActionForm action={text("/posts/", id, "/delete")} className="post-buttons">
          <TextField type="hidden" name="imported_at" value={imported_at} />
          <a className="button ghost" href={text("/posts/", id)}>Cancel</a>
          <button className="danger" type="submit">Delete post</button>
        </ActionForm>
      </>}
    </section>
  </>
}
