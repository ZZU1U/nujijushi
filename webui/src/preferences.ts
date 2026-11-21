import { useSyncExternalStore } from 'react'

const thumbnailsOnlyKey = 'nujijushi.thumbnails-only'
const preferenceEvent = 'nujijushi-preferences-changed'

function readThumbnailsOnly() {
  return localStorage.getItem(thumbnailsOnlyKey) === 'true'
}

function subscribe(listener: () => void) {
  const notify = () => listener()
  window.addEventListener('storage', notify)
  window.addEventListener(preferenceEvent, notify)
  return () => {
    window.removeEventListener('storage', notify)
    window.removeEventListener(preferenceEvent, notify)
  }
}

export function useThumbnailsOnly() {
  const enabled = useSyncExternalStore(subscribe, readThumbnailsOnly, () => false)
  function setEnabled(next: boolean) {
    localStorage.setItem(thumbnailsOnlyKey, String(next))
    window.dispatchEvent(new Event(preferenceEvent))
  }
  return [enabled, setEnabled] as const
}
