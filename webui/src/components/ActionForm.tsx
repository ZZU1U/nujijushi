import { useRef, useState, type FormEvent, type FormHTMLAttributes, type ReactNode } from 'react'
import { action, expired, friendly, type ActionFields, type ActionResult } from '../api'
import { useActions } from '../context'
import { SelectField, TextField } from './FormFields'

export { SelectField, TextField }
export { TextArea } from './FormFields'

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

type FormProps = Omit<FormHTMLAttributes<HTMLFormElement>, 'action' | 'onSubmit'> & {
  action: string
  fields?: ActionFields
  onSaved?: (result: ActionResult) => void
  onJobDone?: () => void
  feedback?: 'inline' | 'notice'
  children?: ReactNode
}

function formValues(form: HTMLFormElement, fields: ActionFields | undefined, button: HTMLElement | null) {
  const values = new URLSearchParams()
  for (const [name, value] of new FormData(form).entries()) {
    if (typeof value === 'string') values.set(name, value)
  }
  for (const [name, value] of Object.entries(fields ?? {})) {
    if (value != null) values.set(name, String(value))
    else values.delete(name)
  }
  if (button instanceof HTMLButtonElement && button.name) values.set(button.name, button.value)
  return values
}

export function ActionForm({
  action: endpoint, fields, onSaved, onJobDone, feedback = 'notice', children, ...props
}: FormProps) {
  const page = useActions()
  const pending = useRef(false)
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
    const values = formValues(form, fields, button)
    pending.current = true
    setBusy(true)
    setFailed(false)
    setMessage(form.hasAttribute('data-verify-destination') && values.get('tg_id')?.trim() !== form.dataset.verifyDestination
      ? 'Checking Telegram…' : 'Working…')
    try {
      const result = await action(endpoint, Object.fromEntries(values), page.csrf)
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
    {children}
    {message && <span className={'action-feedback' + (failed ? ' action-error' : '')}
      role={failed ? 'alert' : 'status'}>{message}</span>}
  </form>
}
