import { Fragment } from 'react'
import { ActionForm, SelectField, listKey } from '../ui'
import type { SettingsPage } from '../api'

export function Settings({ data }: { data: SettingsPage }) {
  const { channels, default_channel } = data
  return <>
    <section className="panel">
      <h2>Settings</h2>
      <ActionForm action="/settings">
        <label>Default queue channel
          <SelectField defaultValue={default_channel ?? undefined} name="channel">
            <option value="">No default</option>
            {channels.map((channel, index) => <Fragment key={listKey(channel, index)}>
              <option value={channel.id}>{channel.title}</option>
            </Fragment>)}
          </SelectField>
        </label>
        <button type="submit">Save settings</button>
      </ActionForm>
    </section>
  </>
}
