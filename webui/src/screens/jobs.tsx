import { PageLink } from '../components/PageLink'
import type { JobsPage } from '../api'
import { JobTable, useLiveJobs } from '../components/JobTable'

export function Jobs({ data }: { data: JobsPage }) {
  const { parallel, waiting } = data
  const jobs = useLiveJobs(data.jobs)
  return <section className="panel">
    <h2>Jobs</h2>
    <p className="hint">
      Publishing, uploading and importing all happen here, in the background: the page you
      clicked from returned at once and this list shows the real progress.
      {' '}{parallel} import{parallel === 1 ? '' : 's'} run in parallel{waiting > 0 ? ', ' + waiting + ' waiting' : ''}
      {' '}· raise <code>[import] max_concurrent</code> to do more at once.
    </p>
    <JobTable jobs={jobs} layout="history"
      empty={<p className="hint">No jobs yet. <PageLink href="/import">Import something</PageLink> or publish from the dashboard.</p>} />
    {jobs.length > 0 && <p className="hint" data-live-hint="">this list updates itself while something is running</p>}
  </section>
}
