import type { Key, ReactNode } from 'react'

export interface Column<Row> {
  label: string
  cell: (row: Row) => ReactNode
  className?: string
  title?: string
  hideLabel?: boolean
}

interface RowAttributes {
  className?: string
  [attribute: `data-${string}`]: string | number | undefined
}

interface TableProps<Row> {
  label: string
  rows: readonly Row[]
  columns: readonly Column<Row>[]
  rowKey: (row: Row, index: number) => Key
  rowAttributes?: (row: Row) => RowAttributes
  empty?: ReactNode
  compact?: boolean
  hideHeader?: boolean
  className?: string
  id?: string
}

export function DataTable<Row>({
  label, rows, columns, rowKey, rowAttributes, empty, compact, hideHeader, className, id,
}: TableProps<Row>) {
  if (rows.length === 0) return empty ?? null

  const classes = [compact && 'compact', className].filter(Boolean).join(' ')
  return <div className="table-scroll" role="region" aria-label={label} tabIndex={0}>
    <table id={id} className={classes} aria-label={label}>
      <thead className={hideHeader ? 'table-head-hidden' : undefined}>
        <tr>{columns.map((column) => <th key={column.label} scope="col" title={column.title}>
          {hideHeader || column.hideLabel
            ? <span className="visually-hidden">{column.label}</span>
            : column.label}
        </th>)}</tr>
      </thead>
      <tbody>
        {rows.map((row, index) => <tr key={rowKey(row, index)} {...rowAttributes?.(row)}>
          {columns.map((column) => <td key={column.label} className={column.className}>
            {column.cell(row)}
          </td>)}
        </tr>)}
      </tbody>
    </table>
  </div>
}
