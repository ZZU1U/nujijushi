export function Timezones({ zones }: { zones: string[] }) {
  return <datalist id="tz-zones">{zones.map((zone) => <option key={zone} value={zone} />)}</datalist>
}
