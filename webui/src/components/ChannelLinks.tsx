export function ChannelLinks({ channelId }: { channelId: number }) {
  const base = '/channels/' + channelId
  return <>
    <a className="button small" href={base + '/queue'}>queue</a>
    <a className="button small ghost" href={base + '/preview'}>preview</a>
  </>
}
