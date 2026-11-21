import type { QueuePage, QueueRow } from '../api'
import { Badge } from '../components/Badge'
import { DataTable, type Column } from '../components/DataTable'
import { DeliveryActions } from '../components/DeliveryActions'
import { ActionForm } from '../ui'
import { Thumbnail } from '../components/Thumbnail'
import { channelPath, postPath } from '../routes'

export function Queue({ data }: { data: QueuePage }) {
  const { channel, activity, items, eta_hint } = data
  const attempt = activity.latest_attempt
  const channelRoot = channelPath(channel.id)
  const flushing = activity.jobs.some((job) => job.kind === 'flush_channel'
    && (job.state === 'pending' || job.state === 'running'))
  return <section className="panel" data-queue-revision={activity.revision}>
    <h2>queue · {channel.title} <span className="hint">{channel.tg_id}</span></h2>
    <p className="hint">
      <a href={channelRoot + '/edit'}>settings</a> · <a href={channelRoot + '/preview'}>preview next slots</a>
    </p>
    {channel.enabled
      ? <ActionForm action={channelRoot + '/schedule'} className="inline">
        <button className="small ghost" name="action" value={channel.paused ? 'resume' : 'pause'}>
          {channel.paused ? 'Resume schedule' : 'Pause schedule'}
        </button>
      </ActionForm>
      : <p className="hint">Scheduling is off. <a href={channelRoot + '/preview'}>Review the preview to start</a>.</p>}
    <ActionForm action={channelRoot + '/flush'} className="inline"
      data-confirm={`Publish all queued posts to ${channel.title} (${channel.tg_id}) now? This ignores the schedule and daily cap.`}>
      <button className="small" type="submit" disabled={activity.queued === 0 || flushing}>Flush queue</button>
    </ActionForm>
    {attempt && <p className="queue-attempt">
      Latest send attempt{attempt.post_id && <> for <a href={postPath(attempt.post_id)}>post #{attempt.post_id}</a></>}
      {' '}<Badge tone={attempt.state === 'Sent' ? 'green' : 'red'}>{attempt.state}</Badge>
      {attempt.error && <span className="hint">{attempt.error}</span>}
    </p>}
    {activity.jobs.length > 0 && <ul id="queue-publish-jobs" className="queue-jobs">
      {activity.jobs.map((job) => <li key={job.id} data-publish-job={job.id}>
        <span>{job.queue_id ? 'Queue item #' + job.queue_id : job.kind === 'flush_channel' ? 'Flush queue' : 'Publish next'}</span>
        <Badge tone={job.state === 'failed' ? 'red' : undefined} data-live-state={job.state}>{job.label}</Badge>
        {job.kind === 'flush_channel' && job.state === 'running' && job.total > 0 &&
          <span className="hint">{job.progress}/{job.total} processed</span>}
        {job.message && <span className="hint">{job.message}</span>}
      </li>)}
    </ul>}
    <QueueTable channel={channel} activity={activity} items={items} />
    {items.length > 0 && <p className="hint">
      <b>est.</b> is when that post would go out if nobody interferes: the order is the
      scheduler's own selection, the times come from the interval, the active hours and the
      daily cap{eta_hint && <> — {eta_hint}</>}.
    </p>}
  </section>
}

function QueueTable({ channel, activity, items }: Pick<QueuePage, 'channel' | 'activity' | 'items'>) {
  const publishing = new Set(activity.jobs
    .filter((job) => job.state === 'pending' || job.state === 'running')
    .map((job) => job.queue_id))

  const columns: Column<QueueRow>[] = [
    { label: '#', cell: (item) => item.id },
    {
      label: 'artwork', hideLabel: true, className: 'mini',
      cell: (item) => <>
        {item.thumb && <Thumbnail hash={item.thumb} alt="" />}
        {item.nsfw && <Badge tone="red">nsfw</Badge>}
      </>,
    },
    {
      label: 'post',
      cell: (item) => <>
        <a href={postPath(item.post_id)}>#{item.post_id}</a>
        {item.pinned && <Badge>pinned</Badge>}
        <Badge tone="accent" data-role="publishing" hidden={item.state !== 'sending' && !publishing.has(item.id)}>
          publishing…
        </Badge>
        <div className="hint">{item.artist} — {item.snippet}</div>
        {item.error && <div className="hint err">{item.error}</div>}
      </>,
    },
    {
      label: 'state',
      cell: (item) => <Badge tone={item.state === 'unknown' || item.state === 'failed' ? 'red' : undefined}
        data-live-state={item.state}>{item.state_label}</Badge>,
    },
    { label: 'prio', cell: (item) => item.priority },
    { label: 'attempts', cell: (item) => item.attempts },
    { label: 'added', className: 'hint', cell: (item) => item.added },
    {
      label: 'est.', title: 'when it would go out if nobody interferes', className: 'hint',
      cell: (item) => item.eta && <>{item.eta}<br /><span className="hint">in {item.eta_in}</span></>,
    },
    {
      label: 'actions', hideLabel: true, className: 'actions',
      cell: (item) => <QueueActions item={item} channel={channel} />,
    },
  ]

  return <DataTable label="Channel queue" id="channel-queue-items" rows={items} columns={columns}
    rowKey={(item) => item.id}
    rowAttributes={(item) => ({ className: item.state === 'unknown' ? 'row-unknown' : undefined, 'data-queue': item.id })}
    empty={<p className="hint">Empty. Queue posts from <a href="/posts">the posts grid</a>.</p>} />
}

function QueueActions({ item, channel }: { item: QueueRow; channel: QueuePage['channel'] }) {
  const channelRoot = channelPath(channel.id)
  if (item.state === 'sending') return <span className="hint">Send in progress</span>

  return <>
    {item.state === 'queued' && <ActionForm action="/publish" className="inline"
      fields={{ queue_id: item.id, back: channelRoot + '/queue' }}
      data-confirm={`Queue post #${item.post_id} for ${channel.title}? The worker sends it in the background; this returns immediately.`}>
      <button className="small" type="submit" title="publish this item now, ignoring the schedule">publish</button>
    </ActionForm>}
    <ActionForm id={'queue-action-' + item.id} action="/queue/action" className="inline"
      fields={{ id: item.id, channel: channel.id }}>
      {item.state === 'unknown'
        ? <>
          <p className="hint">Check the Telegram channel before confirming delivery or sending again.</p>
          {item.delivery?.messages.map((message, index) => <a key={message} href={message} target="_blank" rel="noopener">
            Telegram message {index + 1}
          </a>)}
          {item.delivery?.public_channel && <a href={item.delivery.public_channel} target="_blank" rel="noopener">
            Open Telegram channel
          </a>}
          <DeliveryActions inputId={'telegram-message-' + item.id} />
        </>
        : <>
          {item.state === 'held'
            ? <a className="button small" href={channelRoot + '/preview'}>review preview</a>
            : <>
              <button name="action" value={item.pinned ? 'unpin' : 'pin'} className="small ghost">
                {item.pinned ? 'unpin' : 'pin'}
              </button>
              {item.state !== 'skipped' && <button name="action" value="skip" className="small ghost">Skip</button>}
              {item.state !== 'queued' && <button name="action" value="reset" className="small ghost">Requeue</button>}
            </>}
          <button name="action" value="remove" className="small danger"
            data-confirm="Remove this entry from the queue? The post stays in the archive.">
            Remove
          </button>
        </>}
    </ActionForm>
  </>
}
