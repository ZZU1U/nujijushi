import { Fragment, useEffect } from 'react'
import { ActionForm, TextField, JobProgress, externalUrl, text, listKey } from '../ui'
import type { WatchesPage } from '../api'

const dateTime = new Intl.DateTimeFormat(undefined, { dateStyle: 'medium', timeStyle: 'short' })

export function Watches({ data, reload }: { data: WatchesPage; reload: (quiet?: boolean) => Promise<WatchesPage | undefined> }) {
  const { watches } = data
  useEffect(() => {
    let inFlight = false
    function refresh() {
      if (document.hidden || inFlight) return
      inFlight = true
      void reload(true).catch(() => undefined).finally(() => { inFlight = false })
    }
    const timer = window.setInterval(refresh, 5000)
    document.addEventListener('visibilitychange', refresh)
    return () => {
      window.clearInterval(timer)
      document.removeEventListener('visibilitychange', refresh)
    }
  }, [reload])

  return <>
    <section className="panel">
      <h1>Watched sources</h1>
      <ActionForm action="/watches" className="fieldrow">
        <label>Name <TextField name="name" maxLength={100} required /></label>
        <label>Feed URL <TextField name="url" type="url" required /></label>
        <label>First check limit <TextField name="initial_batch" type="number" min="1" max="100" defaultValue="10" /></label>
        <button type="submit">Add watch</button>
      </ActionForm>
      <p className="hint">Pixiv and FANBOX creator pages, X and Bluesky artist media feeds, and Danbooru and Gelbooru tag searches.</p>
    </section>
    <section className="panel">
      {watches.length > 0 && <p className="hint">Enabled watches check automatically every 24 hours.</p>}
      {watches.length === 0 ? <><p className="hint">No watched sources yet.</p></> : null}
      {watches.map((watch, index) => <Fragment key={listKey(watch, index)}>
        <article className="panel inset" {...(watch.job ? { "data-job": watch.job.id, "data-state": watch.job.state } : {})}>
          <h2>{watch.name}{" "}{!(watch.enabled) ? <><span className="badge amber">paused</span></> : null}</h2>
          <p><a href={externalUrl(watch.url)} target="_blank" rel="noopener noreferrer">{watch.url}</a></p>
          <p className="hint">Last run {watch.last_run_at
            ? <time dateTime={watch.last_run_at}>{dateTime.format(new Date(watch.last_run_at))}</time>
            : 'Never'}</p>
          {watch.job ? <>
            <p><JobProgress initial={watch.job} /></p>
          </> : <>
            <p className="hint">First check will consider up to {watch.initial_batch} posts.</p>
          </>}
          <ActionForm className="inline" action={text("/watches/", watch.id, "/check")}>
            <button type="submit" data-watch-action="" {...(!(watch.enabled) ? { "data-watch-paused": "" } : {})} {...((!(watch.enabled) || (watch.job && watch.job.active)) ? { "disabled": true } : {})}>Check now</button>
          </ActionForm>
          <ActionForm className="inline" action={text("/watches/", watch.id, "/toggle")}>
            {watch.enabled ? <>
              <button className="ghost" type="submit" name="action" value="pause">Pause</button>
            </> : <>
              <button type="submit" name="action" value="resume">Resume</button>
            </>}
          </ActionForm>
          <ActionForm className="inline" action={text("/watches/", watch.id, "/remove")}>
            <button className="ghost" type="submit" data-watch-action="" {...((watch.job && watch.job.active) ? { "disabled": true } : {})}>Remove watch</button>
          </ActionForm>
        </article>
      </Fragment>)}
    </section>
  </>
}
