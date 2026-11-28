import { Fragment } from 'react'
import { ActionForm, text, listKey, PageLink } from '../ui'
import type { DashboardChannel, DashboardPage, Job, UnknownDelivery } from '../api'
import { Badge } from '../components/Badge'
import { ChannelLinks } from '../components/ChannelLinks'
import { DataTable, type Column } from '../components/DataTable'
import { DeliveryActions } from '../components/DeliveryActions'
import { JobTable, useLiveJobs } from '../components/JobTable'

export function Dashboard({ data }: { data: DashboardPage }) {
  const { setup, scheduler, stats, channels, recent_unknown, sends, storage } = data
  const jobs = useLiveJobs(data.jobs)
  return <>
    {setup ? <>
      <section className="panel setup">
        <div className="setup-heading">
          <div>
            <h2>First run setup</h2>
            <p className="hint">Complete these steps to publish your first post.</p>
          </div>
          {(setup.next && setup.next.href) ? <>
            <PageLink className="button" href={setup.next.href ?? undefined}>{setup.next.action} →</PageLink>
          </> : null}
        </div>
        <ol className="setup-steps">
          {setup.steps.map((step, index, all) => <Fragment key={listKey(step, index)}>
            <li className={(step.done ? "done" : "")}>
              <span className="setup-marker" role="img" aria-label={(step.done ? "Complete" : "Incomplete")}>{step.done ? <>✓</> : <>{(index + 1)}</>}</span>
              <div>
                <strong>{step.title}</strong>
                <div className="hint">{step.detail}</div>
              </div>
              {(index === all.length - 1 && scheduler.stale) ? <>
                <code className="setup-action">nujijushi run</code>
              </> : null}
            </li>
          </Fragment>)}
        </ol>
        {!(setup.connected) ? <>
          <div id="telegram-setup" className="panel inset">
            <h3>Configure Telegram</h3>
            <p>Create a bot with @BotFather. Set <code>[telegram] bot_token</code> in your <code>config.toml</code>
              or set <code>TG_BOT_TOKEN</code> in the environment. Restart nujijushi to load the token.</p>
            <p className="hint">To manage the archive from Telegram, find your user ID with <code>/whoami</code>
              in a private chat with the running bot, add it to <code>[access] admins</code>, and restart.</p>
          </div>
        </> : null}
      </section>
    </> : null}

    {scheduler.stale ? <>
      <section className="panel alert">
        <h2>Nothing is publishing on a schedule</h2>
        <p className="hint">
          {scheduler.last_tick ? <>The scheduler last ran {scheduler.age} ago.</> : <>The scheduler has never run in this data directory.</>}
          Scheduler slots are driven by <code>nujijushi run</code>; <code>nujijushi serve</code> only
          serves this UI. The queue, imports and the publish buttons keep working, but no post goes
          out on its own until that process runs. If it is running, check it is still alive.
        </p>
      </section>
    </> : null}

    <section className="stats">
      <div className="stat"><span className="num">{stats.posts}</span><span className="lbl">posts</span></div>
      <div className="stat"><span className="num">{stats.approved}</span><span className="lbl">approved</span></div>
      <div className="stat"><span className="num">{stats.queued}</span><span className="lbl">queued</span></div>
      <div className="stat"><span className="num">{stats.sent}</span><span className="lbl">sent</span></div>
      <div className={text("stat ", ((stats.unknown > 0) ? "warn" : ""))}><span className="num">{stats.unknown}</span><span className="lbl">unconfirmed</span></div>
    </section>

    {recent_unknown.length > 0 ? <>
      <section className="panel alert">
        <h2>Unconfirmed sends</h2>
        <p className="hint">These were interrupted mid-publish. Check the channel, then mark them delivered or not — nujijushi will never guess.</p>
        <DataTable label="Unconfirmed sends" rows={recent_unknown} columns={unconfirmedColumns}
          rowKey={(item) => item.id} />
      </section>
    </> : null}

    <section className="panel">
      <h2>Channels</h2>
      <DashboardChannels channels={channels} jobs={jobs} />
    </section>

    {storage.length > 0 ? <>
      <section className="panel">
        <h2>Storage mirrors</h2>
        <DataTable label="Storage mirrors" rows={storage} columns={storageColumns}
          rowKey={(sink) => sink.name} compact />
        <ActionForm action="/storage/drain" className="inline">
          <button className="small" type="submit" title="queue an upload of the pending mirrors">Upload pending</button>
          <span className="hint">queued as a job · mirrors are also drained automatically by the background worker</span>
        </ActionForm>
      </section>
    </> : null}

    <div className="cols">
      <section className="panel">
        <h2>Recent sends</h2>
        <DataTable label="Recent sends" rows={sends} columns={sendColumns} rowKey={(_, index) => index}
          compact hideHeader empty={<p className="hint">Nothing sent yet.</p>} />
      </section>

      <section className="panel">
        <h2>Jobs <PageLink className="hint" href="/jobs">all jobs →</PageLink></h2>
        <JobTable jobs={jobs} layout="recent"
          empty={<p className="hint">Nothing queued. <PageLink href="/import">Import something</PageLink>.</p>} />
      </section>
    </div>
  </>
}

const unconfirmedColumns: Column<UnknownDelivery>[] = [
  { label: 'queue', cell: (item) => '#' + item.id },
  {
    label: 'channel',
    cell: (item) => <>
      {item.public_channel
        ? <a href={item.public_channel} target="_blank" rel="noopener">{item.channel}</a>
        : item.channel}
      {item.messages.map((message, index) => <div key={message}>
        <a href={message} target="_blank" rel="noopener">Telegram message {index + 1}</a>
      </div>)}
    </>,
  },
  { label: 'post', cell: (item) => <PageLink href={'/posts/' + item.post_id}>#{item.post_id}</PageLink> },
  { label: 'note', className: 'hint', cell: (item) => item.error },
  {
    label: 'actions', hideLabel: true, className: 'actions',
    cell: (item) => <ActionForm action="/queue/action" className="inline" fields={{ id: item.id, channel: item.channel_id }}>
      <DeliveryActions />
    </ActionForm>,
  },
]

function DashboardChannels({ channels, jobs }: { channels: DashboardChannel[]; jobs: Job[] }) {
  const publishing = new Set(jobs
    .filter((job) => job.active && (job.kind === 'publish_channel' || job.kind === 'flush_channel'))
    .map((job) => job.payload))
  const columns: Column<DashboardChannel>[] = [
    {
      label: 'channel',
      cell: (channel) => <>
        <PageLink href={text('/channels/', channel.id, '/edit')}>{channel.title}</PageLink>
        <span className="hint">{channel.tg_id}</span>
        {channel.paused && <Badge tone="red">paused</Badge>}
        {channel.failures > 0 && <Badge tone="red">{channel.failures} fails</Badge>}
        <Badge tone="accent" data-role="publishing"
          hidden={!channel.publishing && !publishing.has(String(channel.id))}>publishing…</Badge>
      </>,
    },
    { label: 'queue', cell: (channel) => <span data-role="queued">{channel.queued}</span> },
    { label: 'every', cell: (channel) => channel.interval + 'm' },
    { label: 'nsfw', cell: (channel) => <Badge>{channel.nsfw_policy}</Badge> },
    {
      label: 'next', className: 'hint',
      cell: (channel) => <>{channel.reason}{channel.next_at && <> · {channel.next_at}</>}</>,
    },
    {
      label: 'actions', hideLabel: true, className: 'actions',
      cell: (channel) => <>
        <ChannelLinks channelId={channel.id} />
        <ActionForm action="/publish" className="inline" fields={{ channel_id: channel.id, back: '/' }}
          data-confirm={text('Queue a publish of the next post to ', channel.title, '? The worker sends it in the background; this returns immediately.')}>
          <button className="small" type="submit" title="queue the next post for this channel, ignoring the schedule">Publish next</button>
        </ActionForm>
      </>,
    },
  ]
  return <DataTable label="Channels" rows={channels} columns={columns} rowKey={(channel) => channel.id}
    rowAttributes={(channel) => ({ 'data-channel': channel.id })}
    empty={<p className="hint">No channels yet. <PageLink href="/channels">Create one</PageLink> — you need the bot as an admin in it.</p>} />
}

const storageColumns: Column<DashboardPage['storage'][number]>[] = [
  { label: 'sink', cell: (sink) => <code>{sink.name}</code> },
  { label: 'where', className: 'hint', cell: (sink) => sink.where },
  { label: 'done', cell: (sink) => sink.done },
  { label: 'pending', cell: (sink) => sink.pending },
  { label: 'failed', cell: (sink) => sink.failed > 0 ? <Badge tone="red">{sink.failed}</Badge> : 0 },
  { label: 'No outgoing media', cell: (sink) => sink.blocked },
]

const sendColumns: Column<DashboardPage['sends'][number]>[] = [
  { label: 'time', cell: (send) => send.at },
  { label: 'channel', cell: (send) => 'ch #' + send.channel_id },
  { label: 'post', cell: (send) => send.post_id && <PageLink href={'/posts/' + send.post_id}>#{send.post_id}</PageLink> },
  { label: 'result', cell: (send) => send.ok ? <Badge tone="green">ok</Badge> : <Badge tone="red">fail</Badge> },
  { label: 'note', className: 'hint', cell: (send) => send.error },
]
