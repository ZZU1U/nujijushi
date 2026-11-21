import { channelPath } from '../routes'

export function ChannelLinks({ channelId }: { channelId: number }) {
  return <>
    <a className="button small" href={channelPath(channelId, '/queue')}>queue</a>
    <a className="button small ghost" href={channelPath(channelId, '/preview')}>preview</a>
  </>
}
