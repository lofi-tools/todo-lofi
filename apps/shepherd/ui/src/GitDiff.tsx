import { createUniqueId } from 'solid-js'

import type { GitFile } from './bridge'

const statusLabels: Partial<Record<string, string>> = {
  A: 'added', C: 'copied', D: 'deleted', M: 'modified', R: 'renamed', T: 'type changed', U: 'unmerged',
}

function describeStatus(index: string, worktree: string): string {
  const status = index + worktree
  if (status === '??') return 'Untracked'
  if (status === '!!') return 'Ignored'
  if (['DD', 'AU', 'UD', 'UA', 'DU', 'AA', 'UU'].includes(status)) return 'Merge conflict'
  return [
    index && `Staged ${statusLabels[index] ?? index}`,
    worktree && `Unstaged ${statusLabels[worktree] ?? worktree}`,
  ].filter(Boolean).join('. ')
}

export function GitDiffFile(props: { file: GitFile; selected: boolean; onSelect: () => void }) {
  const statusId = createUniqueId()
  const index = () => props.file.index.trim()
  const worktree = () => props.file.worktree.trim()
  return (
    <>
      <code aria-hidden="true">{index() || '·'}{worktree() || '·'}</code>{' '}
      <button class="git-pane__file" type="button" disabled={!props.file.actionable}
        aria-pressed={props.selected} aria-describedby={statusId} onClick={props.onSelect}>
        {props.file.originalPath ? `${props.file.originalPath} → ${props.file.path}` : props.file.path}
      </button>
      <span id={statusId} hidden>{describeStatus(index(), worktree())}</span>
    </>
  )
}

export function GitDiffText(props: { label: string; text: string }) {
  return <pre class="git-pane__diff" role="region" aria-label={props.label} tabIndex={0}>{props.text}</pre>
}
