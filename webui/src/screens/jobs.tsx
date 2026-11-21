import { Fragment } from 'react'
import { ActionForm, TextField, JobBar, JobState, useActions, text, listKey } from '../ui'
import type { JobsPage } from '../api'

export function Jobs({ data }: { data: JobsPage }) {
  const { jobs: initialJobs, parallel, waiting } = data
  const { jobs: updates } = useActions()
  const jobs = initialJobs.map((job) => updates.get(job.id) ?? job)
  return <>
    <section className="panel">
      <h2>jobs</h2>
      <p className="hint">
        Publishing, uploading and importing all happen here, in the background: the page you
        clicked from returned at once and this list shows the real progress.
        {parallel} import{((parallel === 1) ? "" : "s")} run{((parallel === 1) ? "" : "")} in parallel{waiting ? <>,
          {waiting} waiting</> : null} — raise <code>[import] max_concurrent</code> to do more at once.
      </p>
      {jobs.length > 0 ? <>
        <div className="job-table-scroll" role="region" aria-label="Job history" tabIndex={0}>
          <table className="compact job-table">
            <thead><tr><th>#</th><th>kind</th><th>target</th><th>state</th><th>progress</th><th>result</th><th>age</th><th></th></tr></thead>
            <tbody>
              {jobs.map((j, index) => <Fragment key={listKey(j, index)}>
                <tr data-job={j.id} data-state={j.state}>
                  <td>{j.id}</td>
                  <td><span className="badge">{j.kind_label}</span></td>
                  <td className="hint job-target">{j.payload}</td>
                  <td><JobState state={j.state} /></td>
                  <td className="progress-cell">
                    <JobBar job={j} />
                  </td>
                  <td className="hint job-result" data-role="message">{j.message}</td>
                  <td className="hint" data-role="age">{j.age}</td>
                  <td className="actions">
                    {j.post_id ? <><a className="button small ghost" href={text("/posts/", j.post_id)}>post</a></> : null}
                    {j.retryable ? <>
                      <ActionForm action="/jobs/retry" className="inline">
                        <TextField type="hidden" name="id" value={j.id} />
                        <button className="small" type="submit" title="run this import again as a new job">retry</button>
                      </ActionForm>
                    </> : null}
                  </td>
                </tr>
              </Fragment>)}
            </tbody>
          </table>
        </div>
        <p className="hint" data-live-hint="">this list updates itself while something is running</p>
      </> : <>
        <p className="hint">No jobs yet. <a href="/import">Import something</a> or publish from the dashboard.</p>
      </>}
    </section>
  </>
}
