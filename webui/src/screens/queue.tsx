import { Fragment } from 'react'
import { ActionForm, TextField, text, listKey } from '../ui'
import type { QueuePage } from '../api'

export function Queue({ data }: { data: QueuePage }) {
  const { channel, activity, items, eta_hint } = data
  const attempt = activity.latest_attempt
  return <>
    <section className="panel" data-queue-revision={activity.revision}>
      <h2>queue · {channel.title}{" "}<span className="hint">{channel.tg_id}</span></h2>
      <p className="hint">
        <a href={text("/channels/", channel.id, "/edit")}>settings</a> ·
        <a href={text("/channels/", channel.id, "/preview")}>preview next slots</a>
      </p>
      {channel.enabled ? <>
        <ActionForm action={text("/channels/", channel.id, "/schedule")} className="inline">
          <button className="small ghost" name="action" value={(channel.paused ? "resume" : "pause")}>{(channel.paused ? "Resume schedule" : "Pause schedule")}</button>
        </ActionForm>
      </> : <>
        <p className="hint">Scheduling is off. <a href={text("/channels/", channel.id, "/preview")}>Review the preview to start</a>.</p>
      </>}
      {attempt ? <>

        <p className="queue-attempt">
          Latest send attempt{attempt.post_id ? <> for <a href={text("/posts/", attempt.post_id)}>post #{attempt.post_id}</a></> : null}
          <span className={text("badge ", ((attempt.state === "Sent") ? "green" : "red"))}>{attempt.state}</span>
          {attempt.error ? <><span className="hint">{attempt.error}</span></> : null}
        </p>
      </> : null}
      {activity.jobs.length > 0 ? <>
        <ul id="queue-publish-jobs" className="queue-jobs">
          {activity.jobs.map((job, index) => <Fragment key={listKey(job, index)}>
            <li data-publish-job={job.id}>
              <span>{(job.queue_id ? text("Queue item #", job.queue_id) : "Publish next")}</span>
              <span className={text("badge ", ((job.state === "failed") ? "red" : ""))} data-live-state={job.state}>{job.label}</span>
              {job.message ? <><span className="hint">{job.message}</span></> : null}
            </li>
          </Fragment>)}
        </ul>
      </> : null}
      {items.length > 0 ? <>
        <div className="responsive-table" tabIndex={0}><table id="channel-queue-items">
          <thead><tr><th>#</th><th></th><th>post</th><th>state</th><th>prio</th><th>attempts</th><th>added</th><th title="when it would go out if nobody interferes">est.</th><th></th></tr></thead>
          <tbody>
            {items.map((i, index, all) => <Fragment key={listKey(i, index)}>
              <tr className={((i.state === "unknown") ? "row-unknown" : "")} data-queue={i.id}>
                <td>{i.id}</td>
                <td className="mini">
                  {i.thumb ? <><img loading="lazy" src={text("/thumb/", i.thumb)} alt="" /></> : null}
                  {i.nsfw ? <><span className="badge red">nsfw</span></> : null}
                </td>
                <td>
                  <a href={text("/posts/", i.post_id)}>#{i.post_id}</a>
                  {i.pinned ? <><span className="badge">pinned</span></> : null}
                  <span className="badge accent" data-role="publishing" hidden={i.state !== "sending" && !activity.jobs.some((job) => job.queue_id === i.id && ["pending", "running"].includes(job.state))}>publishing…</span>
                  <div className="hint">{i.artist} — {i.snippet}</div>
                  {i.error ? <><div className="hint err">{i.error}</div></> : null}
                </td>
                <td><span className={text("badge ", (((i.state === "unknown") || (i.state === "failed")) ? "red" : ""))} data-live-state={i.state}>{i.state_label}</span></td>
                <td>{i.priority}</td>
                <td>{i.attempts}</td>
                <td className="hint">{i.added}</td>
                <td className="hint">{i.eta ? <>{i.eta}<br /><span className="hint">in {i.eta_in}</span></> : null}</td>
                <td className="actions">
                  {(i.state === "queued") ? <>
                    <ActionForm action="/publish" className="inline" data-confirm={text("Queue post #", i.post_id, " for ", channel.title, "? The worker sends it in the background; this returns immediately.")}>
                      <TextField type="hidden" name="queue_id" value={i.id} />
                      <TextField type="hidden" name="back" value={text("/channels/", channel.id, "/queue")} />
                      <button className="small" type="submit" title="publish this item now, ignoring the schedule">publish</button>
                    </ActionForm>
                  </> : null}
                  <ActionForm id={text("queue-action-", i.id)} action="/queue/action" className="inline">
                    <TextField type="hidden" name="id" value={i.id} />
                    <TextField type="hidden" name="channel" value={channel.id} />
                    {(i.state === "unknown") ? <>
                      <p className="hint">Check the Telegram channel before confirming delivery or sending again.</p>
                      {i.delivery?.messages.map((message, index) => <Fragment key={listKey(message, index)}><a href={message} target="_blank" rel="noopener">Telegram message {(index + 1)}</a></Fragment>)}
                      {i.delivery?.public_channel ? <><a href={i.delivery?.public_channel ?? undefined} target="_blank" rel="noopener">Open Telegram channel</a></> : null}
                      <TextField id={text("telegram-message-", i.id)} type="number" name="message_id" min="1" aria-label="Telegram message ID (optional)" placeholder="Message ID" />
                      <button name="action" value="delivered" className="small">Confirm delivered</button>
                      <button name="action" value="not_delivered" className="small ghost" data-confirm="Check Telegram first. This queues the entire post for another send.">Requeue entire post</button>
                    </> : <>{(i.state === "held") ? <>
                      <a className="button small" href={text("/channels/", channel.id, "/preview")}>review preview</a>
                      <button name="action" value="remove" className="small danger" data-confirm="Remove this entry from the queue? The post stays in the archive.">Remove</button>
                    </> : <>{(i.state === "sending") ? <>
                      <span className="hint">Send in progress</span>
                    </> : <>
                      {i.pinned ? <>
                        <button name="action" value="unpin" className="small ghost">unpin</button>
                      </> : <>
                        <button name="action" value="pin" className="small ghost">pin</button>
                      </>}
                      {(i.state !== "skipped") ? <><button name="action" value="skip" className="small ghost">Skip</button></> : null}
                      {(i.state !== "queued") ? <><button name="action" value="reset" className="small ghost">Requeue</button></> : null}
                      <button name="action" value="remove" className="small danger" data-confirm="Remove this entry from the queue? The post stays in the archive.">Remove</button>
                    </>}</>}</>}
                  </ActionForm>
                </td>
              </tr>
            </Fragment>)}
          </tbody>
        </table></div>
        <p className="hint">
          <b>est.</b> is when that post would go out if nobody interferes: the order is the
          scheduler's own selection, the times come from the interval, the active hours and the
          daily cap{eta_hint ? <> — {eta_hint}</> : null}.
        </p>
      </> : <>
        <p className="hint">Empty. Queue posts from <a href="/posts">the posts grid</a>.</p>
      </>}
    </section>
  </>
}
