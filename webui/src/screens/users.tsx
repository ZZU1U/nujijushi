import type { UsersPage } from '../api'
import { Badge } from '../components/Badge'
import { DataTable, type Column } from '../components/DataTable'
import { ActionForm } from '../ui'

const columns: Column<UsersPage['users'][number]>[] = [
  { label: 'user', cell: (user) => <>{user.label}{user.config_admin && <> <Badge>config</Badge></>}</> },
  { label: 'id', className: 'hint', cell: (user) => user.tg_id },
  {
    label: 'role',
    cell: (user) => <>
      <Badge tone={user.role === 'banned' ? 'red' : user.role === 'admin' ? 'green' : undefined}>{user.role}</Badge>
      {user.db_role !== user.role && <span className="hint"> db: {user.db_role}</span>}
    </>,
  },
  { label: 'archived', title: 'posts this user imported', cell: (user) => user.archived },
  { label: 'pushed', title: 'channel pushes of those posts', cell: (user) => user.pushed },
  { label: 'sent', title: 'pushes that already reached Telegram', cell: (user) => user.sent },
  { label: 'last seen', className: 'hint', cell: (user) => user.last_seen },
  {
    label: 'actions', hideLabel: true, className: 'actions',
    cell: (user) => !user.config_admin && <ActionForm action="/users" className="inline" fields={{ tg_id: user.tg_id }}>
      <button name="role" value="admin" className="small ghost">make admin</button>
      <button name="role" value="guest" className="small ghost">guest</button>
      <button name="role" value="banned" className="small danger">ban</button>
    </ActionForm>,
  },
]

export function Users({ data }: { data: UsersPage }) {
  return <section className="panel">
    <h2>bot users</h2>
    <p className="hint">
      Admins come from <code>[access] admins</code> in <code>config.toml</code> (or <code>$NUJIJUSHI_ADMINS</code>)
      and cannot be demoted here. Everyone else is a row in the <code>users</code> table.
      Guests only get what the <code>allow_*</code> switches permit — import by default.
    </p>
    <DataTable label="Bot users" rows={data.users} columns={columns} rowKey={(user) => user.tg_id}
      empty={<p className="hint">Nobody has messaged the bot yet.</p>} />
  </section>
}
