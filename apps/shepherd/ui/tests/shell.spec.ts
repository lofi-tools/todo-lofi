/**
 * The S1 smoke test: the shell is rendered against a mocked IPC bridge, so the
 * components, commands and state flow under test are the real ones and only the
 * core is replaced by canned snapshots.
 */

import { expect, test } from '@playwright/test'

import type { TerminalLayout, TerminalSnapshot } from '../src/bridge'

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

    const projects = [project]

    const scan = {
      root: '/tmp/home',
      repositories: 3,
      tree: [
        {
          name: 'src',
          path: '/tmp/home/src',
          repository: false,
          children: [
            { name: 'alpha', path: '/tmp/home/src/alpha', repository: true, children: [] },
            { name: 'beta', path: '/tmp/home/src/beta', repository: true, children: [] },
            {
              name: 'notes',
              path: '/tmp/home/src/notes',
              repository: false,
              children: [{ name: 'solo', path: '/tmp/home/src/notes/solo', repository: true, children: [] }],
            },
          ],
        },
      ],
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
        {
          id: 'entry-2',
          sequence: 2,
          kind: {
            type: 'permission',
            data: {
              approvalId: 'approval-1',
              title: 'Run tests',
              arguments: { command: 'cargo test' },
              paths: ['src/main.rs'],
              patch: {
                path: 'src/main.rs',
                oldText: 'const answer = 41;',
                newText: 'const answer = 42;',
              },
              decision: null,
            },
          },
        },
        {
          id: 'entry-3',
          sequence: 3,
          kind: {
            type: 'permission',
            data: {
              approvalId: 'approval-2',
              title: 'Delete scratch file',
              arguments: null,
              paths: [],
              patch: null,
              decision: null,
            },
          },
        },
      ],
    }

    const terminals: TerminalSnapshot[] = [
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

    let terminalLayout: TerminalLayout = { revision: 0, root: null }
    const subscriptions: string[] = []
    if (new URLSearchParams(window.location.search).has('scoped-terminals')) {
      terminals.push({
        ...terminals[0],
        id: 'terminal-task',
        taskId: 'task-1',
        scope: 'task',
        title: 'Task shell',
        cwd: tasks[0].worktreePath,
      })
      tasks.push({
        ...tasks[0],
        id: 'task-other',
        title: 'Other task',
        branch: 'shepherd/other',
        worktreePath: '/tmp/demo/.git/shepherd/worktrees/other',
      })
      terminalLayout = {
        revision: 1,
        root: {
          type: 'split', axis: 'horizontal', ratio: 500,
          first: { type: 'pane', terminalId: 'terminal-1' },
          second: { type: 'pane', terminalId: 'terminal-task' },
        },
      }
    }

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
            return { projects: [...projects] }
          case 'project_scan':
            return scan
          case 'add_project': {
            const name = args.path.split('/').filter(Boolean).pop() ?? 'project'
            if (!projects.some((candidate) => candidate.path === args.path)) {
              projects.push({
                id: `project-${name}`,
                path: args.path,
                name,
                missing: false,
                lastTaskId: null,
                unfinishedTaskCount: 0,
              })
            }
            return { projects: [...projects] }
          }
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
          case 'terminal_layout':
            return terminalLayout
          case 'save_terminal_layout':
            terminalLayout = { ...args.layout, revision: args.layout.revision + 1 }
            return terminalLayout
          case 'terminal_list':
          case 'start_terminal':
            return [...terminals]
          case 'subscribe_terminal': {
            subscriptions.push(args.terminalId)
            const frame = new Uint8Array([
              1, 3, 2, 0, 1, 0, 1, 0, 0, 0, 0, 0, 0, 0, 1, 4, 0, 0, 1, 0,
              0, 0, 1, 0, 2, 0, 1, 234, 230, 230, 0, 22, 19, 19, 0, 1, 0, 0, 0, 65,
            ])
            callbacks.get(args.channel.id)?.({ index: 0, message: frame.buffer })
            const update = new Uint8Array([
              1, 1, 2, 0, 1, 0, 1, 0, 0, 0, 0, 0, 0, 0, 3, 4, 0, 0, 1, 0,
              0, 0, 1, 0, 2, 0, 1, 234, 230, 230, 0, 22, 19, 19, 0, 1, 0, 0, 0, 66,
            ])
            if (new URLSearchParams(window.location.search).has('kitty-terminal')) {
              new DataView(update.buffer).setUint32(14, (31 << 18) | 1025, true)
            }
            callbacks.get(args.channel.id)?.({ index: 1, message: update.buffer })
            return `subscription-${args.terminalId}`
          }
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
    window.__subscriptions = subscriptions
    window.__terminalLayout = () => terminalLayout
  })
})

test('shows the project, drafts a task, streams a reply and switches tasks', async ({ page }) => {
  await page.goto('/')

  await expect(page.getByRole('button', { name: /demo/ })).toBeVisible()

  await page.getByRole('button', { name: 'New task', exact: true }).click()
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

test('shows an approval with its tool, arguments and diff', async ({ page }) => {
  await page.goto('/')
  await page.getByRole('button', { name: /Ship it/ }).click()

  const withDiff = page.locator('.entry__permission').filter({ hasText: 'Run tests' })
  await expect(withDiff).toContainText('cargo test')
  await expect(withDiff.locator('.entry__diff-path')).toHaveText('src/main.rs')
  await expect(withDiff).toContainText('- const answer = 41;')
  await expect(withDiff).toContainText('+ const answer = 42;')
  await expect(withDiff.getByRole('button', { name: 'Allow' })).toBeVisible()

  const withoutDiff = page
    .locator('.entry__permission')
    .filter({ hasText: 'Delete scratch file' })
  await expect(withoutDiff).toContainText('diff not available')
})

test('encodes typed keys into terminal input', async ({ page }) => {
  await page.goto('/')

  const input = page.getByRole('textbox', { name: 'Terminal', exact: true })
  await expect(input).toBeVisible()
  const canvas = page.locator('.terminal-view__canvas')
  await expect(canvas).toHaveCSS('height', '17px')
  await canvas.click()
  await expect(input).toBeFocused()

  await input.press('a')
  await input.press('Control+c')
  await input.press('Enter')
  await input.press('ArrowUp')

  await expect
    .poll(() =>
      page.evaluate(() => (window as unknown as { __writes: string[] }).__writes ?? []),
    )
    .toEqual(['a', '\u0003', '\r', '\u001bOA'])
})

test('opens the project picker, filters, and adds a project', async ({ page }) => {
  await page.goto('/')

  await page.getByRole('button', { name: 'Add project' }).click()
  const picker = page.getByRole('dialog', { name: 'Add project' })
  await expect(picker).toBeVisible()
  await expect(picker.getByRole('option')).toHaveCount(3)
  await expect(picker.getByRole('option', { name: /alpha/ })).toBeVisible()
  await expect(picker.getByRole('option', { name: /beta/ })).toBeVisible()
  await picker.getByRole('option', { name: /notes/ }).click()
  await expect(picker.getByRole('option', { name: /solo/ })).toBeVisible()
  await picker.getByRole('button', { name: '/tmp/home', exact: true }).click()

  const filter = page.getByLabel('Filter repositories')
  await filter.fill('alph')
  await expect(page.getByRole('option', { name: /alpha/ })).toBeVisible()
  await expect(page.getByRole('option', { name: /solo/ })).toHaveCount(0)

  await page.getByRole('option', { name: /alpha/ }).click()
  await expect(picker).toHaveCount(0)
  await expect(page.getByRole('button', { name: /alpha/ })).toBeVisible()
})

test('shows a live terminal and an ended one with a restart action', async ({ page }) => {
  await page.goto('/')

  await page.getByRole('tab', { name: 'Terminals' }).click()
  await expect(page.getByRole('button', { name: 'New shared terminal' })).toBeVisible()
  await expect(page.getByRole('button', { name: /Terminal 1/ })).toBeVisible()

  await page.getByRole('button', { name: /Terminal 2/ }).click()
  await expect(page.getByText('ended with exit code 0', { exact: false })).toBeVisible()
  await expect(page.getByRole('button', { name: 'Restart' })).toBeVisible()
})

test('keeps the shared terminal attached while switching task terminals', async ({ page }) => {
  await page.goto('/?scoped-terminals')

  const shared = page.getByRole('region', { name: 'Terminal 1' })
  const scoped = page.getByRole('region', { name: 'Task shell' })
  await expect(shared.getByRole('textbox')).toBeVisible()
  await expect(scoped.getByRole('textbox')).toBeVisible()

  await page.getByRole('button', { name: /Other task/ }).click()
  await expect(scoped).toHaveCount(0)
  await expect(shared.getByRole('textbox')).toBeVisible()

  await page.getByRole('button', { name: /Ship it/ }).click()
  await expect(scoped.getByRole('textbox')).toBeVisible()
  const subscriptions = await page.evaluate(() =>
    (window as unknown as { __subscriptions: string[] }).__subscriptions,
  )
  expect(subscriptions.filter((id) => id === 'terminal-1')).toHaveLength(1)
  expect(subscriptions.filter((id) => id === 'terminal-task')).toHaveLength(2)
})

test('resizes a split and reopens a hidden terminal from the instance list', async ({ page }) => {
  await page.goto('/?scoped-terminals')

  const divider = page.getByRole('separator', { name: 'Resize terminal split' })
  await expect(divider).toHaveAttribute('aria-valuenow', '50')
  await divider.press('ArrowRight')
  await expect(divider).toHaveAttribute('aria-valuenow', '55')
  expect(await page.evaluate(() => {
    const layout = (window as unknown as { __terminalLayout: () => TerminalLayout }).__terminalLayout()
    return layout.root?.type === 'split' ? layout.root.ratio : null
  })).toBe(550)

  const scoped = page.getByRole('region', { name: 'Task shell' })
  await page.getByRole('region', { name: 'Terminal 1' }).getByRole('textbox').press('Control+Backquote')
  await expect(scoped.getByRole('textbox')).toBeFocused()
  expect(await page.evaluate(() => (window as unknown as { __writes: string[] }).__writes)).toEqual([])
  await scoped.getByRole('button', { name: 'Hide pane' }).click()
  await expect(scoped).toHaveCount(0)
  await page.getByRole('button', { name: /Task shell.*hidden/ }).click()
  await expect(scoped.getByRole('textbox')).toBeFocused()
})

test('reports Kitty key events and keeps a keyboard copy view', async ({ page }) => {
  await page.goto('/?kitty-terminal')

  const input = page.getByRole('textbox', { name: 'Terminal', exact: true })
  await expect(page.getByText('BB', { exact: true })).toBeAttached()
  await input.press('a')
  await expect.poll(() => page.evaluate(() =>
    (window as unknown as { __writes: string[] }).__writes,
  )).toEqual(['\x1b[97;;97u', '\x1b[97;1:3u'])

  await input.press('Control+Shift+x')
  const copy = page.getByRole('textbox', { name: 'Copy terminal output' })
  await expect(copy).toBeFocused()
  await expect(copy).toHaveValue('BB')
  await copy.press('Escape')
  await expect(input).toBeFocused()
})
