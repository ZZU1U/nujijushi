const basePath = document.querySelector<HTMLMetaElement>('meta[name="nujijushi-base-path"]')?.content ?? ''

export const appUrl = (path: string) => basePath + path
export const apiUrl = (path: string) => appUrl('/api/v1' + path)

export function appPath(pathname: string) {
  if (pathname === basePath) return '/'
  if (pathname.startsWith(basePath + '/')) return pathname.slice(basePath.length)
  return null
}
