import type { Job } from '../api'
import { useActions } from '../context'
import { Badge } from './Badge'

export function useJob(initial: Job): Job {
  return useActions().jobs.get(initial.id) ?? initial
}

export function JobState({ state }: Pick<Job, 'state'>) {
  const tone = state === 'failed' ? 'red' : state === 'done' ? 'green' : undefined
  return <Badge tone={tone} data-role="state">{state}</Badge>
}

export function JobBar({ job }: { job: Job }) {
  if (!job.track) return <span className="hint">—</span>
  return <>
    <div className="progress" role="progressbar" aria-valuemin={0}
      aria-valuemax={job.total} aria-valuenow={job.progress}>
      <span style={{ width: job.percent + '%' }} />
    </div>
    <span className="hint" data-role="counts">{job.progress}/{job.total}</span>
  </>
}

export function JobProgress({ initial }: { initial: Job }) {
  const job = useJob(initial)
  return <span className="job-progress" aria-live="polite">
    <JobState state={job.state} />
    {job.total > 0 && <span className="hint">{job.progress}/{job.total}</span>}
    {job.message && <span>{job.message}</span>}
  </span>
}
