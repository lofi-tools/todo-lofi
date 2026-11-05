/**
 * The S1 smoke test: the shell is rendered against a mocked IPC bridge, so the
 * components, commands and state flow under test are the real ones and only the
 * core is replaced by canned snapshots.
 */

import { expect, test } from '@playwright/test'

import type { TerminalLayout, TerminalSnapshot } from '../src/bridge'

test.beforeEach(async ({ context }) => {
  await context.addInitScript(() => {
    const callbacks = new Map()
    let nextCallbackId = 1
    let nextTask = 2
    let desktopProjectReads = 0
    const options = new URLSearchParams(window.location.search)

    const project = {
      id: 'project-1',
      path: '/tmp/demo',
      name: 'demo',
      missing: false,
      isGitRepository: true,
      lastTaskId: 'task-1',
      unfinishedTaskCount: 1,
    }

    const projects = options.has('empty-projects') ? [] : [project]
    if (options.has('two-projects')) {
      projects.push({ ...project, id: 'project-notes', path: '/tmp/notes', name: 'notes',
        isGitRepository: false, lastTaskId: null, unfinishedTaskCount: 0 })
    }

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
    if (options.has('two-projects')) {
      tasks.push({ ...tasks[0], id: 'task-archived', title: 'Old chat', state: 'done' })
    }

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
      return {
        project: projects.find((candidate) => candidate.id === task.projectId),
        task: detail(task),
        transcript: transcripts[taskId] ?? [],
        diagnostics: task.state === 'waiting' ? { command: 'codex app-server', protocol: 'app-server', model: 'session-model', stderrTail: '' } : null,
      }
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
          case 'plugin:path|resolve_directory':
            return '/tmp/home/'
          case 'project_list':
            return { projects: [...projects] }
          case 'codex_project_model':
            return 'gpt-6.1-terra'
          case 'project_remote':
            if (options.has('hugging-face-remote')) {
              return { label: 'datasets/example/data', forge: 'huggingface' }
            }
            return args.projectId === 'project-alpha'
              ? { label: 'demo/alpha-a-repository-with-a-long-name-that-still-fits-on-one-line', forge: 'gitlab' }
              : { label: 'example/demo', forge: 'github' }
          case 'project_scan':
            return scan
          case 'local_codex_available':
            return !new URLSearchParams(window.location.search).has('no-codex')
          case 'local_codex_projects':
            return [
              { name: 'Desktop notes', path: '/tmp/outside-scan/notes' },
              { name: 'Desktop service', path: '/tmp/outside-scan/service' },
              { name: 'Desktop website', path: '/tmp/outside-scan/website' },
              ...(desktopProjectReads++ ? [{ name: 'Desktop tools', path: '/tmp/outside-scan/tools' }] : []),
            ]
          case 'add_project': {
            const name = args.path.split('/').filter(Boolean).pop() ?? 'project'
            if (!projects.some((candidate) => candidate.path === args.path)) {
              projects.push({
                id: `project-${name}`,
                path: args.path,
                name,
                missing: false,
                isGitRepository: args.path.startsWith('/tmp/home/'),
                lastTaskId: null,
                unfinishedTaskCount: 0,
              })
            }
            return { projects: [...projects] }
          }
          case 'task_list':
            // A fresh array each call: real IPC serializes, and Solid skips a
            // setter handed the identical reference.
            return { projectId: args.projectId, tasks: tasks.filter((task) => task.projectId === args.projectId) }
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
          case 'start_freebuff_terminal':
            return 'freebuff-fixture'
          case 'subscribe_terminal': {
            subscriptions.push(args.terminalId)
            const frame = new Uint8Array([
              1, 3, 2, 0, 1, 0, 1, 0, 0, 0, 0, 0, 0, 0, 1, 4, 0, 0, 1, 0,
              0, 0, 1, 0, 2, 0, 1, 234, 230, 230, 0, 22, 19, 19, 0, 1, 0, 0, 0, 65,
              1, 0, 0, 0, 1, 0, 0, 0,
            ])
            callbacks.get(args.channel.id)?.({ index: 0, message: frame.buffer })
            const update = new Uint8Array([
              1, 1, 2, 0, 1, 0, 1, 0, 0, 0, 0, 0, 0, 0, 3, 4, 0, 0, 1, 0,
              0, 0, 1, 0, 2, 0, 1, 234, 230, 230, 0, 22, 19, 19, 0, 1, 0, 0, 0, 66,
              2, 0, 0, 0, 2, 0, 0, 0,
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

test('puts the macOS window controls and history buttons in one header row', async ({ page }) => {
  await page.addInitScript(() => {
    Object.defineProperty(window, 'isTauri', { value: true })
    Object.defineProperty(navigator, 'platform', { value: 'MacIntel' })
  })
  await page.goto('/')

  const header = page.getByRole('banner', { name: 'Workspace header' })
  const [windowControls, previous, next] = await Promise.all([
    header.locator('.shell__window-controls').boundingBox(),
    header.getByRole('button', { name: 'Previous' }).boundingBox(),
    header.getByRole('button', { name: 'Next' }).boundingBox(),
  ])
  if (!windowControls || !previous || !next) throw new Error('The window and history controls should be visible')

  const windowControlsCenter = windowControls.y + windowControls.height / 2
  expect(previous.y + previous.height / 2).toBeCloseTo(windowControlsCenter)
  expect(next.y + next.height / 2).toBeCloseTo(windowControlsCenter)
  expect(previous.x).toBeGreaterThanOrEqual(windowControls.x + windowControls.width)
  expect(next.x).toBeGreaterThanOrEqual(previous.x + previous.width)
})

test('shows the project, drafts a task, streams a reply and switches tasks', async ({ page }) => {
  await page.goto('/')

  await expect(page.getByRole('button', { name: 'demo', exact: true })).toBeVisible()

  await page.getByRole('button', { name: 'New chat in demo' }).click()
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
  await expect(page.getByRole('button', { name: 'Model / effort: session-model' })).toHaveText('session-model')

  await page.getByRole('button', { name: /Ship it/ }).click()
  await expect(page.getByText('ready to go')).toBeVisible()
})

test('shows an approval with its tool, arguments and diff', async ({ page }) => {
  await page.goto('/')

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

test('composes the first chat in an empty project and hides its sole task row', async ({ page }) => {
  await page.goto('/?two-projects')
  const directory = page.getByRole('button', { name: 'notes', exact: true })
  await directory.click()
  const welcome = page.getByRole('heading', { name: 'What should Codex build in notes?' })
  await expect(welcome).toBeVisible()
  await expect(page.getByRole('button', { name: 'Model / effort: gpt-6.1-terra' })).toHaveText('gpt-6.1-terra')
  const agent = welcome.getByRole('button', { name: 'Agent: Codex' })
  await agent.click()
  const agentMenu = welcome.getByRole('menu', { name: 'Agent' })
  const codex = agentMenu.getByRole('menuitemradio', { name: 'Codex', checked: true })
  await expect(codex).toBeFocused()
  await expect(agentMenu.getByRole('menuitemradio', { name: 'OpenCode (coming soon)' })).toBeDisabled()
  await codex.press('Escape')
  await expect(agentMenu).toBeHidden()
  await expect(agent).toBeFocused()

  await welcome.getByRole('button', { name: 'Project: notes' }).press('Enter')
  const projectMenu = welcome.getByRole('menu', { name: 'Project' })
  const notes = projectMenu.getByRole('menuitemradio', { name: 'notes', checked: true })
  await expect(notes).toBeFocused()
  await notes.press('ArrowUp')
  const demo = projectMenu.getByRole('menuitemradio', { name: 'demo' })
  await expect(demo).toBeFocused()
  await demo.press('Enter')
  await expect(page.getByRole('button', { name: 'demo', exact: true })).toHaveAttribute('aria-current', 'true')
  await expect(welcome).toBeHidden()
  await directory.click()
  await expect(welcome).toBeVisible()
  const headlineBounds = await welcome.boundingBox()
  const conversationBounds = await page.locator('.chat__transcript').boundingBox()
  if (!headlineBounds || !conversationBounds) throw new Error('The headline and conversation should be visible')
  expect(headlineBounds.x + headlineBounds.width / 2).toBeCloseTo(conversationBounds.x + conversationBounds.width / 2, 0)
  expect(headlineBounds.y + headlineBounds.height / 2).toBeCloseTo(conversationBounds.y + conversationBounds.height / 2, 0)
  const prompt = page.getByLabel('Prompt')
  await expect(prompt).toBeEnabled()
  await expect(prompt).toBeFocused()
  await expect(page.locator('.task-list')).toHaveCount(0)
  await prompt.fill('First prompt')
  await expect(page.getByText('Proposed branch')).toBeVisible()
  await expect(prompt).toHaveValue('First prompt')
  await prompt.press('Enter')
  await expect(page.locator('.transcript')).toContainText('First prompt')
  await expect(page.locator('.task-list')).toHaveCount(0)
  await directory.click()
  await expect(page.locator('.transcript')).toContainText('First prompt')
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

test('opens Freebuff as a standalone terminal in the conversation pane', async ({ page }) => {
  await page.goto('/')
  const chat = page.getByRole('region', { name: 'Task chat' })
  await chat.getByLabel('Prompt').fill('Keep this draft')
  await chat.getByRole('button', { name: 'Agent: Codex' }).click()
  await chat.getByRole('menuitemradio', { name: 'Freebuff (terminal)' }).click()

  const freebuff = page.getByRole('region', { name: 'Freebuff terminal' })
  await expect(chat).toBeHidden()
  await expect(freebuff.getByRole('textbox', { name: 'Terminal', exact: true })).toBeFocused()
  await expect(freebuff.getByLabel('Terminal output')).toContainText('B')
  await freebuff.getByRole('button', { name: 'Back to chat' }).click()

  await expect(freebuff).toBeHidden()
  await expect(chat.getByLabel('Prompt')).toHaveValue('Keep this draft')
})

test('opens the project picker, filters, and adds a project', async ({ page }) => {
  await page.goto('/')

  const heading = page.locator('.sidebar__header')
  await expect(heading.getByRole('heading', { name: 'Projects' })).toBeVisible()
  const addProject = heading.getByRole('button', { name: 'Add project' })
  await expect(addProject).toHaveAttribute('title', 'Add project')
  await addProject.click()
  const picker = page.getByRole('dialog', { name: 'Add project' })
  await expect(picker).toBeVisible()
  await expect(picker.getByRole('treeitem')).toHaveCount(4)
  await expect(picker.getByRole('treeitem', { name: /alpha/ })).toBeVisible()
  await expect(picker.getByRole('treeitem', { name: /beta/ })).toBeVisible()
  const notes = picker.getByRole('treeitem', { name: /notes/ })
  const solo = picker.getByRole('treeitem', { name: /solo/ })
  await expect(notes).toHaveAttribute('aria-expanded', 'true')
  await expect(solo).toBeVisible()
  await notes.click()
  await expect(solo).toHaveCount(0)
  await expect(picker.getByRole('treeitem', { name: /alpha/ })).toBeVisible()
  await page.getByLabel('Filter repositories').press('ArrowRight')
  await expect(solo).toBeVisible()

  const filter = page.getByLabel('Filter repositories')
  await filter.fill('alph')
  await expect(page.getByRole('treeitem', { name: /alpha/ })).toBeVisible()
  await expect(page.getByRole('treeitem', { name: /solo/ })).toHaveCount(0)

  await page.getByRole('treeitem', { name: /alpha/ }).click()
  await expect(picker).toHaveCount(0)
  await page.getByRole('button', { name: 'alpha', exact: true }).hover()
  const card = page.getByRole('menu', { name: 'alpha' })
  await expect(card.getByText('~/src/alpha', { exact: true })).toBeVisible()
  await expect(card.locator('.project-menu__location svg')).toBeVisible()
  const remote = card.getByText(/^demo\/alpha-a-repository/)
  await expect(remote).toHaveCSS('white-space', 'nowrap')
  expect(await remote.evaluate((label) => label.scrollWidth)).toBeLessThanOrEqual(await remote.evaluate((label) => label.clientWidth))
  expect((await card.boundingBox())?.width).toBeGreaterThan(260)
  const iconBounds = await card.getByRole('img', { name: 'GitLab' }).boundingBox()
  const remoteBounds = await remote.boundingBox()
  if (!iconBounds || !remoteBounds) throw new Error('The remote icon and label should be visible')
  expect(iconBounds.y + iconBounds.height / 2).toBeCloseTo(remoteBounds.y + remoteBounds.height / 2, 0)
})

test('shows a Hugging Face icon beside its project remote', async ({ page }) => {
  await page.goto('/?hugging-face-remote')
  await page.getByRole('button', { name: 'demo', exact: true }).hover()
  const card = page.getByRole('menu', { name: 'demo' })
  await expect(card.getByText('datasets/example/data')).toBeVisible()
  await expect(card.getByRole('img', { name: 'Hugging Face' })).toBeVisible()
})

test('shows project icons, opens archive controls on hover, and composes in another project', async ({ page }) => {
  await page.goto('/?two-projects')
  const repository = page.getByRole('button', { name: 'demo', exact: true })
  const directory = page.getByRole('button', { name: 'notes', exact: true })
  await expect(repository.locator('.project-icon__git')).toBeVisible()
  await expect(directory.locator('.project-icon')).toBeVisible()
  await expect(directory.locator('.project-icon__git')).toHaveCount(0)
  await expect(page.getByRole('button', { name: /Old chat/ })).toHaveCount(0)
  await repository.hover()
  const card = page.getByRole('menu', { name: 'demo' })
  await expect(card).toBeVisible()
  await expect(card.getByText('/tmp/demo', { exact: true })).toBeVisible()
  await expect(card.getByText('example/demo')).toBeVisible()
  await expect(card.getByRole('img', { name: 'GitHub' })).toBeVisible()
  const rowMetrics = await card.locator('p, button').evaluateAll((rows) => rows.map((row) => {
    const style = getComputedStyle(row)
    return [style.fontSize, style.padding, row.getBoundingClientRect().height]
  }))
  expect(rowMetrics).toHaveLength(4)
  expect(rowMetrics.every((metrics) => metrics.join() === rowMetrics[0].join())).toBe(true)
  const rowBounds = await repository.locator('..').boundingBox()
  const cardBounds = await card.boundingBox()
  if (!rowBounds || !cardBounds) throw new Error('The project row and context card should be visible')
  expect(cardBounds.x - (rowBounds.x + rowBounds.width)).toBeCloseTo(4, 0)
  expect(cardBounds.y).toBeCloseTo(rowBounds.y, 0)
  await page.mouse.move(cardBounds.x + 12, rowBounds.y + rowBounds.height / 2, { steps: 20 })
  await expect(card).toBeVisible()
  const archive = page.getByRole('menuitemcheckbox', { name: 'Show archived' })
  await expect(archive).toHaveAttribute('aria-checked', 'false')
  await archive.locator('.project-menu__switch').click()
  await expect(archive).toHaveAttribute('aria-checked', 'true')
  await expect(card).toBeVisible()
  await expect(page.getByRole('button', { name: /Old chat/ })).toBeVisible()

  await directory.click()
  await expect(page.getByText('No tasks yet.')).toHaveCount(0)
  await expect(page.locator('.tasks')).toHaveCount(0)
  await repository.click()

  await directory.press('Shift+F10')
  await expect(archive).toBeFocused()
  await archive.press('ArrowDown')
  const projectSettings = page.getByRole('menuitem', { name: 'Project settings' })
  await expect(projectSettings).toBeFocused()
  await projectSettings.press('Enter')
  const settings = page.getByRole('region', { name: 'Settings', exact: true })
  await expect(settings.getByRole('heading', { name: 'Project settings' })).toBeVisible()
  await expect(settings.getByText('/tmp/notes')).toBeVisible()
  await expect(settings.getByLabel('Show archived tasks')).not.toBeChecked()
  await expect(repository).toHaveAttribute('aria-current', 'true')
  await settings.getByRole('button', { name: 'Workspace' }).click()
  await directory.hover()
  await projectSettings.click()
  await expect(settings.getByRole('heading', { name: 'Project settings' })).toBeVisible()

  await page.getByRole('button', { name: 'New chat in notes' }).click()
  await expect(directory).toHaveAttribute('aria-current', 'true')
  await expect(page.locator('.chat__title')).toHaveText('New task')
  await expect(page.locator('.task-list')).toHaveCount(0)
  await expect(page.getByRole('button', { name: /Old chat/ })).toHaveCount(0)
})

test('batch imports local Codex projects from the empty Projects list', async ({ page }) => {
  await page.goto('/?empty-projects')
  const projects = page.locator('.projects')
  await projects.getByRole('button', { name: 'New project' }).click()
  await expect(page.getByRole('dialog', { name: 'Add project' })).toBeVisible()
  await page.getByRole('button', { name: 'Close picker' }).click()
  await expect(projects.getByText('or import all from')).toBeVisible()
  const sources = projects.getByRole('group', { name: 'Project import sources' })
  for (const source of ['OpenCode', 'Pi', 'Claude Code', 'Freebuff']) {
    await expect(sources.getByRole('button', { name: source })).toBeDisabled()
  }
  const importProjects = sources.getByRole('button', { name: 'ChatGPT app' })
  await expect(importProjects).toBeEnabled()
  await importProjects.click()
  await expect(page.getByRole('dialog', { name: 'Add project' })).toHaveCount(0)
  await expect(page.getByRole('button', { name: /^notes/ })).toBeVisible()
  await expect(page.getByRole('button', { name: /^service/ })).toBeVisible()
  await expect(page.getByRole('button', { name: /^website/ })).toBeVisible()
  await expect(importProjects).toHaveCount(0)

  await page.getByRole('button', { name: 'Settings' }).click()
  await page.getByRole('navigation', { name: 'Settings categories' }).getByRole('button', { name: 'Projects' }).click()
  const settingsImport = page.getByRole('region', { name: 'Projects' }).getByRole('button', { name: 'ChatGPT app' })
  await settingsImport.click()
  await expect(page.getByRole('button', { name: /^tools/ })).toBeVisible()
  await expect(page.getByRole('button', { name: /^notes/ })).toHaveCount(1)
  await settingsImport.click()
  await expect(page.getByRole('status')).toHaveText('No new local Codex project folders to import.')

  await page.goto('/?empty-projects&no-codex')
  await expect(importProjects).toBeDisabled()
  await expect(projects.getByRole('button', { name: 'New project' })).toBeEnabled()
})

test('opens settings in the task pane and returns to the current chat', async ({ page }) => {
  await page.goto('/')
  await expect(page.getByRole('button', { name: 'Quit', exact: true })).toHaveCount(0)
  await page.getByLabel('Prompt').fill('keep this draft')

  const settings = page.getByRole('button', { name: 'Settings' })
  const chat = page.getByRole('region', { name: 'Task chat' })
  await settings.click()
  await expect(chat).toBeHidden()
  await expect(settings).toHaveAttribute('aria-pressed', 'true')
  const categories = page.getByRole('navigation', { name: 'Settings categories' })
  await expect(categories.getByRole('button', { name: 'Workspace' })).toHaveAttribute('aria-current', 'true')
  await page.getByLabel('Show archived tasks').check()
  await categories.getByRole('button', { name: 'Projects' }).click()
  await page.getByRole('region', { name: 'Projects' }).getByRole('button', { name: 'Add project' }).click()
  await page.getByRole('button', { name: 'Close picker' }).click()
  await settings.click()

  await expect(chat).toBeVisible()
  await expect(page.getByLabel('Prompt')).toHaveValue('keep this draft')
  await page.getByRole('button', { name: 'demo', exact: true }).press('Shift+F10')
  await expect(page.getByRole('menuitemcheckbox', { name: 'Show archived' })).toHaveAttribute('aria-checked', 'true')
})

test('applies the Send accent from Appearance and restores it after reload', async ({ page }) => {
  await page.goto('/')
  const settings = page.getByRole('button', { name: 'Settings', exact: true })
  const appearance = page.getByRole('button', { name: 'Appearance', exact: true })
  const accents = page.getByRole('group', { name: 'Send button accent' })
  const blue = accents.getByRole('radio', { name: 'Blue' })
  const teal = accents.getByRole('radio', { name: 'Teal' })
  await settings.click()
  await appearance.click()
  await expect(blue).toBeChecked()
  await teal.check()
  await settings.click()

  const composer = page.locator('.composer')
  const prompt = composer.getByLabel('Prompt')
  await expect(composer).toHaveCSS('box-shadow', /inset/)
  await expect(prompt).toHaveCSS('box-shadow', 'none')
  await prompt.fill('Use the new accent')
  await expect(composer.getByRole('button', { name: 'Send', exact: true })).toHaveCSS('background-color', 'rgb(128, 233, 214)')

  await page.reload()
  await settings.click()
  await appearance.click()
  await expect(teal).toBeChecked()

  const nextWindow = await page.context().newPage()
  await nextWindow.goto('/')
  await nextWindow.getByRole('button', { name: 'Settings', exact: true }).click()
  await nextWindow.getByRole('button', { name: 'Appearance', exact: true }).click()
  const expectedAccent = process.env.VITE_SHEPHERD_EPHEMERAL === '1' ? 'Blue' : 'Teal'
  await expect(nextWindow.getByRole('radio', { name: expectedAccent })).toBeChecked()
})

test('focuses the composer card and opens the toolbar menus above it', async ({ page }) => {
  await page.setViewportSize({ width: 900, height: 600 })
  await page.goto('/')
  const composer = page.locator('.composer')
  const prompt = composer.getByLabel('Prompt')
  await expect(prompt).toBeEnabled()
  await prompt.focus()
  await expect(composer).toHaveCSS('border-color', 'rgb(111, 125, 255)')
  await expect(prompt).toHaveCSS('outline-style', 'none')

  await composer.getByRole('button', { name: 'Attachments and commands' }).click()
  const attachments = composer.getByRole('menu', { name: 'Attachments and commands' })
  await expect(attachments.getByRole('menuitem', { name: 'Attach files (coming soon)' })).toBeDisabled()
  await expect(attachments.getByRole('menuitem', { name: 'Commands (coming soon)' })).toBeDisabled()

  const shell = composer.getByRole('button', { name: 'Shell approvals: Ask me' })
  await expect(shell).toHaveText('Ask me')
  await shell.click()
  const approvals = composer.getByRole('menu', { name: 'Shell approvals' })
  await expect(attachments).toBeHidden()
  await expect(approvals.getByRole('menuitemradio', { name: 'Ask me', checked: true })).toBeFocused()
  await expect(approvals.getByRole('menuitemradio', { name: 'Approve for me (coming soon)' })).toBeDisabled()
  const shellBounds = await shell.boundingBox()
  const menuBounds = await approvals.boundingBox()
  if (!shellBounds || !menuBounds) throw new Error('The approvals menu and trigger should be visible')
  expect(menuBounds.y + menuBounds.height).toBeLessThan(shellBounds.y)

  await composer.getByRole('button', { name: 'Agent: Codex' }).click()
  await expect(composer.getByRole('menuitemradio', { name: 'Codex', checked: true })).toBeVisible()
  await composer.getByRole('button', { name: 'Model / effort: gpt-6.1-terra' }).click()
  const model = composer.getByRole('menu', { name: 'Model / effort' })
  await expect(model.getByRole('menuitemradio', { name: 'gpt-6.1-terra', checked: true })).toBeVisible()
  await expect(model.getByRole('menuitemradio', { name: 'Choose effort (coming soon)' })).toBeDisabled()
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
