import { Fragment } from 'react'
import { ActionForm, TextField, SelectField, Timezones, text, capitalize, listKey, PageLink } from '../ui'
import type { ChannelsPage, ChannelSummaryRow } from '../api'
import { Badge } from '../components/Badge'
import { ChannelLinks } from '../components/ChannelLinks'
import { DataTable, type Column } from '../components/DataTable'

const columns: Column<ChannelSummaryRow>[] = [
  {
    label: 'alias',
    cell: (channel) => <><code>{channel.alias}</code>{channel.discovered && <> <Badge>discovered</Badge></>}</>,
  },
  {
    label: 'channel',
    cell: (channel) => <>
      <PageLink href={text('/channels/', channel.id, '/edit')}>{channel.title}</PageLink>
      <div className="hint">{channel.tg_id}</div>
      {channel.paused && <Badge tone="red">paused</Badge>}
      {!channel.enabled && <Badge>disabled</Badge>}
    </>,
  },
  { label: 'queue', cell: (channel) => channel.queued },
  { label: 'every', cell: (channel) => channel.interval + 'm' },
  { label: 'window', className: 'hint', cell: (channel) => channel.active },
  { label: 'nsfw', cell: (channel) => <Badge>{channel.nsfw_policy}</Badge> },
  { label: 'tags', className: 'hint', cell: (channel) => text(channel.tag_style, ' · ', channel.attribution) },
  { label: 'next', className: 'hint', cell: (channel) => channel.next },
  {
    label: 'actions', hideLabel: true, className: 'actions',
    cell: (channel) => <ChannelLinks channelId={channel.id} />,
  },
]

export function Channels({ data }: { data: ChannelsPage }) {
  const { channels, timezones, templates } = data
  return <>
    <section className="panel">
      <h2>channels</h2>
      <DataTable label="Channels" rows={channels} columns={columns} rowKey={(channel) => channel.id}
        empty={<p className="hint">None yet — add one below.</p>} />
    </section>

    <section className="panel" id="add-channel">
      <h2>add a channel</h2>
      <p className="hint">
        Create the channel in Telegram, add your bot as an administrator with "post messages",
        then paste its <code>@username</code> or numeric id (<code>-100…</code>).
      </p>
      <ActionForm action="/channels" className="grid-form" data-verify-destination="">
        <label>alias<TextField name="alias" required placeholder="night-shift" pattern={"[A-Za-z0-9_\\-]+"} /></label>
        <label>title<TextField name="title" required placeholder="Late night art" /></label>
        <label>channel<TextField name="tg_id" required placeholder="@mychannel" /></label>
        <label>interval (minutes)<TextField name="interval_minutes" type="number" min="1" defaultValue="180" /></label>
        <label>active from<TextField name="active_from" defaultValue="09:00" /></label>
        <label>active to<TextField name="active_to" defaultValue="23:00" /></label>
        <label>timezone<TextField name="tz" list="tz-zones" defaultValue="Europe/Berlin" placeholder="Europe/Berlin" autoComplete="off" spellCheck="false" /></label>
        <label>nsfw policy
          <SelectField name="nsfw_policy" defaultValue="spoil">
            <option value="spoil">spoil (blur + #nsfw)</option>
            <option value="allow">allow</option>
            <option value="reject">reject</option>
          </SelectField>
        </label>
        <label>caption template
          <SelectField name="template_name">
            {templates.map((t, index) => <Fragment key={listKey(t, index)}><option value={t}>{capitalize(t)}</option></Fragment>)}
          </SelectField>
        </label>
        <label className="check"><TextField type="checkbox" name="enabled" /> enable scheduled publishing now</label>
        <label className="check"><TextField type="checkbox" name="include_source_tag" defaultChecked /> add source hashtag</label>
        <div className="form-actions"><button type="submit">create channel</button></div>
      </ActionForm>
      <Timezones zones={timezones} />
    </section>
  </>
}
