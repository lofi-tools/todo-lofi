/**
 * The S1 smoke test: the shell is rendered against a mocked IPC bridge, so the
 * components, commands and state flow under test are the real ones and only the
 * core is replaced by canned snapshots.
 */

import { expect, test } from '@playwright/test'

test.beforeEach(async ({ page }) => {
  await page.addInitScript(() => {
    const callbacks = new Map()
    let nextCallbackId = 1
    let nextTask = 2

    const project = {
      id: 'project-1',
      path: '/tmp/demo',
      name: 'demo',
      missing: false,
      lastTaskId: 'task-1',
      unfinishedTaskCount: 1,
    }

    const tasks = [
      {
        id: 'task-1',
        projectId: 'project-1',
        title: 'Ship it',
        state: 'ready',
        tier: 'native',
        branch: 'shepherd/ship-it',
        worktreePath: '/tmp/demo/.git/shepherd/worktrees/ship-it',
        error: null,
        createdAt: 1_760_000_000_000,
      },
    ]

    const transcripts = {
      'task-1': [
        {
          id: 'entry-1',
          sequence: 1,
          kind: { type: 'agentText', data: { text: 'ready to go' } },
        },
      ],
    }

    const terminals = [
      {
        id: 'terminal-1',
        projectId: 'project-1',
        taskId: null,
        scope: 'project',
        title: 'Terminal 1',
        cwd: '/tmp/demo',
        command: '/bin/zsh',
        createdAt: 1_760_000_000_000,
        endedAt: null,
        exitCode: null,
      },
      {
        id: 'terminal-2',
        projectId: 'project-1',
        taskId: null,
        scope: 'project',
        title: 'Terminal 2',
        cwd: '/tmp/demo',
        command: '/bin/zsh',
        createdAt: 1_760_000_100_000,
        endedAt: 1_760_000_200_000,
        exitCode: 0,
      },
    ]

    // Every byte the shell writes to the PTY, in order.
    const writes = []

    function entry(id, sequence, kind) {
      return { id, sequence, kind }
    }

    function detail(task) {
      return {
        ...task,
        proposedBranch: task.branch ?? 'shepherd/new-task',
        proposedWorktreePath: task.worktreePath ?? '/tmp/demo/.git/shepherd/worktrees/new-task',
        baseBranch: task.state === 'draft' ? null : 'main',
        updatedAt: task.createdAt,
      }
    }

    function workspace(taskId) {
      const task = tasks.find((candidate) => candidate.id === taskId)
      return { project, task: detail(task), transcript: transcripts[taskId] ?? [] }
    }

    window.__TAURI_INTERNALS__ = {
      callbacks,
      transformCallback(callback) {
        const id = nextCallbackId
        nextCallbackId += 1
        callbacks.set(id, callback)
        return id
      },
      unregisterCallback(id) {
        callbacks.delete(id)
      },
      runCallback(id, payload) {
        const callback = callbacks.get(id)
        if (callback) callback(payload)
      },
      metadata: {
        currentWindow: { label: 'main' },
        currentWebview: { windowLabel: 'main', label: 'main' },
      },
      invoke: async (command, args) => {
        switch (command) {
          case 'project_list':
          case 'add_project':
            return { projects: [project] }
          case 'task_list':
            // A fresh array each call: real IPC serializes, and Solid skips a
            // setter handed the identical reference.
            return { projectId: args.projectId, tasks: [...tasks] }
          case 'task_workspace':
            return workspace(args.taskId)
          case 'create_draft': {
            const id = `task-${nextTask}`
            nextTask += 1
            tasks.unshift({
              id,
              projectId: args.projectId,
              title: args.title,
              state: 'draft',
              tier: 'native',
              branch: null,
              worktreePath: null,
              error: null,
              createdAt: 1_760_000_500_000,
            })
            transcripts[id] = []
            return workspace(id)
          }
          case 'send_prompt': {
            const task = tasks.find((candidate) => candidate.id === args.taskId)
            task.state = 'waiting'
            transcripts[task.id] = [
              entry(`entry-${task.id}-1`, 1, { type: 'userMessage', data: { text: args.text } }),
              entry(`entry-${task.id}-2`, 2, {
                type: 'agentText',
                data: { text: 'working on it' },
              }),
            ]
            return workspace(task.id)
          }
          case 'terminal_list':
          case 'start_terminal':
            return [...terminals]
          case 'terminal_frame':
            return null
          case 'terminal_write':
            writes.push(args.data)
            return null
          case 'quit':
            return { interruptedTasks: 0, liveTerminals: 0 }
          default:
            return null
        }
      },
    }

    window.__writes = writes
  })
})

test('shows the project, drafts a task, streams a reply and switches tasks', async ({ page }) => {
  await page.goto('/')

  await expect(page.getByRole('button', { name: /demo/ })).toBeVisible()

  await page.getByRole('button', { name: 'New task' }).click()
  await expect(page.locator('.task', { hasText: 'New task' })).toBeVisible()
  await expect(page.getByText('Proposed branch')).toBeVisible()
  await expect(page.getByText('shepherd/new-task')).toBeVisible()
  await expect(
    page.getByText('/tmp/demo/.git/shepherd/worktrees/new-task', { exact: false }),
  ).toBeVisible()

  await page.getByLabel('Prompt').fill('make it fast')
  await page.getByRole('button', { name: 'Send' }).click()
  await expect(page.getByText('working on it')).toBeVisible()
  await expect(page.locator('.transcript')).toContainText('make it fast')

  await page.getByRole('button', { name: /Ship it/ }).click()
  await expect(page.getByText('ready to go')).toBeVisible()
})

test('encodes typed keys into terminal input', async ({ page }) => {
  await page.goto('/')

  const canvas = page.getByLabel('Terminal', { exact: true })
  await expect(canvas).toBeVisible()
  await canvas.click()

  await canvas.press('a')
  await canvas.press('Control+c')
  await canvas.press('Enter')
  await canvas.press('ArrowUp')

  await expect
    .poll(() =>
      page.evaluate(() => (window as unknown as { __writes: string[] }).__writes ?? []),
    )
    .toEqual(['a', '\u0003', '\r', '\u001b[A'])
})

test('shows a live terminal and an ended one with a restart action', async ({ page }) => {
  await page.goto('/')

  await page.getByRole('tab', { name: 'Terminals' }).click()
  await expect(page.getByRole('button', { name: 'New terminal' })).toBeVisible()
  await expect(page.getByRole('button', { name: /Terminal 1/ })).toBeVisible()

  await page.getByRole('button', { name: /Terminal 2/ }).click()
  await expect(page.getByText('ended with exit code 0', { exact: false })).toBeVisible()
  await expect(page.getByRole('button', { name: 'Restart' })).toBeVisible()
})
