import { Fragment } from 'react'
import { ActionForm, TextField, CaptionPreview, text, listKey } from '../ui'
import type { PreviewPage } from '../api'

export function Preview({ data }: { data: PreviewPage }) {
  const { channel, held, stopped, enable_item, items } = data
  return <>
    <section className="panel">
      <h2>next slots · {channel.title}{" "}<span className="hint">{channel.tg_id}</span></h2>
      <p className="hint"><a href={text("/channels/", channel.id, "/edit")}>settings</a> · <a href={text("/channels/", channel.id, "/queue")}>queue</a></p>
      {stopped ? <>
        <p className="hint">Scheduling is off for this channel. These times show when posts could go out after scheduling is enabled.</p>
      </> : null}
      {held ? <>
        <article className="slot">
          <header>
            <span className="badge">held for review</span>
            <a href={text("/posts/", held.post_id)}>View artwork #{held.post_id}</a>
            <span className="hint">{held.media} media · {held.messages} message(s) · {held.reason}</span>
          </header>
          <CaptionPreview caption={held.caption}></CaptionPreview>
        </article>
      </> : null}
      {items.length > 0 ? <>
        {items.map((item, index) => <Fragment key={listKey(item, index)}>
          <article className="slot">
            <header>
              <span className="badge">slot {item.slot}</span>
              <span className="hint">{item.eta} · in {item.eta_in}</span>
              <a href={text("/posts/", item.post_id)}>#{item.post_id}</a>
              <span className="hint">{item.media} media · {item.messages} message(s) · {item.reason}</span>
              {item.spoiler ? <><span className="badge">spoiler</span></> : null}
              {item.nsfw ? <><span className="badge red">nsfw</span></> : null}
            </header>
            <CaptionPreview caption={item.caption}></CaptionPreview>
          </article>
        </Fragment>)}
      </> : <>{!(held) ? <>
        <p className="hint">The queue has no posts eligible for publishing.</p>
      </> : null}</>}
      {enable_item ? <>
        <ActionForm action={text("/channels/", channel.id, "/preview")}>
          <TextField type="hidden" name="queue_id" value={enable_item.queue_id} />
          <TextField type="hidden" name="post_id" value={enable_item.post_id} />
          <TextField type="hidden" name="target" value={channel.tg_id} />
          <TextField type="hidden" name="held" value={(held ? "true" : "false")} />
          <p className="hint">Confirming allows scheduled publishing for {channel.title}.
            If the scheduler is running, queued posts can go out as soon as a slot is due.</p>
          <button type="submit">{(stopped ? "Confirm preview and enable scheduling" : "Confirm preview and release post")}</button>
        </ActionForm>
      </> : <>{Boolean((held && !(held.allowed))) ? <>
        <p className="hint">This post cannot be released. Review the post or remove it from the <a href={text("/channels/", channel.id, "/queue")}>queue</a>.</p>
      </> : null}</>}
    </section>
  </>
}
