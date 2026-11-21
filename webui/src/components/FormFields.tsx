import { useEffect, useLayoutEffect, useRef, useState, type ComponentProps } from 'react'

export function TextField({ defaultValue, value, onChange, defaultChecked, checked, ...props }: ComponentProps<'input'>) {
  const [draft, setDraft] = useState(String(defaultValue ?? ''))
  const [selection, setSelection] = useState(defaultChecked ?? false)
  useEffect(() => {
    if (value == null) setDraft(String(defaultValue ?? ''))
  }, [defaultValue, value])
  useEffect(() => {
    if (checked == null) setSelection(defaultChecked ?? false)
  }, [checked, defaultChecked])
  const val = value == null ? draft : String(value)
  const checkbox = props.type === 'checkbox'
  const selected = checked ?? selection
  return <input {...props} value={checkbox ? value ?? 'on' : val} checked={checkbox ? selected : undefined}
    onChange={(event) => {
      if (checkbox) setSelection(event.target.checked)
      else setDraft(event.target.value)
      onChange?.(event)
    }} />
}

export function SelectField({ defaultValue, value, onChange, ...props }: ComponentProps<'select'>) {
  const [draft, setDraft] = useState<string>()
  const select = useRef<HTMLSelectElement>(null)
  useLayoutEffect(() => {
    if (value == null && defaultValue == null && draft == null && select.current) setDraft(select.current.value)
  }, [value, defaultValue, draft])
  useEffect(() => {
    if (value == null) setDraft(defaultValue == null ? undefined : String(defaultValue))
  }, [defaultValue, value])
  const val = value ?? draft ?? defaultValue
  return <select {...props} ref={select} value={val} onChange={(event) => {
    setDraft(event.target.value)
    onChange?.(event)
  }} />
}

export function TextArea({ defaultValue, value, onChange, ...props }: ComponentProps<'textarea'>) {
  const [draft, setDraft] = useState(String(defaultValue ?? ''))
  useEffect(() => {
    if (value == null) setDraft(String(defaultValue ?? ''))
  }, [defaultValue, value])
  const val = value == null ? draft : String(value)
  return <textarea {...props} value={val} onChange={(event) => {
    setDraft(event.target.value)
    onChange?.(event)
  }} />
}
