import { Fragment } from 'react'
import { ActionForm, TextField, SelectField, TextArea, CaptionPreview, Timezones, text, capitalize, listKey, PageLink } from '../ui'
import type { ChannelEditPage } from '../api'

export function ChannelEdit({ data }: { data: ChannelEditPage }) {
  const { channel, timezones, policies, tag_styles, attributions, parse_modes, templates, default_template, sample } = data
  return <>
    <div className="cols wide-left">
      <section className="panel">
        <h2>{channel.title}{" "}<span className="hint">{channel.tg_id}</span>{" "}<code>{channel.alias}</code></h2>
        <p className="hint">
          <PageLink href={text("/channels/", channel.id, "/queue")}>open queue</PageLink> ·
          <PageLink href={text("/channels/", channel.id, "/preview")}>preview next slots</PageLink>
          {channel.paused ? <><span className="badge red">paused after {channel.failures} failures</span></> : null}
        </p>
        <ActionForm action={text("/channels/", channel.id, "/edit")} data-verify-destination={channel.tg_id}>
          <div className="grid-form">
            <label>alias<TextField name="alias" defaultValue={channel.alias} required pattern={"[A-Za-z0-9_\\-]+"} /></label>
            <label>title<TextField name="title" defaultValue={channel.title} required /></label>
            <label>channel id<TextField name="tg_id" defaultValue={channel.tg_id} required /></label>
            <label>interval (minutes)<TextField name="interval_minutes" type="number" min="1" defaultValue={channel.interval_minutes} /></label>
            <label>active from<TextField name="active_from" defaultValue={channel.active_from} /></label>
            <label>active to<TextField name="active_to" defaultValue={channel.active_to} /></label>
            <label>timezone
              <TextField name="tz" list="tz-zones" defaultValue={channel.tz} placeholder="Europe/Berlin" autoComplete="off" spellCheck="false" aria-describedby="tz-hint" />
              <span className="field-hint" id="tz-hint">IANA zone name (e.g. <code>Europe/Berlin</code>) — it sets the clock the active hours and daily cap run on.</span>
            </label>
            <label>nsfw policy
              <SelectField defaultValue={channel.nsfw_policy} name="nsfw_policy">
                {policies.map((p, index) => <Fragment key={listKey(p, index)}><option value={p}>{p}</option></Fragment>)}
              </SelectField>
            </label>
          </div>
          <details className="panel inset">
            <summary>Advanced channel settings</summary>
            <div className="grid-form">
              <label>daily cap (0 = off)<TextField name="daily_cap" type="number" min="0" defaultValue={channel.daily_cap} /></label>
              <label>jitter (seconds)<TextField name="jitter_secs" type="number" min="0" defaultValue={channel.jitter_secs ?? undefined} /></label>
              <label>artist cooldown<TextField name="artist_cooldown" type="number" min="0" defaultValue={channel.artist_cooldown ?? undefined} /></label>
              <label>min media<TextField name="min_media" type="number" min="0" defaultValue={channel.min_media} /></label>
              <label>tag style
                <SelectField defaultValue={channel.tag_style} name="tag_style">
                  {tag_styles.map((t, index) => <Fragment key={listKey(t, index)}><option value={t}>{capitalize(t)}</option></Fragment>)}
                </SelectField>
              </label>
              <label>max tags<TextField name="max_tags" type="number" min="0" defaultValue={channel.max_tags} /></label>
              <label>attribution
                <SelectField defaultValue={channel.attribution} name="attribution">
                  {attributions.map((a, index) => <Fragment key={listKey(a, index)}><option value={a}>{a}</option></Fragment>)}
                </SelectField>
              </label>
              <label>parse mode
                <SelectField defaultValue={channel.parse_mode} name="parse_mode">
                  {parse_modes.map((m, index) => <Fragment key={listKey(m, index)}><option value={m}>{m}</option></Fragment>)}
                </SelectField>
              </label>
              <label>blocked tags (csv)<TextField name="blocked_tags" defaultValue={channel.blocked_tags} /></label>
              <label>required tags (csv)<TextField name="required_tags" defaultValue={channel.required_tags} /></label>
              <label>built-in template
                <SelectField defaultValue={channel.template_name} name="template_name">
                  {templates.map((t, index) => <Fragment key={listKey(t, index)}><option value={t}>{capitalize(t)}</option></Fragment>)}
                </SelectField>
              </label>
            </div>
            <Timezones zones={timezones} />

            <label>caption template (leave empty to use the built-in one)
              <TextArea name={"template"} rows={12} spellCheck={"false"} placeholder={default_template} defaultValue={channel.template ?? undefined}></TextArea>
            </label>
            <p className="hint">
              Available: <code>author.name</code>, <code>author.handle</code>, <code>author.url</code>,
              <code>post.title</code>, <code>post.description</code>, <code>post.url</code>,
              <code>post.date</code>, <code>post.media_count</code>, <code>tags</code>, <code>hashtags</code>,
              <code>source_label</code>, <code>source_link</code>, <code>nsfw</code>, <code>spoiler</code>.
              Filters: <code>truncate_words(n)</code>, <code>hashtags</code>, <code>one_line</code>.
            </p>

          </details>
          <div className="checks">
            <label className="check"><TextField type="checkbox" name="enabled" {...(channel.enabled ? { "defaultChecked": true } : {})} /> enabled</label>
            <label className="check"><TextField type="checkbox" name="include_source_tag" {...(channel.include_source_tag ? { "defaultChecked": true } : {})} /> source hashtag</label>
            <label className="check"><TextField type="checkbox" name="silent" {...(channel.silent ? { "defaultChecked": true } : {})} /> silent</label>
            <label className="check"><TextField type="checkbox" name="protect" {...(channel.protect ? { "defaultChecked": true } : {})} /> protect content</label>
          </div>
          <div className="form-actions"><button type="submit">save channel</button></div>
        </ActionForm>
      </section>

      <aside className="panel">
        <h2>next slot preview</h2>
        {sample ? <>
          <div className="hint">{sample.reason} · {sample.media} media · {sample.messages} message(s){sample.spoiler ? <> · spoiler</> : null}</div>
          <CaptionPreview caption={sample.caption}></CaptionPreview>
        </> : <>
          <p className="hint">Nothing queued for this channel yet.</p>
        </>}
      </aside>
    </div>
  </>
}
