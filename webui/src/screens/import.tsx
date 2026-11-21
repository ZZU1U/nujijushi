import { Fragment, useState } from 'react'
import { ActionForm, TextField, TextArea, JobBar, JobState, useActions, text, listKey } from '../ui'
import type { ImportPage } from '../api'

export function Import({ data }: { data: ImportPage }) {
  const { jobs: initialJobs, parallel, waiting, cookies } = data
  const { jobs: updates } = useActions()
  const jobs = initialJobs.map((job) => updates.get(job.id) ?? job)
  const [targets, setTargets] = useState('')
  return <>
    <section className="panel">
      <h2>import artwork</h2>
      <p className="hint">
        One target per line: a post URL (<code>https://www.pixiv.net/artworks/…</code>,
        <code>https://x.com/user/status/…</code>) or a folder on this machine.
        Cookies are read from <code>{cookies}</code> when a site needs them.
      </p>
      <ActionForm action="/import" fields={{ targets }} onSaved={(result) => {
        if (result.job_ids.length) setTargets((draft) => draft === targets ? '' : draft)
      }}>
        <TextArea name={"targets"} rows={6} spellCheck={"false"} placeholder={"https://www.pixiv.net/artworks/123456\nhttps://x.com/someone/status/1234567890\n/Users/me/Pictures/dump/set-01"} value={targets} onChange={(event) => setTargets(event.target.value)}></TextArea>
        <div className="fieldrow">
          <button type="submit">start import</button>
          <span className="hint">the worker picks jobs up within a couple of seconds</span>
        </div>
      </ActionForm>
      <p className="hint">
        {parallel} import{((parallel === 1) ? "" : "s")} run{((parallel === 1) ? "" : "")} in
        parallel{waiting ? <>, {waiting} waiting</> : null}.
        Each target is one job; a big batch therefore continues while you browse.
        Raise <code>[import] max_concurrent</code> to run more at once — gallery-dl is
        network and disk heavy, so more parallelism is not always faster.
      </p>
    </section>

    <section className="panel">
      <h2>jobs <a className="hint" href="/jobs">all jobs →</a></h2>
      {jobs.length > 0 ? <>
        <div className="responsive-table" tabIndex={0}><table className="compact">
          <thead><tr><th>#</th><th>state</th><th>target</th><th>progress</th><th>result</th><th>started</th></tr></thead>
          <tbody>
            {jobs.map((j, index) => <Fragment key={listKey(j, index)}>
              <tr data-job={j.id} data-state={j.state}>
                <td>{j.id}</td>
                <td><JobState state={j.state} /></td>
                <td className="hint">{j.payload}</td>
                <td className="progress-cell">
                  <JobBar job={j} />
                </td>
                <td className="hint"><span data-role="message">{j.message}</span>
                  {j.post_id ? <><a href={text("/posts/", j.post_id)}>Open post</a></> : null}
                  {j.retryable ? <><ActionForm action="/jobs/retry">
                    <TextField type="hidden" name="id" value={j.id} />
                    <TextField type="hidden" name="back" value="/import" />
                    <button type="submit" className="small">Retry import</button>
                  </ActionForm></> : null}
                </td>
                <td className="hint">{j.created}</td>
              </tr>
            </Fragment>)}
          </tbody>
        </table></div>
        <p className="hint" data-live-hint="">this list updates itself while something is running</p>
      </> : <>
        <p className="hint">No jobs yet.</p>
      </>}
    </section>
  </>
}
