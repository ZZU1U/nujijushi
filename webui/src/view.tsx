import { createElement, type ReactNode } from 'react'

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

// React owns the document tree. Axum provides the route's view markup so the
// existing policy, escaping and control visibility are shared with the fallback.
export function viewNodes(parent: ParentNode): ReactNode[] {
  return Array.from(parent.childNodes).map((node, index) => {
    if (node.nodeType === Node.TEXT_NODE) return node.textContent
    if (!(node instanceof Element)) return null
    const tag = node.localName
    if (['script', 'iframe', 'object', 'embed', 'style', 'link'].includes(tag)) return null
    const props: Record<string, unknown> = { key: node.id || tag + index }
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
    if (node instanceof HTMLSelectElement) props.defaultValue = node.value
    if (tag === 'textarea' || voidTags.has(tag)) return createElement(tag, props)

    const children = viewNodes(node)
    if (tag === 'form' && node.getAttribute('method')?.toLowerCase() === 'post') {
      children.push(<span key="result" className="action-feedback" data-action-feedback role="status" hidden />)
    }
    const result = createElement(tag, props, children)
    return tag === 'table' && !node.closest('.job-table-scroll')
      ? <div className="responsive-table" tabIndex={0} key={props.key as string}>{result}</div>
      : result
  })
}
