import type { ComponentProps } from 'react'

type BadgeProps = ComponentProps<'span'> & { tone?: 'green' | 'red' | 'amber' | 'accent' }

export function Badge({ tone, className, ...props }: BadgeProps) {
  return <span className={['badge', tone, className].filter(Boolean).join(' ')} {...props} />
}
