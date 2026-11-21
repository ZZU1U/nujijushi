import type { ComponentProps } from 'react'

export function thumbnailUrl(hash: string) {
  return '/thumb/' + hash
}

interface ThumbnailProps extends Omit<ComponentProps<'img'>, 'src' | 'loading' | 'decoding'> {
  hash: string
}

export function Thumbnail({ hash, ...props }: ThumbnailProps) {
  return <img {...props} loading="lazy" decoding="async" src={thumbnailUrl(hash)} />
}
