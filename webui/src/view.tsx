import { createElement, useState, type ReactNode } from 'react'

const names: Record<string, string> = {
  class: 'className', for: 'htmlFor', tabindex: 'tabIndex', colspan: 'colSpan',
  rowspan: 'rowSpan', maxlength: 'maxLength', minlength: 'minLength',
  autocomplete: 'autoComplete', autofocus: 'autoFocus', inputmode: 'inputMode',
  spellcheck: 'spellCheck', readonly: 'readOnly', datetime: 'dateTime',
  crossorigin: 'crossOrigin', referrerpolicy: 'referrerPolicy', srcset: 'srcSet',
}
const booleans = new Set(['autofocus', 'checked', 'controls', 'disabled', 'hidden', 'multiple',
  'novalidate', 'open', 'readonly', 'required', 'selected'])
const voidTags = new Set(['area', 'br', 'col', 'embed', 'hr', 'img', 'input', 'source', 'track', 'wbr'])

export interface Job {
  id: number
  kind: string
  state: string
  message?: string
  progress: number
  total: number
  percent: number
  age?: string
}

interface RenderOptions {
  jobs?: ReadonlyMap<number, Job>
  job?: Job
  compression?: { mode: string; setMode: (mode: string) => void }
}

function CompressionOptions({ source }: { source: Element }) {
  const [mode, setMode] = useState(source.querySelector('select')?.value || '')
  return <div data-compression-options>{viewNodes(source, { compression: { mode, setMode } })}</div>
}

function jobText(job: Job, role: string | null) {
  switch (role) {
    case 'state': return job.state
    case 'message': return job.message || ''
    case 'counts': return job.total ? job.progress + '/' + job.total : ''
    case 'age': return job.age || ''
    default: return undefined
  }
}

function stableKey(node: Element, index: number) {
  if (node.id) return node.id
  for (const attribute of ['data-queue', 'data-job', 'data-publish-job']) {
    const id = node.getAttribute(attribute)
    if (id) return attribute + ':' + id
  }
  return node.localName + index
}

// React owns the document tree. Axum provides the route's view markup so the
// existing policy, escaping and control visibility are shared with the fallback.
export function viewNodes(parent: ParentNode, options: RenderOptions = {}): ReactNode[] {
  return Array.from(parent.childNodes).map((node, index) => {
    if (node.nodeType === Node.TEXT_NODE) return node.textContent
    if (!(node instanceof Element)) return null
    const tag = node.localName
    if (['script', 'iframe', 'object', 'embed', 'style', 'link'].includes(tag)) return null
    const key = stableKey(node, index)
    if (node.hasAttribute('data-compression-options')) return <CompressionOptions key={key} source={node} />
    const props: Record<string, unknown> = { key }
    const id = node.getAttribute('data-job')
    const initialState = node.getAttribute('data-state')
    const job = id ? (initialState === 'running' || initialState === 'pending' ? options.jobs?.get(Number(id)) : undefined) : options.job
    const role = node.getAttribute('data-role')
    for (const attr of node.attributes) {
      if (attr.name.startsWith('on') || attr.name === 'style' || attr.name === 'selected') continue
      if (['href', 'src', 'action'].includes(attr.name) && /^\s*(?:javascript|vbscript|data):/i.test(attr.value)) continue
      const name = names[attr.name] || attr.name
      props[name] = booleans.has(attr.name) ? true : attr.value
    }
    if (node instanceof HTMLElement && node.style.length) {
      props.style = Object.fromEntries(Array.from(node.style).map((key) => [
        key.startsWith('--') ? key : key.replace(/-([a-z])/g, (_, letter: string) => letter.toUpperCase()),
        node.style.getPropertyValue(key),
      ]))
    }
    if (node instanceof HTMLInputElement) {
      if (props.checked) props.defaultChecked = true
      delete props.checked
      if (props.value != null && !['radio', 'checkbox', 'hidden', 'submit'].includes(node.type)) {
        props.defaultValue = props.value
        delete props.value
      } else if (node.type === 'hidden') {
        props.readOnly = true
      }
    }
    if (node instanceof HTMLTextAreaElement) props.defaultValue = node.value
    if (node instanceof HTMLSelectElement) {
      if (options.compression && node.name === 'output') {
        const controls = options.compression
        props.value = controls.mode
        props.onChange = (event: React.ChangeEvent<HTMLSelectElement>) => controls.setMode(event.target.value)
      } else {
        props.defaultValue = node.value
      }
    }
    if (options.compression && node.hasAttribute('data-jpeg-quality')) props.hidden = options.compression.mode !== 'quality'
    if (options.compression && node instanceof HTMLInputElement && node.name === 'quality') {
      props.disabled = options.compression.mode !== 'quality'
      props.required = !props.disabled
    }
    if (job && role === 'state') {
      const classes = node.className.split(' ').filter((name) => name !== 'red' && name !== 'green')
      if (job.state === 'failed') classes.push('red')
      if (job.state === 'done') classes.push('green')
      props.className = classes.join(' ')
      props['data-live-state'] = job.state
    }
    if (job && node.getAttribute('role') === 'progressbar') {
      props['aria-valuenow'] = job.progress
      props['aria-valuemax'] = job.total
    }
    if (job && node.parentElement?.getAttribute('role') === 'progressbar') {
      props.style = { width: job.percent + '%' }
    }
    if (props['data-live-state']) props.key = key + ':' + props['data-live-state']
    if (tag === 'textarea' || voidTags.has(tag)) return createElement(tag, props)

    const text = job && jobText(job, role)
    const children = typeof text === 'string' ? [text] : viewNodes(node, id ? { ...options, job } : options)
    if (tag === 'form' && node.getAttribute('method')?.toLowerCase() === 'post') {
      children.push(<span key="result" className="action-feedback" data-action-feedback role="status" hidden />)
    }
    const result = createElement(tag, props, children)
    return tag === 'table' && !node.closest('.job-table-scroll')
      ? <div className="responsive-table" tabIndex={0} key={props.key as string}>{result}</div>
      : result
  })
}
