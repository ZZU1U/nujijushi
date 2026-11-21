import type { ReactNode } from 'react'
import type { Job } from '../api'
import { ActionForm, JobBar, JobState, useActions } from '../ui'
import { Badge } from './Badge'
import { DataTable, type Column } from './DataTable'

function RetryImport({ job, fromImport = false }: { job: Job; fromImport?: boolean }) {
  if (!job.retryable) return null
  return <ActionForm action="/jobs/retry" className={fromImport ? undefined : 'inline'}
    fields={{ id: job.id, back: fromImport ? '/import' : undefined }}>
    <button type="submit" className="small" title="Run this import again as a new job">
      {fromImport ? 'Retry import' : 'retry'}
    </button>
  </ActionForm>
}

const id: Column<Job> = { label: '#', cell: (job) => job.id }
const kind: Column<Job> = { label: 'kind', cell: (job) => <Badge>{job.kind_label}</Badge> }
const target: Column<Job> = { label: 'target', className: 'hint job-target', cell: (job) => job.payload }
const state: Column<Job> = { label: 'state', cell: (job) => <JobState state={job.state} /> }
const progress: Column<Job> = {
  label: 'progress', className: 'progress-cell', cell: (job) => <JobBar job={job} />,
}
const result: Column<Job> = {
  label: 'result', className: 'hint job-result', cell: (job) => <span data-role="message">{job.message}</span>,
}

const history: Column<Job>[] = [
  id, kind, target, state, progress, result,
  { label: 'age', className: 'hint', cell: (job) => <span data-role="age">{job.age}</span> },
  {
    label: 'actions', hideLabel: true, className: 'actions',
    cell: (job) => <>
      {job.post_id && <a className="button small ghost" href={'/posts/' + job.post_id}>post</a>}
      <RetryImport job={job} />
    </>,
  },
]
const imports: Column<Job>[] = [
  id, state, target, progress,
  {
    ...result,
    cell: (job) => <>
      <span data-role="message">{job.message}</span>
      {job.post_id && <a href={'/posts/' + job.post_id}>Open post</a>}
      <RetryImport job={job} fromImport />
    </>,
  },
  { label: 'started', className: 'hint', cell: (job) => job.created },
]
const recent: Column<Job>[] = [
  { ...id, cell: (job) => '#' + job.id }, kind, state, progress, { ...result, className: 'hint job-message' },
]
const layouts = {
  history: { label: 'Job history', columns: history, className: 'job-table' },
  imports: { label: 'Import jobs', columns: imports },
  recent: { label: 'Recent jobs', columns: recent, className: 'dashboard-jobs' },
}

export function useLiveJobs(jobs: Job[]) {
  const { jobs: updates } = useActions()
  return jobs.map((job) => updates.get(job.id) ?? job)
}

export function JobTable({ jobs, layout, empty }: {
  jobs: Job[]
  layout: keyof typeof layouts
  empty: ReactNode
}) {
  return <DataTable {...layouts[layout]} rows={jobs} compact hideHeader={layout === 'recent'} empty={empty}
    rowKey={(job) => job.id} rowAttributes={(job) => ({ 'data-job': job.id, 'data-state': job.state })} />
}
