import { createComputed } from 'solid-js'
import { createStore, reconcile } from 'solid-js/store'

export function createSnapshotRows<Row extends { id: string }>(source: () => Row[]) {
  const [state, setState] = createStore<{ rows: Row[] }>({ rows: [] })
  createComputed(() => {
    // Reconciliation mutates its target, so let the view own its copy of the snapshot.
    setState('rows', reconcile(structuredClone(source()), { key: 'id' }))
  })
  return () => state.rows
}
