import { useEffect, useId, useState, type ComponentProps, type KeyboardEvent } from 'react'
import { api, expired, type TagMatch } from '../api'
import { useActions } from '../context'

type Props = Pick<ComponentProps<'input'>, 'aria-label' | 'name' | 'placeholder' | 'onPaste' | 'enterKeyHint'> & {
  value: string
  onChange: (value: string) => void
  onPick: (tag: string) => void
  postId?: number
  exclude?: ReadonlySet<string>
}

export function TagAutocomplete({ value, onChange, onPick, postId, exclude, ...input }: Props) {
  const id = useId()
  const { expire } = useActions()
  const [focused, setFocused] = useState(false)
  const [dismissed, setDismissed] = useState(false)
  const [active, setActive] = useState<string>()
  const [result, setResult] = useState<{ query: string; tags: TagMatch[] }>()
  const query = value.trim().replace(/^#+/, '').trim()
  const lookup = focused && !dismissed && query !== ''
  const options = result?.query === query ? result.tags.filter(({ name }) => !exclude?.has(name)) : []
  const index = options.findIndex(({ name }) => name === active)
  const expanded = lookup && options.length > 0
  const activeId = expanded && index >= 0 ? id + '-' + index : undefined

  useEffect(() => {
    if (!lookup) return
    const controller = new AbortController()
    const timeout = window.setTimeout(async () => {
      const params = new URLSearchParams({ q: query })
      if (postId != null) params.set('post_id', String(postId))
      try {
        const tags = await api<TagMatch[]>('/tags/autocomplete?' + params, { signal: controller.signal })
        if (!controller.signal.aborted) setResult({ query, tags })
      } catch (error) {
        if (controller.signal.aborted) return
        if (expired(error)) expire()
        else setResult(undefined)
      }
    }, 150)
    return () => { window.clearTimeout(timeout); controller.abort() }
  }, [expire, lookup, postId, query])

  useEffect(() => {
    if (activeId) document.getElementById(activeId)?.scrollIntoView({ block: 'nearest' })
  }, [activeId])

  function close() {
    setDismissed(true)
    setActive(undefined)
  }

  function pick(tag: string) {
    close()
    onPick(tag)
  }

  function keyDown(event: KeyboardEvent<HTMLInputElement>) {
    if (event.nativeEvent.isComposing) return
    if (event.key === 'Escape' && lookup) {
      event.preventDefault()
      close()
    } else if (event.key === 'Enter') {
      if (expanded && index >= 0) {
        event.preventDefault()
        pick(options[index].name)
      } else close()
    } else if (options.length && (event.key === 'ArrowDown' || event.key === 'ArrowUp')) {
      event.preventDefault()
      setDismissed(false)
      const next = event.key === 'ArrowDown' ? (index + 1) % options.length
        : index <= 0 ? options.length - 1 : index - 1
      setActive(options[next].name)
    }
  }

  return <div className="tag-autocomplete" data-expanded={expanded || undefined}>
    <input {...input} type="text" role="combobox" value={value} aria-autocomplete="list"
      aria-expanded={expanded} aria-controls={expanded ? id : undefined} aria-activedescendant={activeId}
      autoComplete="off" autoCapitalize="none" spellCheck={false}
      onFocus={() => { setFocused(true); setDismissed(false) }}
      onBlur={() => { setFocused(false); setActive(undefined) }}
      onKeyDown={keyDown} onChange={(event) => {
        setDismissed(false)
        setActive(undefined)
        onChange(event.target.value)
      }} />
    {expanded && <ul id={id} role="listbox" aria-label="Matching tags" className="tag-matches">
      {options.map(({ name, count }, position) => <li key={name} id={id + '-' + position}
        role="option" aria-selected={index === position} onMouseDown={(event) => event.preventDefault()}
        onClick={() => pick(name)}>
        <span>{name}</span><span className="hint">{count} {count === 1 ? 'post' : 'posts'}</span>
      </li>)}
    </ul>}
  </div>
}
