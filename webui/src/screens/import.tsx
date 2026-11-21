import { useState } from 'react'
import { ActionForm, TextArea } from '../ui'
import { JobTable, useLiveJobs } from '../components/JobTable'
import type { ImportPage } from '../api'

export function Import({ data }: { data: ImportPage }) {
  const { parallel, waiting, cookies } = data
  const jobs = useLiveJobs(data.jobs)
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
      <JobTable jobs={jobs} layout="imports" empty={<p className="hint">No jobs yet.</p>} />
      {jobs.length > 0 && <p className="hint" data-live-hint="">this list updates itself while something is running</p>}
    </section>
  </>
}
