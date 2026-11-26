import type { AnchorHTMLAttributes } from 'react'
import { appUrl } from '../paths'

export function PageLink({ href, ...props }: AnchorHTMLAttributes<HTMLAnchorElement>) {
  return <a {...props} href={href && appUrl(href)} />
}
