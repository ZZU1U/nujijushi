import { PageLink } from './PageLink'
import { channelPath } from '../routes'

export function ChannelLinks({ channelId }: { channelId: number }) {
  return <>
    <PageLink className="button small" href={channelPath(channelId, '/queue')}>queue</PageLink>
    <PageLink className="button small ghost" href={channelPath(channelId, '/preview')}>preview</PageLink>
  </>
}
