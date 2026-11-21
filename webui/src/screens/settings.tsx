import { useState, type FormEvent } from 'react'
import { ActionForm, JobProgress, useActions } from '../ui'
import { ChannelSelect } from '../components/ChannelSelect'
import { api, expired, friendly, type CleanupPreview, type SettingsPage } from '../api'

const archiveBytes = new Intl.NumberFormat(undefined, { notation: 'compact', style: 'unit', unit: 'byte' })

export function Settings({ data }: { data: SettingsPage }) {
  const { channels, default_channel } = data
  return <>
    <section className="panel">
      <h2>Settings</h2>
      <ActionForm action="/settings">
        <label>Default queue channel
          <ChannelSelect channels={channels} defaultValue={default_channel ?? ''} name="channel" emptyLabel="No default" />
        </label>
        <button type="submit">Save settings</button>
      </ActionForm>
    </section>
    <StorageCleanup job={data.cleanup_job} />
  </>
}

function StorageCleanup({ job }: { job: SettingsPage['cleanup_job'] }) {
  const page = useActions()
  const [days, setDays] = useState('30')
  const [preview, setPreview] = useState<CleanupPreview>()
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState('')
  const currentJob = job && (page.jobs.get(job.id) ?? job)
  const active = currentJob?.active ?? false

  async function check(event: FormEvent<HTMLFormElement>) {
    event.preventDefault()
    setLoading(true)
    setError('')
    setPreview(undefined)
    try {
      setPreview(await api<CleanupPreview>('/storage/cleanup?older_than_days=' + encodeURIComponent(days)))
    } catch (reason) {
      if (expired(reason)) page.expire()
      else setError(friendly(reason))
    } finally {
      setLoading(false)
    }
  }

  return <section className="panel">
    <h2>Storage cleanup</h2>
    <p className="hint">Keep the edited gallery of confirmed sent posts and reclaim the files retained for undo.</p>
    <form className="post-tool-row" onSubmit={check}>
      <label>Days since latest confirmed send
        <input type="number" min="0" step="1" required value={days} disabled={loading || active}
          onChange={(event) => { setDays(event.target.value); setPreview(undefined) }} />
      </label>
      <button type="submit" disabled={loading || active}>{loading ? 'Checking…' : 'Preview cleanup'}</button>
    </form>
    {error && <p role="alert" className="action-error">{error}</p>}
    {preview && <div aria-live="polite">
      <p>{preview.posts} eligible post(s) · {preview.suggestions} suggestion(s) · {archiveBytes.format(preview.estimated_bytes)} estimated reclaimable</p>
      <ActionForm action="/storage/cleanup" fields={preview.confirmation} onSaved={() => setPreview(undefined)}
        data-confirm="Keep the edited galleries and remove their undo files and unsaved suggestions? To get the originals again, re-import the post from its source.">
        <button type="submit" className="danger" disabled={active || (preview.posts === 0 && preview.files === 0)}>Apply cleanup</button>
      </ActionForm>
    </div>}
    {currentJob && <JobProgress initial={currentJob} />}
  </section>
}
