import { createContext, createElement, memo, useContext, useLayoutEffect, useRef, useState, type ComponentProps, type FormEvent, type FormHTMLAttributes, type ReactNode } from 'react'
import { action, expired, friendly, type ActionFields, type ActionResult, type Job } from './api'

export interface PageActions {
  csrf: string
  path: string
  navigate: (path: string, replace?: boolean) => void
  refresh: () => void
  expire: () => void
  notice: (message: string) => void
  follow: (id: number) => Promise<Job | undefined>
  jobs: ReadonlyMap<number, Job>
  onReady?: () => void
}

export const Actions = createContext<PageActions | null>(null)

export function useActions() {
  const context = useContext(Actions)
  if (!context) throw new Error('Missing admin page context')
  return context
}

export const text = (...values: unknown[]) => values.map((value) => value == null ? '' : String(value)).join('')
export const capitalize = (value: string) => value.charAt(0).toUpperCase() + value.slice(1)
export function listKey(item: unknown, index: number) {
  if (item && typeof item === 'object' && 'id' in item) return String(item.id)
  if (typeof item === 'string' || typeof item === 'number') return item
  return index
}

export function externalUrl(value: string | null) {
  if (!value) return undefined
  try {
    const url = new URL(value)
    if (url.protocol === 'https:' || url.protocol === 'http:') return url.href
  } catch { /* Display the label without a link. */ }
}

export function Timezones({ zones }: { zones: string[] }) {
  return <datalist id="tz-zones">{zones.map((zone) => <option key={zone} value={zone} />)}</datalist>
}

export function CompressionOptions({ merge = false }: { merge?: boolean }) {
  const [mode, setMode] = useState(merge ? 'lossless' : 'fit')
  return <div>
    <label>{merge ? 'Output' : 'Mode'}
      <SelectField name="output" value={mode} onChange={(event) => setMode(event.target.value)}>
        {merge && <option value="lossless">Lossless PNG</option>}
        <option value="fit">Fit to Telegram</option>
        <option value="quality">Use exact JPEG quality</option>
      </SelectField>
    </label>
    <label data-jpeg-quality="" hidden={mode !== 'quality'}>
      JPEG quality <TextField name="quality" type="number" defaultValue="85" min="1" max="100"
        disabled={mode !== 'quality'} required={mode === 'quality'} />
    </label>
    <p className="hint">Fit to Telegram adjusts quality or size automatically and may add borders. JPEG fills transparency with white.</p>
  </div>
}

const captionTags = new Set(['b', 'strong', 'i', 'em', 'u', 'ins', 's', 'strike', 'del', 'code', 'pre', 'blockquote', 'br'])
const discardedTags = new Set(['script', 'style', 'iframe', 'object', 'embed', 'template'])

function captionNodes(parent: ParentNode): ReactNode[] {
  return Array.from(parent.childNodes, (node, key) => {
    if (node.nodeType === Node.TEXT_NODE) return node.textContent
    if (!(node instanceof Element) || discardedTags.has(node.localName)) return null
    const tag = node.localName
    if (tag === 'br') return <br key={key} />
    const children = captionNodes(node)
    if (tag === 'a') {
      const href = externalUrl(node.getAttribute('href'))
      return href ? <a key={key} href={href} target="_blank" rel="noopener noreferrer">{children}</a> : <span key={key}>{children}</span>
    }
    if (tag === 'tg-spoiler' || (tag === 'span' && node.classList.contains('tg-spoiler'))) {
      return <span key={key} className="tg-spoiler">{children}</span>
    }
    return captionTags.has(tag) ? createElement(tag, { key }, children) : <span key={key}>{children}</span>
  })
}

export const CaptionPreview = memo(function CaptionPreview({ caption }: { caption: string }) {
  const template = document.createElement('template')
  template.innerHTML = caption
  return <div className="caption">{captionNodes(template.content)}</div>
})

export function useJob(initial: Job): Job {
  return useActions().jobs.get(initial.id) ?? initial
}

export function JobState({ state }: Pick<Job, 'state'>) {
  const color = state === 'failed' ? 'red' : state === 'done' ? 'green' : ''
  return <span className={'badge ' + color} data-role="state">{state}</span>
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

const FormValues = createContext<Map<string, ActionFields[string]> | null>(null)

function useFormValue(name: string | undefined, value: ActionFields[string]) {
  const fields = useContext(FormValues)
  useLayoutEffect(() => {
    if (fields && name) {
      fields.set(name, value)
      return () => { fields.delete(name) }
    }
  }, [fields, name, value])
}

export function TextField({ defaultValue, value, onChange, defaultChecked, checked, ...props }: ComponentProps<'input'>) {
  const [draft, setDraft] = useState(String(defaultValue ?? ''))
  const [selection, setSelection] = useState(defaultChecked ?? false)
  const val = value == null ? draft : String(value)
  const checkbox = props.type === 'checkbox'
  const selected = checked ?? selection
  useFormValue(props.name, props.disabled || (checkbox && !selected) ? undefined : checkbox ? String(value ?? 'on') : val)
  return <input {...props} value={checkbox ? value ?? 'on' : val} checked={checkbox ? selected : undefined}
    onChange={(event) => {
      if (checkbox) setSelection(event.target.checked)
      else setDraft(event.target.value)
      onChange?.(event)
    }} />
}

export function SelectField({ defaultValue, value, onChange, ...props }: ComponentProps<'select'>) {
  const [draft, setDraft] = useState<string>()
  // A select without an explicit default uses its first enabled option.
  const select = useRef<HTMLSelectElement>(null)
  useLayoutEffect(() => {
    if (value == null && defaultValue == null && draft == null && select.current) setDraft(select.current.value)
  }, [value, defaultValue, draft])
  const val = value ?? draft ?? defaultValue
  useFormValue(props.name, props.disabled || val == null ? undefined : String(val))
  return <select {...props} ref={select} value={val} onChange={(event) => {
    setDraft(event.target.value)
    onChange?.(event)
  }} />
}

export function TextArea({ defaultValue, value, onChange, ...props }: ComponentProps<'textarea'>) {
  const [draft, setDraft] = useState(String(defaultValue ?? ''))
  const val = value == null ? draft : String(value)
  useFormValue(props.name, props.disabled ? undefined : val)
  return <textarea {...props} value={val} onChange={(event) => {
    setDraft(event.target.value)
    onChange?.(event)
  }} />
}

type FormProps = Omit<FormHTMLAttributes<HTMLFormElement>, 'action' | 'onSubmit'> & {
  action: string
  fields?: ActionFields
  onSaved?: (result: ActionResult) => void
  onJobDone?: () => void
  feedback?: 'inline' | 'notice'
}

export function ActionForm({
  action: endpoint, fields, onSaved, onJobDone, feedback = 'notice', children, ...props
}: FormProps) {
  const page = useActions()
  const pending = useRef(false)
  const formValues = useRef(new Map<string, ActionFields[string]>())
  const path = useRef(page.path)
  path.current = page.path
  const [busy, setBusy] = useState(false)
  const [message, setMessage] = useState('')
  const [failed, setFailed] = useState(false)

  async function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault()
    if (pending.current) return
    const form = event.currentTarget
    const submittedAt = page.path
    const button = (event.nativeEvent as SubmitEvent).submitter
    const confirm = button?.dataset.confirm || form.dataset.confirm
    if (confirm && !window.confirm(confirm)) return
    const values: ActionFields = { ...Object.fromEntries(formValues.current), ...fields }
    if (button instanceof HTMLButtonElement && button.name) values[button.name] = button.value
    pending.current = true
    setBusy(true)
    setFailed(false)
    setMessage(form.hasAttribute('data-verify-destination') && String(values.tg_id ?? '').trim() !== form.dataset.verifyDestination
      ? 'Checking Telegram…' : 'Working…')
    try {
      const result = await action(endpoint, values, page.csrf)
      if (!form.isConnected || path.current !== submittedAt) return
      if (endpoint === '/logout') {
        page.expire()
        return
      }
      onSaved?.(result)
      if (result.location && result.location.split('?')[0] !== page.path.split('?')[0]) {
        page.navigate(result.location)
      } else if (feedback !== 'inline' || result.job_ids.length === 0) {
        page.refresh()
      }
      setMessage(feedback === 'inline' ? result.message : '')
      if (feedback === 'notice') page.notice(result.message)
      const jobs = await Promise.all(result.job_ids.map(page.follow))
      const failure = jobs.find((job) => job?.state === 'failed')
      if (form.isConnected && path.current === submittedAt && jobs.some((job) => job?.state === 'done')) {
        onJobDone?.()
        page.refresh()
      }
      if (form.isConnected && failure && feedback === 'inline') {
        setFailed(true)
        setMessage(failure.message || 'The job failed')
      }
    } catch (error) {
      if (!form.isConnected || path.current !== submittedAt) return
      if (expired(error)) page.expire()
      else {
        setMessage(friendly(error))
        setFailed(true)
      }
    } finally {
      pending.current = false
      setBusy(false)
    }
  }

  return <form {...props} method="post" action={'/api/v1' + endpoint} aria-busy={busy || undefined} onSubmit={submit}>
    <FormValues.Provider value={formValues.current}>{children}</FormValues.Provider>
    {message && <span className={'action-feedback' + (failed ? ' action-error' : '')}
      role={failed ? 'alert' : 'status'}>{message}</span>}
  </form>
}
