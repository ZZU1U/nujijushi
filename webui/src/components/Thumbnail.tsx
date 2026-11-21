import { useState, type ComponentProps } from 'react'

export function thumbnailUrl(hash: string) {
  return '/thumb/' + hash
}

interface ThumbnailProps extends Omit<ComponentProps<'img'>, 'src' | 'loading' | 'decoding' | 'onError'> {
  hash: string
  fallback?: string
}

export function Thumbnail({ hash, fallback, ...props }: ThumbnailProps) {
  const [unavailable, setUnavailable] = useState<string>()
  const useOriginal = fallback !== undefined && unavailable === hash
  return <img {...props} loading="lazy" decoding="async"
    src={useOriginal ? fallback : thumbnailUrl(hash)}
    onError={fallback && !useOriginal ? () => setUnavailable(hash) : undefined} />
}
