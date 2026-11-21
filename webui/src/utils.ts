export const text = (...values: unknown[]) => values.map((value) => value == null ? '' : String(value)).join('')
export const capitalize = (value: string) => value.charAt(0).toUpperCase() + value.slice(1)

export function listKey(item: unknown, index: number) {
  if (item && typeof item === 'object' && 'id' in item) return String(item.id)
  if (typeof item === 'string' || typeof item === 'number') return item
  return index
}

export function externalUrl(value: string | null | undefined) {
  if (!value) return undefined
  try {
    const url = new URL(value)
    if (url.protocol === 'https:' || url.protocol === 'http:') return url.href
  } catch { /* Display the label without a link. */ }
}
