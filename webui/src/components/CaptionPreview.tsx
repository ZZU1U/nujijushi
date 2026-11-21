import { createElement, memo, type ReactNode } from 'react'
import { externalUrl } from '../utils'

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
