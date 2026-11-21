import { Fragment } from 'react'
import { ActionForm, TextField, JobBar, JobState, useActions, text, listKey } from '../ui'
import type { DashboardPage } from '../api'

export function Dashboard({ data }: { data: DashboardPage }) {
  const { setup, scheduler, stats, channels, recent_unknown, sends, storage, jobs: initialJobs } = data
  const { jobs: updates } = useActions()
  const jobs = initialJobs.map((job) => updates.get(job.id) ?? job)
  return <>
    {setup ? <>
      <section className="panel setup">
        <div className="setup-heading">
          <div>
            <h2>first run setup</h2>
            <p className="hint">Complete these steps to publish your first post.</p>
          </div>
          {(setup.next && setup.next.href) ? <>
            <a className="button" href={setup.next.href ?? undefined}>{setup.next.action} →</a>
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
        <h2>nothing is publishing on a schedule</h2>
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
        <h2>unconfirmed sends</h2>
        <p className="hint">These were interrupted mid-publish. Check the channel, then mark them delivered or not — nujijushi will never guess.</p>
        <div className="responsive-table" tabIndex={0}><table>
          <thead><tr><th>queue</th><th>channel</th><th>post</th><th>note</th><th></th></tr></thead>
          <tbody>
            {recent_unknown.map((item, index, all) => <Fragment key={listKey(item, index)}>
              <tr>
                <td>#{item.id}</td>
                <td>
                  {item.public_channel ? <><a href={item.public_channel ?? undefined} target="_blank" rel="noopener">{item.channel}</a>
                  </> : <>{item.channel}</>}
                  {item.messages.map((message, index) => <Fragment key={listKey(message, index)}><div><a href={message} target="_blank" rel="noopener">Telegram message {(index + 1)}</a></div></Fragment>)}
                </td>
                <td><a href={text("/posts/", item.post_id)}>#{item.post_id}</a></td>
                <td className="hint">{item.error}</td>
                <td className="actions">
                  <ActionForm action="/queue/action" className="inline">
                    <TextField type="hidden" name="id" value={item.id} />
                    <TextField type="hidden" name="channel" value={item.channel_id} />
                    <TextField type="number" name="message_id" min="1" aria-label="Telegram message ID (optional)" placeholder="Message ID" />
                    <button name="action" value="delivered" className="small">Confirm delivered</button>
                    <button name="action" value="not_delivered" className="small ghost" data-confirm="Check Telegram first. This queues the entire post for another send.">Requeue entire post</button>
                  </ActionForm>
                </td>
              </tr>
            </Fragment>)}
          </tbody>
        </table></div>
      </section>
    </> : null}

    <section className="panel">
      <h2>channels</h2>
      {channels.length > 0 ? <>
        <div className="responsive-table" tabIndex={0}><table>
          <thead><tr><th>channel</th><th>queue</th><th>every</th><th>nsfw</th><th>next</th><th></th></tr></thead>
          <tbody>
            {channels.map((c, index) => <Fragment key={listKey(c, index)}>
              <tr data-channel={c.id}>
                <td>
                  <a href={text("/channels/", c.id, "/edit")}>{c.title}</a>
                  <span className="hint">{c.tg_id}</span>
                  {c.paused ? <><span className="badge red">paused</span></> : null}
                  {(c.failures > 0) ? <><span className="badge red">{c.failures} fails</span></> : null}
                  <span className="badge accent" data-role="publishing" hidden={!c.publishing && !jobs.some((job) => job.active && job.kind === "publish_channel" && job.payload === String(c.id))}>publishing…</span>
                </td>
                <td data-role="queued">{c.queued}</td>
                <td>{c.interval}m</td>
                <td><span className="badge">{c.nsfw_policy}</span></td>
                <td className="hint">{c.reason}{c.next_at ? <> · {c.next_at}</> : null}</td>
                <td className="actions">
                  <a className="button small" href={text("/channels/", c.id, "/queue")}>queue</a>
                  <a className="button small ghost" href={text("/channels/", c.id, "/preview")}>preview</a>
                  <ActionForm action="/publish" className="inline" data-confirm={text("Queue a publish of the next post to ", c.title, "? The worker sends it in the background; this returns immediately.")}>
                    <TextField type="hidden" name="channel_id" value={c.id} />
                    <TextField type="hidden" name="back" value="/" />
                    <button className="small" type="submit" title="queue the next post for this channel, ignoring the schedule">publish next</button>
                  </ActionForm>
                </td>
              </tr>
            </Fragment>)}
          </tbody>
        </table></div>
      </> : <>
        <p className="hint">No channels yet. <a href="/channels">Create one</a> — you need the bot as an admin in it.</p>
      </>}
    </section>

    {storage.length > 0 ? <>
      <section className="panel">
        <h2>storage mirrors</h2>
        <div className="responsive-table" tabIndex={0}><table className="compact">
          <thead><tr><th>sink</th><th>where</th><th>done</th><th>pending</th><th>failed</th><th>No outgoing media</th></tr></thead>
          <tbody>
            {storage.map((sink, index) => <Fragment key={listKey(sink, index)}>
              <tr>
                <td><code>{sink.name}</code></td>
                <td className="hint">{sink.where}</td>
                <td>{sink.done}</td>
                <td>{sink.pending}</td>
                <td>{sink.failed ? <><span className="badge red">{sink.failed}</span></> : <>0</>}</td>
                <td>{sink.blocked}</td>
              </tr>
            </Fragment>)}
          </tbody>
        </table></div>
        <ActionForm action="/storage/drain" className="inline">
          <button className="small" type="submit" title="queue an upload of the pending mirrors">upload pending</button>
          <span className="hint">queued as a job · mirrors are also drained automatically by the background worker</span>
        </ActionForm>
      </section>
    </> : null}

    <div className="cols">
      <section className="panel">
        <h2>recent sends</h2>
        {sends.length > 0 ? <>
          <div className="responsive-table" tabIndex={0}><table className="compact"><tbody>
            {sends.map((s, index) => <Fragment key={listKey(s, index)}>
              <tr>
                <td>{s.at}</td>
                <td>ch #{s.channel_id}</td>
                <td><a href={text("/posts/", s.post_id)}>#{s.post_id}</a></td>
                <td>{s.ok ? <><span className="badge green">ok</span></> : <><span className="badge red">fail</span></>}</td>
                <td className="hint">{s.error}</td>
              </tr>
            </Fragment>)}
          </tbody>
          </table></div>
        </> : <><p className="hint">Nothing sent yet.</p></>}
      </section>

      <section className="panel">
        <h2>jobs <a className="hint" href="/jobs">all jobs →</a></h2>
        {jobs.length > 0 ? <>
          <div className="responsive-table" tabIndex={0}><table className="compact dashboard-jobs"><tbody>
            {jobs.map((j, index) => <Fragment key={listKey(j, index)}>
              <tr data-job={j.id} data-state={j.state}>
                <td>#{j.id}</td>
                <td><span className="badge">{j.kind_label}</span></td>
                <td><JobState state={j.state} /></td>
                <td className="progress-cell">
                  <JobBar job={j} />
                </td>
                <td className="hint job-message" data-role="message">{j.message}</td>
              </tr>
            </Fragment>)}
          </tbody>
          </table></div>
        </> : <><p className="hint">Nothing queued. <a href="/import">Import something</a>.</p></>}
      </section>
    </div>
  </>
}
