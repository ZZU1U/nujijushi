import { TextField } from '../ui'

export function DeliveryActions({ inputId }: { inputId?: string }) {
  return <>
    <TextField id={inputId} type="number" name="message_id" min="1"
      aria-label="Telegram message ID (optional)" placeholder="Message ID" />
    <button name="action" value="delivered" className="small">Confirm delivered</button>
    <button name="action" value="not_delivered" className="small ghost"
      data-confirm="Check Telegram first. This queues the entire post for another send.">
      Requeue entire post
    </button>
  </>
}
