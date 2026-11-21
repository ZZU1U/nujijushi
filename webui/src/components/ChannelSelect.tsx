import type { ComponentProps } from 'react'
import type { ChannelChoice } from '../api'
import { SelectField } from '../ui'

interface ChannelSelectProps extends Omit<ComponentProps<'select'>, 'children'> {
  channels: readonly ChannelChoice[]
  emptyLabel?: string
}

export function ChannelSelect({ channels, emptyLabel = 'Choose a channel', ...props }: ChannelSelectProps) {
  return <SelectField {...props}>
    <option value="">{emptyLabel}</option>
    {channels.map((channel) => <option key={channel.id} value={channel.id}>{channel.title}</option>)}
  </SelectField>
}
