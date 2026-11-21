import { ActionForm } from '../ui'
import { ChannelSelect } from '../components/ChannelSelect'
import type { SettingsPage } from '../api'

export function Settings({ data }: { data: SettingsPage }) {
  const { channels, default_channel } = data
  return <>
    <section className="panel">
      <h2>Settings</h2>
      <ActionForm action="/settings">
        <label>Default queue channel
          <ChannelSelect channels={channels} defaultValue={default_channel ?? ''} name="channel" emptyLabel="No default" />
        </label>
        <button type="submit">Save settings</button>
      </ActionForm>
    </section>
  </>
}
