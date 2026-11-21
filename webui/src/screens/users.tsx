import { Fragment } from 'react'
import { ActionForm, TextField, text } from '../ui'
import type { UsersPage } from '../api'

export function Users({ data }: { data: UsersPage }) {
  const { users } = data
  return <>
    <section className="panel">
      <h2>bot users</h2>
      <p className="hint">
        Admins come from <code>[access] admins</code> in <code>config.toml</code> (or <code>$NUJIJUSHI_ADMINS</code>)
        and cannot be demoted here. Everyone else is a row in the <code>users</code> table.
        Guests only get what the <code>allow_*</code> switches permit — import by default.
      </p>
      {users.length > 0 ? <>
        <div className="responsive-table" tabIndex={0}><table>
          <thead><tr><th>user</th><th>id</th><th>role</th><th title="posts this user imported">archived</th><th title="channel pushes of those posts">pushed</th><th title="pushes that already reached Telegram">sent</th><th>last seen</th><th></th></tr></thead>
          <tbody>
            {users.map((u) => <Fragment key={u.tg_id}>
              <tr>
                <td>{u.label}{u.config_admin ? <>{" "}<span className="badge">config</span></> : null}</td>
                <td className="hint">{u.tg_id}</td>
                <td><span className={text("badge ", ((u.role === "banned") ? "red" : ((u.role === "admin") ? "green" : "")))}>{u.role}</span>{(u.db_role !== u.role) ? <><span className="hint"> db: {u.db_role}</span></> : null}</td>
                <td>{u.archived}</td>
                <td>{u.pushed}</td>
                <td>{u.sent}</td>
                <td className="hint">{u.last_seen}</td>
                <td className="actions">
                  {!(u.config_admin) ? <>
                    <ActionForm action="/users" className="inline">
                      <TextField type="hidden" name="tg_id" value={u.tg_id} />
                      <button name="role" value="admin" className="small ghost">make admin</button>
                      <button name="role" value="guest" className="small ghost">guest</button>
                      <button name="role" value="banned" className="small danger">ban</button>
                    </ActionForm>
                  </> : null}
                </td>
              </tr>
            </Fragment>)}
          </tbody>
        </table></div>
      </> : <>
        <p className="hint">Nobody has messaged the bot yet.</p>
      </>}
    </section>
  </>
}
