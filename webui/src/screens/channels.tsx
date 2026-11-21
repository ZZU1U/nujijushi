import { Fragment } from 'react'
import { ActionForm, TextField, SelectField, Timezones, text, capitalize, listKey } from '../ui'
import type { ChannelsPage } from '../api'

export function Channels({ data }: { data: ChannelsPage }) {
  const { channels, timezones, templates } = data
  return <>
    <section className="panel">
      <h2>channels</h2>
      {channels.length > 0 ? <>
        <div className="responsive-table" tabIndex={0}><table>
          <thead><tr><th>alias</th><th>channel</th><th>queue</th><th>every</th><th>window</th><th>nsfw</th><th>tags</th><th>next</th><th></th></tr></thead>
          <tbody>
            {channels.map((c, index) => <Fragment key={listKey(c, index)}>
              <tr>
                <td><code>{c.alias}</code>{c.discovered ? <>{" "}<span className="badge">discovered</span></> : null}</td>
                <td>
                  <a href={text("/channels/", c.id, "/edit")}>{c.title}</a>
                  <div className="hint">{c.tg_id}</div>
                  {c.paused ? <><span className="badge red">paused</span></> : null}
                  {!(c.enabled) ? <><span className="badge">disabled</span></> : null}
                </td>
                <td>{c.queued}</td>
                <td>{c.interval}m</td>
                <td className="hint">{c.active}</td>
                <td><span className="badge">{c.nsfw_policy}</span></td>
                <td className="hint">{c.tag_style} · {c.attribution}</td>
                <td className="hint">{c.next}</td>
                <td className="actions">
                  <a className="button small" href={text("/channels/", c.id, "/queue")}>queue</a>
                  <a className="button small ghost" href={text("/channels/", c.id, "/preview")}>preview</a>
                </td>
              </tr>
            </Fragment>)}
          </tbody>
        </table></div>
      </> : <><p className="hint">None yet — add one below.</p></>}
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
          <SelectField name="nsfw_policy">
            <option value="spoil" selected>spoil (blur + #nsfw)</option>
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
