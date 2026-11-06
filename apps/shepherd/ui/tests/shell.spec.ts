/**
 * The S1 smoke test: the shell is rendered against a mocked IPC bridge, so the
 * components, commands and state flow under test are the real ones and only the
 * core is replaced by canned snapshots.
 */

import { expect, test } from '@playwright/test'

import type { AgentTerminal, Entry, Event as ShepherdEvent, FileChange, InspectorTab, TerminalLayout, TerminalSnapshot, TerminalSplit, WorkspaceLayout } from '../src/bridge'

test.beforeEach(async ({ context }) => {
  await context.addInitScript(() => {
    const callbacks = new Map()
    let nextCallbackId = 1
    let nextTask = 2
    let desktopProjectReads = 0
    let eventChannel
    let eventIndex = 0
    let cancelRequests = 0
    const fileListings: string[] = []
    const fileSearches: Array<{ id: string; taskId: string | null; mode: string }> = []
    const cancelledFileSearches: string[] = []
    const pendingFileSearches = new Map<string, () => void>()
    const fileWatchers = new Map<string, { taskId: string | null; channel: number; index: number }>()
    const unwatchedFiles: string[] = []
    const watchedSourceFiles = new Map<string | null, Set<string>>()
    let nextFileSearch = 1
    let releaseTerminalWrite: (() => void) | undefined
    let expiringTerminalSubscription: string | undefined
    const options = new URLSearchParams(window.location.search)
    let legacySession = options.has('legacy-session')
    let projectGrants = []
    let sessionGrants = []
    const agentTerminals = new Map<string, AgentTerminal[]>()

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
        state: options.has('agent-failed') ? 'failed' : options.has('interrupted-task') ? 'interrupted'
          : ['approval-fixture', 'scoped-approval', 'project-approval'].some((name) => options.has(name)) ? 'waiting' : 'ready',
        agent: options.has('acp-fallback') ? 'opencode' : 'codex',
        tier: options.has('acp-fallback') ? 'acp' : 'native',
        branch: 'shepherd/ship-it',
        worktreePath: '/tmp/demo/.git/shepherd/worktrees/ship-it',
        error: options.has('agent-failed') ? 'The agent exited with code 23 before the turn finished.' : null,
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
              choices: options.has('project-approval') ? ['allow', 'allowSession', 'allowProject', 'deny']
                : options.has('scoped-approval') ? ['allow', 'allowSession', 'deny'] : undefined,
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
    if (options.has('agent-failed')) {
      transcripts['task-1'] = options.has('reopened') ? [] : [entry('failure-1', 1, {
        type: 'failure',
        data: { kind: 'midTurnExit', text: tasks[0].error, detail: 'Agent stderr fixture' },
      })]
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

    let terminalLayout: TerminalLayout = { revision: 0, root: null, inspectorTab: 'terminals' }
    const otherInspectorTabs = new Map<string, InspectorTab>()
    const workspaceLayouts = new Map<string, WorkspaceLayout>()
    const subscriptions: string[] = []
    if (options.has('missing-terminal')) {
      terminalLayout = {
        ...terminalLayout,
        revision: 1,
        root: {
          type: 'split', axis: 'horizontal', ratio: 650,
          first: { type: 'pane', terminalId: 'terminal-1' },
          second: { type: 'pane', terminalId: 'terminal-missing' },
        },
      }
    }
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
        ...terminalLayout,
        revision: 1,
        root: {
          type: 'split', axis: 'horizontal', ratio: 500,
          first: { type: 'pane', terminalId: 'terminal-1' },
          second: { type: 'pane', terminalId: 'terminal-task' },
        },
      }
    }
    if (options.has('terminal-presets')) {
      tasks.push({ ...tasks[0], id: 'task-later', title: 'Other task' })
      terminals.push(
        { ...terminals[0], id: 'terminal-3', title: 'Terminal 3' },
        { ...terminals[0], id: 'terminal-4', title: 'Terminal 4' },
        { ...terminals[0], id: 'terminal-later', title: 'Other shell', scope: 'task', taskId: 'task-later' },
      )
      const panes = terminals.map(({ id }): TerminalSplit => ({ type: 'pane', terminalId: id }))
      terminalLayout = {
        ...terminalLayout,
        revision: 1,
        root: panes.reduceRight((second, first) => ({ type: 'split', axis: 'horizontal', ratio: 500, first, second })),
      }
    }

    // Every byte the shell writes to the PTY, in order.
    const writes = []
    const closedTerminals: string[] = []

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
      const limited = options.has('limited-adapter')
      const capabilities = task.state === 'draft' || legacySession ? null : {
        streaming: true, thoughts: true, toolCalls: true, approvals: !limited, resume: !limited,
        cancel: !limited, modelPicker: false, modePicker: false, images: false, resourceLinks: false,
        usage: true, agentTerminals: !limited, patchesInApprovals: !limited,
      }
      return {
        project: projects.find((candidate) => candidate.id === task.projectId),
        task: detail(task),
        layout: workspaceLayouts.get(taskId) ?? { inspectorWidth: null },
        cancelling: false,
        transcript: transcripts[taskId] ?? [],
        agentTerminals: agentTerminals.get(taskId) ?? [],
        adapterTier: task.state === 'draft' ? null : task.tier,
        capabilities,
        recovery: ['failed', 'interrupted'].includes(task.state) ? limited ? 'startNew' : capabilities ? 'resume' : 'reconnect' : null,
        diagnostics: options.has('acp-fallback') ? {
          command: '/existing/opencode acp', protocol: 'ACP v1 OpenCode 2.0.14',
          fallbackReason: 'OpenCode native server unavailable: no loopback server', stderrTail: 'agent stderr',
        } : task.state === 'waiting' ? { command: 'codex app-server', protocol: 'app-server', model: 'session-model', stderrTail: '' } : null,
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
          case 'subscribe':
            eventChannel = args.channel.id
            return null
          case 'plugin:path|resolve_directory':
            return '/tmp/home/'
          case 'project_list':
            return { projects: [...projects] }
          case 'project_approvals':
            return args.projectId === project.id
              ? { saved: [...projectGrants], sessions: [...sessionGrants] }
              : { saved: [], sessions: [] }
          case 'reset_approval_grant':
            if (args.projectId === project.id) {
              if (args.taskId) {
                sessionGrants = sessionGrants.filter((grant) => grant.id !== args.approvalId || grant.taskId !== args.taskId)
              } else {
                projectGrants = projectGrants.filter((grant) => grant.id !== args.approvalId)
              }
            }
            return null
          case 'codex_project_model':
            return 'gpt-6.1-terra'
          case 'opencode_configured':
            return options.has('configured-opencode')
          case 'claude_configured':
            return options.has('configured-claude')
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
          case 'list_files': {
            fileListings.push(args.path)
            if (args.path === 'src') {
              const paths = watchedSourceFiles.get(args.taskId) ?? new Set(['src/main.rs'])
              return { entries: [...paths].map((path) => ({ name: path.split('/').at(-1), path, directory: false })), incomplete: false }
            }
            const name = args.taskId ? 'worktree.txt' : 'README.md'
            return { entries: [
              { name: 'src', path: 'src', directory: true },
              { name, path: name, directory: false },
              ...(args.showDotfiles ? [{ name: '.env.example', path: '.env.example', directory: false }] : []),
            ], incomplete: false }
          }
          case 'read_workspace_file':
            return { firstLine: args.line === 120 ? 119 : 1, lines: [
              [{ text: args.path === 'src/main.rs' ? 'fn main() {}' : '<file>', color: '#a3be8c' }],
              [{ text: '// second line', color: null }],
            ], truncated: false }
          case 'watch_workspace_files': {
            const id = `file-watch-${fileWatchers.size + 1}`
            fileWatchers.set(id, { taskId: args.taskId, channel: args.channel.id, index: 0 })
            return id
          }
          case 'unwatch_workspace_files':
            unwatchedFiles.push(args.watchId)
            return null
          case 'reserve_file_search':
            return `search-${nextFileSearch++}`
          case 'search_workspace_files': {
            fileSearches.push({ id: args.searchId, taskId: args.taskId, mode: args.query.mode })
            if (args.query.mode === 'name') {
              const matches = 'main.rs'.includes(args.query.query)
                ? [{ path: 'src/main.rs', line: null, excerpt: null }] : []
              return { matches, incomplete: false }
            }
            return new Promise((resolve) => pendingFileSearches.set(args.searchId, () => {
              pendingFileSearches.delete(args.searchId)
              resolve({ matches: [{ path: 'src/main.rs', line: 120, excerpt: '// second line' }], incomplete: true })
            }))
          }
          case 'cancel_file_search':
            cancelledFileSearches.push(args.searchId)
            return null
          case 'save_workspace_layout':
            workspaceLayouts.set(args.taskId, args.layout)
            return args.layout
          case 'cancel_task': {
            cancelRequests += 1
            const task = tasks.find((candidate) => candidate.id === args.taskId)
            if (!task || task.state !== 'waiting') throw new Error('No active turn to cancel')
            window.addEventListener('test-cancel-finished', () => {
              task.state = 'ready'
              transcripts[task.id] = transcripts[task.id].map((entry) => entry.kind.type === 'permission'
                ? { ...entry, kind: { ...entry.kind, data: { ...entry.kind.data, withdrawn: true } } }
                : entry)
              callbacks.get(eventChannel)?.({
                index: eventIndex++,
                message: { type: 'taskWorkspaceChanged', data: workspace(task.id) },
              })
            }, { once: true })
            return { ...workspace(task.id), cancelling: true }
          }
          case 'resume_task': {
            const task = tasks.find((candidate) => candidate.id === args.taskId)
            if (!task || task.state !== 'interrupted') throw new Error('Task cannot be resumed')
            task.state = 'ready'
            legacySession = false
            transcripts[task.id] = [
              entry('resumed', 1, {
                type: 'notice',
                data: {
                  text: options.has('limited-adapter') ? 'started a new agent session' : 'resumed the previous agent session',
                  isError: false,
                },
              }),
            ]
            return workspace(task.id)
          }
          case 'create_draft': {
            const id = `task-${nextTask}`
            nextTask += 1
            tasks.unshift({
              id,
              projectId: args.projectId,
              title: args.title,
              state: 'draft',
              agent: args.agent,
              tier: 'native',
              branch: null,
              worktreePath: null,
              error: null,
              createdAt: 1_760_000_500_000,
            })
            transcripts[id] = []
            return workspace(id)
          }
          case 'set_draft_agent': {
            const task = tasks.find((candidate) => candidate.id === args.taskId)
            if (!task || task.state !== 'draft') throw new Error('Task is not a draft')
            task.agent = args.agent
            return workspace(task.id)
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
            if (options.has('cancel-approval')) {
              transcripts[task.id].push(entry(`entry-${task.id}-3`, 3, {
                type: 'permission',
                data: {
                  approvalId: 'cancel-approval',
                  title: 'Run a command',
                  arguments: null,
                  paths: [],
                  patch: null,
                  decision: null,
                  withdrawn: false,
                },
              }))
            }
            return workspace(task.id)
          }
          case 'resolve_approval': {
            const rows = transcripts[args.taskId]
            const request = rows.find((row) => row.kind.type === 'permission' && row.kind.data.approvalId === args.approvalId)
            const projectGrant = options.has('project-approval')
            if (!request || request.kind.type !== 'permission' || args.decision !== (projectGrant ? 'allowProject' : 'allowSession')) {
              throw new Error('Expected a scoped approval')
            }
            const grant = {
              id: 'grant-1',
              agent: 'claude',
              title: request.kind.data.title,
              arguments: request.kind.data.arguments,
              paths: request.kind.data.paths,
            }
            if (projectGrant) projectGrants.push(grant)
            else sessionGrants.push({ ...grant, taskId: args.taskId, taskTitle: 'Ship it' })
            const settled = entry(request.id, request.sequence, {
              type: 'permission',
              data: { ...request.kind.data, decision: args.decision, source: 'user' },
            })
            const repeated = entry('auto-approval', 4, {
              type: 'permission',
              data: {
                ...request.kind.data,
                approvalId: 'auto-approval',
                title: 'Run tests again',
                decision: 'allow',
                source: projectGrant ? 'projectGrant' : 'sessionGrant',
                withdrawn: false,
              },
            })
            transcripts[args.taskId] = [...rows.map((row) => row === request ? settled : row), repeated]
            return workspace(args.taskId)
          }
          case 'terminal_layout':
            return args.projectId === project.id ? terminalLayout
              : { revision: 0, root: null, inspectorTab: otherInspectorTabs.get(args.projectId) ?? 'terminals' }
          case 'save_terminal_layout':
            terminalLayout = { ...args.layout, revision: args.layout.revision + 1, inspectorTab: terminalLayout.inspectorTab }
            return terminalLayout
          case 'save_inspector_tab':
            if (args.projectId === project.id) terminalLayout = { ...terminalLayout, inspectorTab: args.tab }
            else otherInspectorTabs.set(args.projectId, args.tab)
            return null
          case 'terminal_list':
            return { projectId: args.projectId, revision: 0, terminals: [...terminals] }
          case 'start_terminal':
          case 'restart_terminal': {
            if (options.has('early-terminal-exit')) {
              const id = args.terminalId ?? 'terminal-early-exit'
              const running = { ...terminals[0], id, title: 'Short-lived shell' }
              const remaining = terminals.filter((terminal) => terminal.id !== id)
              const live = { projectId: project.id, revision: 1, terminals: [...remaining, running] }
              const ended = { ...running, endedAt: 1_760_000_300_000, exitCode: 23 }
              window.__replayTerminalList = () => window.__emitCoreEvent({ type: 'terminalListChanged', data: live })
              return new Promise((resolve) => {
                window.__finishTerminalLaunch = () => {
                  window.__emitCoreEvent({
                    type: 'terminalListChanged',
                    data: { projectId: project.id, revision: 2, terminals: [...remaining, ended] },
                  })
                  resolve(live)
                }
              })
            }
            return { projectId: args.projectId ?? project.id, revision: 0, terminals: [...terminals] }
          }
          case 'start_freebuff_terminal':
            return 'freebuff-fixture'
          case 'subscribe_terminal': {
            subscriptions.push(args.terminalId)
            const subscriptionId = `subscription-${args.terminalId}-${subscriptions.length}`
            if (options.has('wrapped-word-selection')) {
              const rows = [
                [{ text: 'a', attributes: 1 }, { text: '', attributes: 32 }],
                [{ text: '界', attributes: 2 }, { text: '', attributes: 32 }],
                [{ text: 'b', attributes: 1 }, { text: 'c', attributes: 1 }],
                [{ text: 'd', attributes: 1 }, { text: 'e', attributes: 1 }],
              ]
              const bytes = [1, 2, 2, 0, rows.length, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, rows.length, 0]
              const encoder = new TextEncoder()
              for (const [row, cells] of rows.entries()) {
                bytes.push(row, 0, cells.length, 0)
                for (const { text, attributes } of cells) {
                  const encoded = encoder.encode(text)
                  bytes.push(1, 0, attributes, 234, 230, 230, 0, 22, 19, 19, 0, encoded.length, 0, 0, 0, ...encoded)
                }
              }
              bytes.push(1, 0, 0, 0, 1, 0, 0, 0)
              callbacks.get(args.channel.id)?.({ index: 0, message: Uint8Array.from(bytes).buffer })
              return subscriptionId
            }
            const recovering = options.has('expired-terminal-subscription') && expiringTerminalSubscription !== undefined
            const frame = new Uint8Array([
              1, 3, 2, 0, 1, 0, 1, 0, 0, 0, 0, 0, 0, 0, 1, 4, 0, 0, 1, 0,
              0, 0, 1, 0, 2, 0, 1, 234, 230, 230, 0, 22, 19, 19, 0, 1, 0, 0, 0, 65,
              1, 0, 0, 0, 1, 0, 0, 0,
            ])
            if (recovering) frame[frame.length - 9] = 'D'.charCodeAt(0)
            callbacks.get(args.channel.id)?.({ index: 0, message: frame.buffer })
            const update = new Uint8Array([
              1, 1, 2, 0, 1, 0, 1, 0, 0, 0, 0, 0, 0, 0, 3, 4, 0, 0, 1, 0,
              0, 0, 1, 0, 2, 0, 1, 234, 230, 230, 0, 22, 19, 19, 0, 1, 0, 0, 0, 66,
              2, 0, 0, 0, 2, 0, 0, 0,
            ])
            if (options.has('kitty-terminal')) {
              new DataView(update.buffer).setUint32(14, (31 << 18) | 1025, true)
            }
            if (options.has('kitty-shortcuts')) {
              new DataView(update.buffer).setUint32(14, (3 << 18) | 1025, true)
            }
            if (options.has('mouse-terminal')) {
              new DataView(update.buffer).setUint32(14, (1 << 13) | (1 << 5) | 1025, true)
            }
            if (options.has('interrupted-terminal-paste')) {
              new DataView(update.buffer).setUint32(14, 4 | (1 << 4), true)
            }
            if (options.has('expired-terminal-subscription') && !recovering) expiringTerminalSubscription = subscriptionId
            if (!recovering) callbacks.get(args.channel.id)?.({ index: 1, message: update.buffer })
            if (options.has('scrollback-selection')) {
              let index = recovering ? 1 : 2
              Object.assign(window, {
                __emitTerminalHistory: (offset: number, glyph: string) => {
                  const history = frame.slice()
                  history[1] = 2
                  history[history.length - 9] = glyph.charCodeAt(0)
                  const header = new DataView(history.buffer)
                  header.setUint32(10, offset, true)
                  header.setUint32(history.length - 8, index + 1, true)
                  header.setUint32(history.length - 4, index + 1, true)
                  callbacks.get(args.channel.id)?.({ index: index++, message: history.buffer })
                },
              })
            }
            return subscriptionId
          }
          case 'acknowledge_terminal_frame':
            if (args.subscriptionId === expiringTerminalSubscription && args.sequence === 3) {
              throw { code: 'terminal_subscription_ended', message: 'terminal frame subscription ended' }
            }
            return null
          case 'terminal_write':
            writes.push(args.data)
            if (options.has('interrupted-terminal-paste') && writes.length === 2) {
              throw { message: 'The terminal stopped accepting the paste.' }
            }
            if (options.has('blocked-terminal-write') && writes.length === 1) {
              await new Promise<void>((resolve) => { releaseTerminalWrite = resolve })
            }
            return null
          case 'close_terminal':
            closedTerminals.push(args.terminalId)
            return null
          case 'quit':
            return { interruptedTasks: 0, liveTerminals: 0 }
          default:
            return null
        }
      },
    }

    window.__writes = writes
    window.__releaseTerminalWrite = () => releaseTerminalWrite?.()
    window.__closedTerminals = closedTerminals
    window.__subscriptions = subscriptions
    window.__fileListings = fileListings
    window.__unwatchedFiles = unwatchedFiles
    window.__fileWatchIds = () => [...fileWatchers.keys()]
    window.__publishFileChanges = (id: string, files: FileChange[]) => {
      const watcher = fileWatchers.get(id)
      if (!watcher) throw new Error(`Unknown fixture watcher ${id}`)
      const paths = watchedSourceFiles.get(watcher.taskId) ?? new Set(['src/main.rs'])
      for (const file of files) {
        if (file.originalPath) paths.delete(file.originalPath)
        if (file.path.startsWith('src/')) {
          if (file.kind === 'deleted') paths.delete(file.path)
          else paths.add(file.path)
        }
      }
      watchedSourceFiles.set(watcher.taskId, paths)
      callbacks.get(watcher.channel)?.({ index: watcher.index++, message: { type: 'changes', files } })
    }
    window.__fileSearches = fileSearches
    window.__cancelledFileSearches = cancelledFileSearches
    window.__finishFileSearch = (id: string) => pendingFileSearches.get(id)?.()
    window.__cancelRequests = () => cancelRequests
    window.__terminalLayout = () => terminalLayout
    window.__emitCoreEvent = (event: ShepherdEvent) =>
      callbacks.get(eventChannel)?.({ index: eventIndex++, message: event })
    window.addEventListener('test-approval-session-exited', () => {
      sessionGrants = []
      window.__emitCoreEvent({ type: 'projectApprovalsChanged', data: { projectId: project.id } })
    })
    window.__setCommandEntries = (taskId: string, entries: Entry[]) => {
      transcripts[taskId] = [...(transcripts[taskId] ?? []).filter((row) => row.kind.type !== 'toolCall'), ...entries]
      window.__emitCoreEvent({ type: 'taskWorkspaceChanged', data: workspace(taskId) })
    }
    window.__setAgentTerminals = (taskId: string, terminals: AgentTerminal[]) => {
      agentTerminals.set(taskId, terminals)
      window.__emitCoreEvent({ type: 'taskWorkspaceChanged', data: workspace(taskId) })
    }
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

  const project = page.getByRole('button', { name: 'demo', exact: true })
  await expect(project).toBeVisible()
  await project.press('Tab')
  const compose = page.getByRole('button', { name: 'New chat in demo' })
  await expect(compose).toBeFocused()
  await expect(compose).toHaveCSS('opacity', '1')
  await compose.press('Enter')
  await expect(page.locator('.task', { hasText: 'New task' })).toBeVisible()
  await expect(page.getByText('Proposed branch')).toBeVisible()
  await expect(page.locator('.chat__adapter')).toHaveCount(0)
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

test('browses a worktree and project root with lazy folders and read-only previews', async ({ page }) => {
  await page.goto('/')
  await expect(page.getByRole('region', { name: 'Task chat' }).getByText('ready to go')).toBeVisible()
  expect(await page.evaluate(() => window.__fileListings)).toEqual([])
  await page.getByRole('tab', { name: 'Files' }).click()

  const files = page.getByRole('region', { name: 'Workspace files' })
  const root = files.getByLabel('File root')
  await expect(root).toHaveValue('worktree')
  await expect(files.getByRole('button', { name: 'worktree.txt' })).toBeVisible()
  expect(await page.evaluate(() => window.__fileListings)).toEqual([''])
  const folder = files.getByRole('button', { name: 'src' })
  await expect(folder).toHaveAttribute('aria-expanded', 'false')
  await folder.click()
  await files.getByRole('button', { name: 'main.rs' }).click()
  expect(await page.evaluate(() => window.__fileListings)).toEqual(['', 'src'])
  const viewer = files.getByRole('region', { name: 'File preview' })
  await expect(viewer.getByText('src/main.rs')).toBeVisible()
  await expect(viewer.getByText('fn main() {}')).toHaveCSS('color', 'rgb(163, 190, 140)')
  await expect(viewer.locator('.files-viewer__number')).toHaveText(['1', '2'])

  await root.selectOption('project')
  await expect(viewer).toHaveCount(0)
  await expect(files.getByRole('button', { name: 'README.md' })).toBeVisible()
  await expect(files.getByRole('button', { name: 'worktree.txt' })).toHaveCount(0)
  await expect(files.getByRole('button', { name: '.env.example' })).toHaveCount(0)
  await files.getByLabel('Show dotfiles').check()
  await expect(files.getByRole('button', { name: '.env.example' })).toBeVisible()
  await files.getByRole('button', { name: 'README.md' }).click()
  await expect(viewer.getByText('<file>', { exact: true })).toBeVisible()
})

test('filters by name, cancels content searches, and opens matches at their line', async ({ page }) => {
  await page.goto('/')
  await expect(page.getByRole('region', { name: 'Task chat' }).getByText('ready to go')).toBeVisible()
  await page.getByRole('tab', { name: 'Files' }).click()
  const files = page.getByRole('region', { name: 'Workspace files' })
  const query = files.getByRole('searchbox', { name: 'Find files' })
  const results = files.getByRole('region', { name: 'File search results' })
  const searches = () => page.evaluate(() => window.__fileSearches)
  const cancelled = () => page.evaluate(() => window.__cancelledFileSearches)
  const contentRequest = async (count: number) => {
    await expect.poll(async () => (await searches()).filter((request) => request.mode === 'contents').length).toBe(count)
    const request = (await searches()).filter((request) => request.mode === 'contents').at(-1)
    if (!request) throw new Error('Expected a content search')
    return request.id
  }
  const finish = (id: string) => page.evaluate((searchId) => window.__finishFileSearch(searchId), id)

  await query.fill('main')
  await expect(results.getByRole('button', { name: 'src/main.rs' })).toBeVisible()
  expect(await searches()).toEqual([{ id: 'search-1', taskId: 'task-1', mode: 'name' }])

  await query.press('Enter')
  const first = await contentRequest(1)
  await files.getByRole('button', { name: 'Cancel search' }).click()
  await expect.poll(cancelled).toContain(first)
  await finish(first)
  await expect(results.getByRole('status')).toHaveText('Search cancelled.')
  await expect(results.getByText('// second line')).toHaveCount(0)

  await files.getByRole('button', { name: 'Search contents' }).click()
  await finish(await contentRequest(2))
  await expect(results.getByText('Some files or results were skipped.')).toBeVisible()
  await results.getByRole('button', { name: /src\/main\.rs:120/ }).click()
  const match = files.locator('.files-viewer__line[aria-current="location"]')
  await expect(match).toContainText('// second line')
  await expect(match.locator('.files-viewer__number')).toHaveText('120')

  await files.getByRole('button', { name: 'Search contents' }).click()
  const third = await contentRequest(3)
  await files.getByLabel('File root').selectOption('project')
  await expect.poll(cancelled).toContain(third)
  await finish(third)
  await expect(results).toHaveCount(0)
  await query.fill('main')
  await expect(results.getByRole('button', { name: 'src/main.rs' })).toBeVisible()
  expect((await searches()).at(-1)).toMatchObject({ taskId: null, mode: 'name' })
})

test('refreshes Git badges and open folders and stops watching the old root', async ({ page }) => {
  await page.goto('/')
  await expect(page.getByRole('region', { name: 'Task chat' }).getByText('ready to go')).toBeVisible()
  await page.getByRole('tab', { name: 'Files' }).click()
  const files = page.getByRole('region', { name: 'Workspace files' })
  const root = files.getByLabel('File root')
  const folder = files.getByRole('button', { name: 'src', exact: true })
  const watchIds = () => page.evaluate(() => window.__fileWatchIds())
  const unwatched = () => page.evaluate(() => window.__unwatchedFiles)
  const publish = (id: string, changes: FileChange[]) => page.evaluate(({ id, changes }) => window.__publishFileChanges(id, changes), { id, changes })
  await expect.poll(watchIds).toEqual(['file-watch-1'])
  await folder.click()
  await expect(files.getByRole('button', { name: 'main.rs' })).toBeVisible()
  await folder.focus()

  await publish('file-watch-1', [
    { path: 'src/main.rs', kind: 'modified', originalPath: null },
    { path: 'src/added.rs', kind: 'added', originalPath: null },
    { path: 'src/renamed.rs', kind: 'renamed', originalPath: 'src/old.rs' },
    { path: 'retired.md', kind: 'deleted', originalPath: null },
  ])
  await expect(files.getByRole('button', { name: /main\.rs/ }).getByRole('img', { name: 'Modified' })).toBeVisible()
  await expect(files.getByRole('button', { name: /added\.rs/ }).getByRole('img', { name: 'Added' })).toBeVisible()
  await expect(files.getByRole('button', { name: /renamed\.rs/ }).getByRole('img', { name: 'Renamed from src/old.rs' })).toBeVisible()
  await expect(files.getByText('retired.md')).toBeVisible()
  await expect(files.getByRole('img', { name: 'Deleted', exact: true })).toBeVisible()
  await expect(folder).toBeFocused()
  await expect(folder).toHaveAttribute('aria-expanded', 'true')

  await publish('file-watch-1', [])
  await expect(files.getByRole('img')).toHaveCount(0)
  await expect(files.getByRole('button', { name: 'added.rs' })).toBeVisible()
  await root.selectOption('project')
  await expect.poll(unwatched).toContain('file-watch-1')
  await expect.poll(watchIds).toContain('file-watch-2')
  await publish('file-watch-1', [{ path: 'retired.md', kind: 'deleted', originalPath: null }])
  await expect(files.getByText('retired.md')).toHaveCount(0)
  await page.getByRole('tab', { name: 'Terminals' }).click()
  await expect.poll(unwatched).toContain('file-watch-2')
})

test('resumes an interrupted task from its conversation header', async ({ page }) => {
  await page.goto('/?interrupted-task')

  const chat = page.getByRole('region', { name: 'Task chat' })
  const resume = chat.getByRole('button', { name: 'Resume' })
  await expect(chat.locator('.chat__state')).toHaveText('interrupted')
  await expect(chat.getByLabel('Prompt')).toBeDisabled()
  await resume.click()
  await expect(chat.locator('.chat__state')).toHaveText('ready')
  await expect(chat.getByText('resumed the previous agent session')).toBeVisible()
  await expect(chat.getByLabel('Prompt')).toBeEnabled()
  await expect(resume).toHaveCount(0)
})

test('cancels an active turn and waits for the settled workspace', async ({ page }) => {
  await page.goto('/?cancel-approval')
  const chat = page.getByRole('region', { name: 'Task chat' })
  const prompt = chat.getByLabel('Prompt')
  for (const shortcut of [false, true]) {
    await prompt.fill('wait for a decision')
    await chat.getByRole('button', { name: 'Send' }).click()
    const cancel = chat.getByRole('button', { name: 'Cancel turn' })
    await expect(cancel).toBeEnabled()
    if (shortcut) await page.keyboard.press('ControlOrMeta+.')
    else await cancel.click()
    await expect(chat.getByRole('button', { name: 'Cancelling…' })).toBeDisabled()
    await expect(chat.getByRole('button', { name: 'Allow' })).toBeDisabled()
    await expect(chat.locator('.chat__state')).toHaveText('waiting')
    await expect(prompt).toBeDisabled()
    await page.evaluate(() => window.dispatchEvent(new Event('test-cancel-finished')))
    await expect(chat.locator('.chat__state')).toHaveText('ready')
    await expect(chat.locator('.entry__permission')).toContainText('withdrawn')
    await expect(chat.getByRole('button', { name: 'Allow' })).toHaveCount(0)
    await expect(prompt).toBeEnabled()
  }
})

test('shows an approval with its tool, arguments and diff', async ({ page }) => {
  await page.goto('/?approval-fixture')

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

test('reports the ACP tier and the reason the native adapter was skipped', async ({ page }) => {
  await page.goto('/?acp-fallback')
  const chat = page.getByRole('region', { name: 'Task chat' })
  await expect(chat.getByText('ACP · fallback')).toBeVisible()
  await expect(chat.getByText('OpenCode native server unavailable: no loopback server')).toBeVisible()
  await chat.getByText('Diagnostics', { exact: true }).click()
  await expect(chat.getByText('ACP v1 OpenCode 2.0.14')).toBeVisible()
  await expect(chat.getByText('/existing/opencode acp')).toBeVisible()
  await expect(chat.getByText(/Tool approvals, Resume, Cancel turn/)).toBeVisible()
  await expect(chat.getByText('agent stderr')).toBeVisible()
})

test('hides actions the connected adapter cannot perform', async ({ page }) => {
  await page.goto('/?approval-fixture&limited-adapter')
  const chat = page.getByRole('region', { name: 'Task chat' })
  await expect(chat.locator('.chat__state')).toHaveText('waiting')
  await expect(chat.getByRole('button', { name: 'Cancel turn' })).toHaveCount(0)
  await expect(chat.locator('.entry__permission').getByRole('button')).toHaveCount(0)
  await expect(chat.getByRole('button', { name: 'Shell approvals: Ask me' })).toHaveCount(0)
  await page.keyboard.press('ControlOrMeta+.')
  expect(await page.evaluate(() => window.__cancelRequests())).toBe(0)

  await page.evaluate(() => window.__setAgentTerminals('task-1', [
    { id: 'agent-unsupported', title: 'Unsupported terminal', live: true, exitCode: null },
  ]))
  const inspector = page.getByRole('complementary', { name: 'Inspector' })
  await expect(inspector.getByLabel('Terminal output').first()).toBeVisible()
  await expect(inspector.getByText('agent: Unsupported terminal')).toHaveCount(0)
  expect(await page.evaluate(() => window.__subscriptions)).not.toContain('agent-unsupported')
})

test('shows shell approvals only after a legacy session reconnects and reports support', async ({ page }) => {
  await page.goto('/?interrupted-task&legacy-session')
  const chat = page.getByRole('region', { name: 'Task chat' })
  const approvals = chat.getByRole('button', { name: 'Shell approvals: Ask me' })
  await expect(chat.locator('.chat__state')).toHaveText('interrupted')
  await expect(approvals).toHaveCount(0)
  await chat.getByRole('button', { name: 'Reconnect', exact: true }).click()
  await expect(approvals).toBeVisible()
})

test('opens a fresh session when the last adapter could not resume', async ({ page }) => {
  await page.goto('/?interrupted-task&limited-adapter')
  const chat = page.getByRole('region', { name: 'Task chat' })
  await expect(chat.getByRole('button', { name: 'Resume', exact: true })).toHaveCount(0)
  await chat.getByRole('button', { name: 'Start new session' }).click()
  await expect(chat.getByText('started a new agent session')).toBeVisible()
  await expect(chat.getByLabel('Prompt')).toBeEnabled()
})

test('sends a scoped approval and identifies an automatic session grant', async ({ page }) => {
  await page.goto('/?scoped-approval')
  const request = page.locator('.entry__permission').filter({ hasText: 'Run tests' }).first()
  await expect(request.getByRole('button', { name: 'Allow once' })).toBeVisible()
  await request.getByRole('button', { name: 'Allow for session' }).click()
  await expect(request.locator('.entry__permission-decision')).toHaveText('allowed for session')
  const repeated = page.locator('.entry__permission').filter({ hasText: 'Run tests again' })
  await expect(repeated.locator('.entry__permission-decision')).toHaveText('allowed by session grant')
  await expect(repeated.getByRole('button')).toHaveCount(0)
})

test('lists and resets a saved ACP project grant from Settings', async ({ page }) => {
  await page.goto('/?project-approval')
  await page.getByRole('button', { name: 'Always allow for project' }).click()
  await expect(page.getByText('allowed by project grant')).toBeVisible()
  await page.getByRole('button', { name: 'Settings', exact: true }).click()
  await page.getByRole('navigation', { name: 'Settings categories' }).getByRole('button', { name: 'Projects' }).click()
  const approvals = page.getByRole('region', { name: 'Saved project approvals' })
  await expect(approvals).toContainText('Claude Code')
  await expect(approvals).toContainText('cargo test')
  await approvals.getByRole('button', { name: 'Reset approval for Run tests' }).click()
  await expect(approvals).toContainText('No saved project approvals.')
})

for (const action of ['reset', 'session exit']) {
  test(`removes a live ACP approval after ${action}`, async ({ page }) => {
    await page.goto('/?scoped-approval')
    await page.getByRole('button', { name: 'Allow for session' }).click()
    await expect(page.getByText('allowed for session', { exact: true })).toBeVisible()
    await page.getByRole('button', { name: 'Settings', exact: true }).click()
    await page.getByRole('navigation', { name: 'Settings categories' }).getByRole('button', { name: 'Projects' }).click()
    const approvals = page.getByRole('region', { name: 'Live session approvals' })
    await expect(approvals).toContainText('Claude Code · Ship it')
    await expect(approvals).toContainText('cargo test')
    if (action === 'reset') {
      await approvals.getByRole('button', { name: 'Reset approval for Run tests in Ship it' }).click()
    } else {
      await page.evaluate(() => window.dispatchEvent(new Event('test-approval-session-exited')))
    }
    await expect(approvals).toContainText('No live session approvals.')
  })
}

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
  await expect(agentMenu.getByRole('menuitemradio', { name: 'OpenCode' })).toBeDisabled()
  await expect(agentMenu.getByRole('menuitemradio', { name: 'Claude Code' })).toBeDisabled()
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

test('uses the configured OpenCode agent and keeps the choice on the draft', async ({ page }) => {
  await page.goto('/?two-projects&configured-opencode')
  await page.getByRole('button', { name: 'notes', exact: true }).click()
  const welcome = page.getByRole('heading', { name: 'What should Codex build in notes?' })
  await welcome.getByRole('button', { name: 'Agent: Codex' }).click()
  await welcome.getByRole('menuitemradio', { name: 'OpenCode' }).click()
  await expect(page.getByRole('heading', { name: 'What should OpenCode build in notes?' })).toBeVisible()

  await page.getByLabel('Prompt').fill('Review the code')
  const composer = page.locator('.composer')
  await expect(page.getByRole('button', { name: 'Discard draft' })).toBeVisible()
  await expect(composer.getByRole('button', { name: 'Agent: OpenCode' })).toBeVisible()
  await expect(composer.getByRole('button', { name: /Model \/ effort/ })).toHaveCount(0)

  await composer.getByRole('button', { name: 'Agent: OpenCode' }).click()
  await composer.getByRole('menuitemradio', { name: 'Codex' }).click()
  await composer.getByRole('button', { name: 'Agent: Codex' }).click()
  await composer.getByRole('menuitemradio', { name: 'OpenCode' }).click()
  await composer.getByRole('button', { name: 'Send' }).click()
  await expect(page.getByText('working on it')).toBeVisible()
  await composer.getByRole('button', { name: 'Agent: OpenCode' }).click()
  await expect(composer.getByRole('menuitemradio', { name: 'Codex' })).toBeDisabled()
})

test('selects a configured Claude Code ACP agent for a new draft', async ({ page }) => {
  await page.goto('/?two-projects&configured-claude')
  await page.getByRole('button', { name: 'notes', exact: true }).click()
  const welcome = page.getByRole('heading', { name: 'What should Codex build in notes?' })
  await welcome.getByRole('button', { name: 'Agent: Codex' }).click()
  await welcome.getByRole('menuitemradio', { name: 'Claude Code' }).click()
  await expect(page.getByRole('heading', { name: 'What should Claude Code build in notes?' })).toBeVisible()
  await page.getByLabel('Prompt').fill('Review the code')
  await expect(page.getByRole('button', { name: 'Discard draft' })).toBeVisible()
  const composer = page.locator('.composer')
  await expect(composer.getByRole('button', { name: 'Agent: Claude Code' })).toBeVisible()
  await expect(composer.getByRole('button', { name: /Model \/ effort/ })).toHaveCount(0)
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
  await input.press('Backspace')
  await input.press('Control+Backspace')
  await input.press('Alt+Shift+Tab')
  await input.press('ArrowUp')

  await expect
    .poll(() =>
      page.evaluate(() => (window as unknown as { __writes: string[] }).__writes ?? []),
    )
    .toEqual(['a', '\u0003', '\r', '\x7f', '\x08', '\x1b\x1b[Z', '\u001bOA'])
})

test('sends legacy ASCII control aliases to the terminal', async ({ page }) => {
  await page.goto('/')
  await expect(page.getByLabel('Terminal output')).toHaveText('BB')
  const input = page.getByRole('textbox', { name: 'Terminal', exact: true })
  await input.focus()

  for (const key of ['/', '3', '4', '5', '6', '7', '8', '~']) {
    await input.press(`Control+${key}`)
  }
  await input.press('Control+Alt+/')
  await input.evaluate((element) => element.dispatchEvent(new KeyboardEvent('keydown', {
    key: 'ß', code: 'Minus', ctrlKey: true, bubbles: true, cancelable: true,
  })))

  await expect.poll(() => page.evaluate(() => window.__writes)).toEqual([
    '\x1f', '\x1b', '\x1c', '\x1d', '\x1e', '\x1f', '\x7f', '\x1e', '\x1b\x1f', 'ß',
  ])
})

for (const { name, platform, route, writes } of [
  { name: 'macOS legacy', platform: 'MacIntel', route: '/', writes: ['#', '\x1b[1;3D'] },
  { name: 'macOS Kitty', platform: 'MacIntel', route: '/?kitty-terminal',
    writes: ['\x1b[0;;35u', '\x1b[57443;3u', '\x1b[1;3D', '\x1b[1;3:3D', '\x1b[57443;1:3u'] },
  { name: 'Linux legacy', platform: 'Linux x86_64', route: '/', writes: ['\x1b#', '\x1b[1;3D'] },
]) {
  test(`routes printable Alt input in ${name}`, async ({ page }) => {
    await page.addInitScript((value) => Object.defineProperty(navigator, 'platform', { value }), platform)
    await page.goto(route)
    await expect(page.getByLabel('Terminal output')).toHaveText('BB')
    const input = page.getByRole('textbox', { name: 'Terminal', exact: true })
    await input.focus()
    await input.evaluate((element: HTMLTextAreaElement) => {
      const key = new KeyboardEvent('keydown', {
        key: '#', code: 'Digit3', altKey: true, bubbles: true, cancelable: true,
      })
      if (element.dispatchEvent(key)) {
        element.value = '#'
        element.dispatchEvent(new InputEvent('input', { data: '#', inputType: 'insertText', bubbles: true }))
      }
    })
    await input.press('Alt+ArrowLeft')
    await expect.poll(() => page.evaluate(() => window.__writes)).toEqual(writes)
  })
}

test('bounds stalled terminal input and reserves room for key release', async ({ page }) => {
  await page.goto('/?kitty-terminal&blocked-terminal-write')
  const input = page.getByRole('textbox', { name: 'Terminal', exact: true })
  await expect(page.getByText('BB', { exact: true })).toBeAttached()
  const writes = () => page.evaluate(() => (window as unknown as { __writes: string[] }).__writes)

  await input.evaluate((element) => {
    for (let index = 0; index < 130; index++) {
      element.dispatchEvent(new KeyboardEvent('keydown', {
        key: 'a', code: 'KeyA', bubbles: true, cancelable: true, repeat: index > 0,
      }))
    }
  })
  await expect(page.getByText('Terminal input is full.', { exact: false })).toBeVisible()
  await expect.poll(writes).toEqual(['\x1b[97;;97u'])

  await input.dispatchEvent('keyup', { key: 'a', code: 'KeyA', bubbles: true, cancelable: true })
  await page.evaluate(() => (window as unknown as { __releaseTerminalWrite: () => void }).__releaseTerminalWrite())
  await expect.poll(async () => (await writes()).length).toBe(128)
  expect((await writes()).at(-1)).toBe('\x1b[97;1:3u')

  await input.evaluate((element) => {
    const clipboard = new DataTransfer()
    clipboard.setData('text/plain', '界'.repeat(400_000))
    element.dispatchEvent(new ClipboardEvent('paste', { bubbles: true, cancelable: true, clipboardData: clipboard }))
  })
  await expect(page.getByText('Terminal paste exceeds the 1 MiB input limit.')).toBeVisible()
  expect(await writes()).toHaveLength(128)

  await input.press('b')
  await expect.poll(async () => (await writes()).slice(-2)).toEqual(['\x1b[98;;98u', '\x1b[98;1:3u'])
})

test('pairs terminal mouse presses and releases when the input queue fills', async ({ page }) => {
  const loadTerminal = async () => {
    await page.goto('/?mouse-terminal&blocked-terminal-write')
    await expect(page.getByText('BB', { exact: true })).toBeAttached()
    await page.locator('.terminal-view__canvas').hover({ position: { x: 2, y: 2 } })
  }
  const input = page.getByRole('textbox', { name: 'Terminal', exact: true })
  const writes = () => page.evaluate(() => window.__writes)
  const resumeWrites = () => page.evaluate(() => window.__releaseTerminalWrite())
  const fillQueue = () => input.evaluate((element) => {
    for (let index = 0; index < 130; index++) {
      element.dispatchEvent(new KeyboardEvent('keydown', { key: 'a', bubbles: true, cancelable: true }))
    }
  })

  await loadTerminal()
  await page.mouse.down()
  await fillQueue()
  await expect(page.getByText('Terminal input is full.', { exact: false })).toBeVisible()
  await expect.poll(writes).toEqual(['\x1b[<0;1;1M'])
  await page.mouse.up()
  await resumeWrites()
  await expect.poll(async () => (await writes()).length).toBe(128)
  expect((await writes()).at(-1)).toBe('\x1b[<0;1;1m')

  await loadTerminal()
  await fillQueue()
  await expect.poll(writes).toEqual(['a'])
  await page.mouse.down()
  await resumeWrites()
  await expect.poll(async () => (await writes()).length).toBe(128)
  await page.mouse.up()
  await input.press('b')
  await expect.poll(async () => (await writes()).slice(128)).toEqual(['b'])
})

test('reports horizontal terminal wheel movement alongside vertical steps', async ({ page }) => {
  await page.goto('/?mouse-terminal')
  await expect(page.getByLabel('Terminal output')).toHaveText('BB')
  const canvas = page.locator('.terminal-view__canvas')
  const writes = () => page.evaluate(() => window.__writes)
  const wheel = (deltaX: number, deltaY = 0, shiftKey = false) => canvas.evaluate((element, options) => {
    const { left, top } = element.getBoundingClientRect()
    return element.dispatchEvent(new WheelEvent('wheel', {
      ...options, deltaMode: WheelEvent.DOM_DELTA_LINE, clientX: left + 2, clientY: top + 2,
      bubbles: true, cancelable: true,
    }))
  }, { deltaX, deltaY, shiftKey })

  expect(await wheel(-0.5)).toBe(false)
  expect(await writes()).toEqual([])
  await wheel(-0.5)
  await wheel(2, -1)
  await expect.poll(writes).toEqual(['\x1b[<66;1;1M', '\x1b[<67;1;1M\x1b[<67;1;1M', '\x1b[<64;1;1M'])
  expect(await wheel(1, 0, true)).toBe(true)
  expect(await writes()).toHaveLength(3)

  await page.goto('/')
  await expect(page.getByLabel('Terminal output')).toHaveText('BB')
  expect(await wheel(1)).toBe(true)
  expect(await writes()).toEqual([])
})

test('releases a held terminal mouse button when focus leaves', async ({ page }) => {
  await page.goto('/?mouse-terminal')
  await expect(page.getByText('BB', { exact: true })).toBeAttached()
  const input = page.getByRole('textbox', { name: 'Terminal', exact: true })
  const writes = () => page.evaluate(() => window.__writes)
  const reports = ['\x1b[<0;1;1M', '\x1b[<32;2;1M', '\x1b[<0;2;1m']
  const bounds = await page.locator('.terminal-view__canvas').boundingBox()
  if (!bounds) throw new Error('The terminal canvas has no bounds')

  await page.mouse.move(bounds.x + 2, bounds.y + 2)
  await page.mouse.down()
  await page.mouse.move(bounds.x + bounds.width * 0.75, bounds.y + 2)
  await expect.poll(writes).toEqual(reports.slice(0, 2))
  await expect(input).toBeFocused()
  await input.evaluate((element) => (element as HTMLTextAreaElement).blur())
  await expect.poll(writes).toEqual(reports)

  await page.mouse.up()
  await input.press('b')
  await expect.poll(writes).toEqual([...reports, 'b'])
})

test('paints the latest terminal output when a hidden window returns', async ({ page }) => {
  await page.goto('/?scrollback-selection')
  const output = page.getByLabel('Terminal output')
  await expect(output).toHaveText('BB')

  const paints = await page.evaluate(async () => {
    const fixture = window as unknown as { __emitTerminalHistory: (offset: number, glyph: string) => void }
    let hidden = false
    Object.defineProperty(document, 'hidden', { configurable: true, get: () => hidden })
    const glyphs: string[] = []
    const originalFillText = CanvasRenderingContext2D.prototype.fillText
    CanvasRenderingContext2D.prototype.fillText = function (...args) {
      if (this.canvas.classList.contains('terminal-view__canvas')) glyphs.push(args[0])
      originalFillText.apply(this, args)
    }
    const waitForPaint = () => new Promise<void>((resolve) => {
      requestAnimationFrame(() => requestAnimationFrame(() => resolve()))
    })

    try {
      fixture.__emitTerminalHistory(2, 'C')
      hidden = true
      document.dispatchEvent(new Event('visibilitychange'))
      fixture.__emitTerminalHistory(3, 'D')
      await waitForPaint()
      const whileHidden = glyphs.splice(0)

      hidden = false
      document.dispatchEvent(new Event('visibilitychange'))
      await waitForPaint()
      return { whileHidden, restored: glyphs }
    } finally {
      CanvasRenderingContext2D.prototype.fillText = originalFillText
      Reflect.deleteProperty(document, 'hidden')
    }
  })

  expect(paints).toEqual({ whileHidden: [], restored: ['D', 'D'] })
  await expect(output).toHaveText('DD')
})

test('reconnects an expired frame subscription without restarting the shell', async ({ page }) => {
  await page.goto('/?scrollback-selection&expired-terminal-subscription')
  const output = page.getByLabel('Terminal output')
  await expect(output).toHaveText('BB')
  const emit = (offset: number, glyph: string) => page.evaluate(({ offset, glyph }) => {
    const fixture = window as unknown as { __emitTerminalHistory: (offset: number, glyph: string) => void }
    fixture.__emitTerminalHistory(offset, glyph)
  }, { offset, glyph })
  await emit(2, 'C')

  await expect(output).toHaveText('DD')
  await emit(3, 'E')
  await expect(output).toHaveText('EE')
  await expect(page.getByText('terminal frame subscription ended')).toHaveCount(0)
  const subscriptions = await page.evaluate(() => window.__subscriptions)
  expect(subscriptions).toHaveLength(2)
  expect(subscriptions[0]).toBe(subscriptions[1])
})

test('keeps a selection while new output leaves the selected scrollback unchanged', async ({ page }) => {
  await page.goto('/?scrollback-selection')
  const output = page.getByLabel('Terminal output')
  const input = page.getByRole('textbox', { name: 'Terminal', exact: true })
  const emit = (offset: number, glyph: string) => page.evaluate(({ offset, glyph }) => {
    const fixture = window as unknown as { __emitTerminalHistory: (offset: number, glyph: string) => void }
    fixture.__emitTerminalHistory(offset, glyph)
  }, { offset, glyph })
  const copy = () => input.evaluate((element) => {
    const clipboard = new DataTransfer()
    element.dispatchEvent(new ClipboardEvent('copy', { bubbles: true, cancelable: true, clipboardData: clipboard }))
    return clipboard.getData('text/plain')
  })

  await expect(output).toHaveText('BB')
  await emit(4, 'A')
  await expect(output).toHaveText('AA')
  const canvas = page.locator('.terminal-view__canvas')
  const bounds = await canvas.boundingBox()
  if (!bounds) throw new Error('The terminal canvas has no bounds')
  await canvas.dispatchEvent('dblclick', { clientX: bounds.x + 2, clientY: bounds.y + 2 })
  expect(await copy()).toBe('AA')

  await emit(5, 'A')
  expect(await copy()).toBe('AA')

  await emit(6, 'C')
  await expect(output).toHaveText('CC')
  expect(await copy()).toBe('')
})

test('double-click selects a word across soft wraps and stops at a hard break', async ({ page }) => {
  await page.goto('/?wrapped-word-selection')
  await expect(page.getByLabel('Terminal output')).toHaveText(/^a界bc\nde$/)
  const canvas = page.locator('.terminal-view__canvas')
  const input = page.getByRole('textbox', { name: 'Terminal', exact: true })
  const bounds = await canvas.boundingBox()
  if (!bounds) throw new Error('The terminal canvas has no bounds')
  const copyWord = async (row: number, column: number) => {
    await canvas.dblclick({ position: { x: (column + 0.5) * bounds.width / 2, y: (row + 0.5) * bounds.height / 4 } })
    return input.evaluate((element) => {
      const clipboard = new DataTransfer()
      element.dispatchEvent(new ClipboardEvent('copy', { bubbles: true, cancelable: true, clipboardData: clipboard }))
      return clipboard.getData('text/plain')
    })
  }

  expect(await copyWord(0, 0)).toBe('a界bc')
  expect(await copyWord(0, 1)).toBe('a界bc')
  expect(await copyWord(1, 1)).toBe('a界bc')
  expect(await copyWord(2, 0)).toBe('a界bc')
  expect(await copyWord(3, 0)).toBe('de')
})

for (const mac of process.platform === 'darwin' ? [true, false] : [false]) {
  const shortcut = mac ? 'Meta+c' : 'Control+Shift+c'
  test(`copies the selected terminal text with ${shortcut}`, async ({ page }) => {
    if (!mac) {
      await page.addInitScript(() => Object.defineProperty(navigator, 'platform', { value: 'Linux x86_64' }))
    }
    await page.goto('/')
    await expect(page.getByLabel('Terminal output')).toHaveText('BB')
    const input = page.getByRole('textbox', { name: 'Terminal', exact: true })
    await page.locator('.terminal-view__canvas').dblclick({ position: { x: 2, y: 2 } })
    await expect(input).toBeFocused()

    await page.evaluate((mac) => {
      const probe = { text: '', target: '' }
      Object.assign(window, { __terminalCopy: probe })
      if (mac) {
        document.addEventListener('copy', (event) => {
          if (!event.isTrusted) return
          probe.target = (event.target as Element).tagName
          const clipboard = new DataTransfer()
          Object.defineProperty(event, 'clipboardData', { value: clipboard })
          window.setTimeout(() => { probe.text = clipboard.getData('text/plain') }, 0)
        }, { capture: true })
      } else {
        Object.defineProperty(navigator.clipboard, 'writeText', {
          value: async (text: string) => { probe.text = text },
        })
      }
    }, mac)
    await input.press(shortcut)
    await expect.poll(() => page.evaluate(() =>
      (window as unknown as { __terminalCopy: { text: string; target: string } }).__terminalCopy,
    )).toEqual({ text: 'BB', target: mac ? 'TEXTAREA' : '' })
    expect(await page.evaluate(() => window.__writes)).toEqual([])

    await input.press('Control+c')
    await expect.poll(() => page.evaluate(() => window.__writes)).toEqual(['\x03'])
  })
}

test('ends a partial bracketed paste before sending the next key', async ({ page }) => {
  await page.goto('/?interrupted-terminal-paste')
  const input = page.getByRole('textbox', { name: 'Terminal', exact: true })
  await expect(page.getByText('BB', { exact: true })).toBeAttached()

  await input.evaluate((element) => {
    const clipboard = new DataTransfer()
    clipboard.setData('text/plain', 'a'.repeat(32_768))
    element.dispatchEvent(new ClipboardEvent('paste', { bubbles: true, cancelable: true, clipboardData: clipboard }))
  })
  await expect(page.getByText('The terminal stopped accepting the paste.')).toBeVisible()
  await input.press('b')
  const writes = () => page.evaluate(() => (window as unknown as { __writes: (string | number[])[] }).__writes)
  await expect.poll(async () => (await writes()).slice(2)).toEqual(['\x1b[201~', 'b'])
  expect((await writes())[0].slice(0, 6)).toEqual([27, 91, 50, 48, 48, 126])
})

test('opens Freebuff as a standalone terminal in the conversation pane', async ({ page }) => {
  await page.goto('/')
  const chat = page.getByRole('region', { name: 'Task chat' })
  await chat.getByLabel('Prompt').fill('Keep this draft')
  await chat.getByRole('button', { name: 'Agent: Codex' }).click()
  await chat.getByRole('menuitemradio', { name: 'Freebuff (terminal)' }).click()

  const freebuff = page.getByRole('region', { name: 'Freebuff terminal' })
  await expect(chat).toBeHidden()
  await expect(freebuff.getByRole('heading')).toHaveCount(0)
  const input = freebuff.getByRole('textbox', { name: 'Terminal', exact: true })
  await expect(input).toBeFocused()
  await expect(freebuff.getByLabel('Terminal output')).toContainText('B')
  await input.press('a')
  await expect.poll(() => page.evaluate(() =>
    (window as unknown as { __writes: string[] }).__writes,
  )).toEqual(['a'])
  await page.getByRole('banner', { name: 'Workspace header' }).getByRole('button', { name: 'Back to chat' }).click()

  await expect(freebuff).toBeHidden()
  await expect(chat.getByLabel('Prompt')).toHaveValue('Keep this draft')
  await expect.poll(() => page.evaluate(() =>
    (window as unknown as { __closedTerminals: string[] }).__closedTerminals,
  )).toEqual(['freebuff-fixture'])
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
  const compose = page.getByRole('button', { name: 'New chat in demo' })
  await expect(compose).toHaveCSS('opacity', '0')
  await repository.hover()
  await expect(compose).toHaveCSS('opacity', '1')
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
  await expect(compose).toHaveCSS('opacity', '0')
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

  await directory.hover()
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

test('shows the agent exit and resume action, including after reopening', async ({ page }) => {
  await page.goto('/?agent-failed')
  await expect(page.getByText('Agent exited mid-turn', { exact: true })).toBeVisible()
  await expect(page.getByText('Agent stderr fixture')).toBeVisible()
  await expect(page.getByRole('button', { name: 'Resume', exact: true })).toBeVisible()

  await page.goto('/?agent-failed&reopened')
  await expect(page.getByRole('alert')).toContainText('The agent exited with code 23')
  await expect(page.getByRole('button', { name: 'Resume', exact: true })).toBeVisible()
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

for (const restart of [false, true]) {
  test(`keeps an immediate terminal exit after ${restart ? 'restart' : 'start'}`, async ({ page }) => {
    await page.goto('/?early-terminal-exit')
    if (restart) {
      await page.getByRole('button', { name: /Terminal 2.*ended/ }).click()
      await page.getByRole('button', { name: 'Restart' }).click()
    } else {
      await page.getByRole('button', { name: 'New shared terminal' }).click()
    }
    await page.waitForFunction(() => typeof window.__finishTerminalLaunch === 'function')
    await page.evaluate(() => window.__finishTerminalLaunch())
    const pane = page.getByRole('region', { name: 'Short-lived shell' })
    await expect(pane).toContainText('ended with exit code 23')
    await expect(pane.getByRole('button', { name: 'Restart' })).toBeEnabled()
    await page.evaluate(() => window.__replayTerminalList())
    await expect(pane).toContainText('ended with exit code 23')
    await expect(pane.getByRole('textbox')).toHaveCount(0)
  })
}

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

test('streams read-only Codex command output beside a live shell', async ({ page }) => {
  await page.goto('/?scoped-terminals')
  const shared = page.getByRole('region', { name: 'Terminal 1' }).getByRole('textbox')
  const commands = page.getByRole('region', { name: 'Codex command terminals' })
  const output = commands.getByLabel('Command output')
  await shared.focus()

  const command = (sequence: number, title: string, status: 'running' | 'completed', output: string | null): Entry => ({
    id: `command-${sequence}`,
    sequence,
    kind: {
      type: 'toolCall',
      data: { toolCallId: `item-${sequence}`, title, kind: 'command', status, output, diffs: [], terminalId: null },
    },
  })
  const publish = (entries: Entry[]) => page.evaluate((rows) => {
    const fixture = window as unknown as { __setCommandEntries: (taskId: string, entries: Entry[]) => void }
    fixture.__setCommandEntries('task-1', rows)
  }, entries)

  await publish([command(4, 'cargo test', 'running', null)])
  await expect(commands.getByRole('button', { name: 'cargo test running' })).toBeVisible()
  await expect(output).toHaveText('No output yet.')
  await publish([command(4, 'cargo test', 'running', 'running tests\n')])
  await expect(output).toContainText('running tests')
  await publish([command(4, 'cargo test', 'running', 'running tests\nwarning: unused import\n')])
  await expect(output).toContainText('warning: unused import')
  await expect(shared).toBeFocused()
  await expect(commands.getByRole('textbox')).toHaveCount(0)

  const completed = command(4, 'cargo test', 'completed', 'running tests\nwarning: unused import\nok\n')
  await publish([completed, command(5, 'git status', 'running', 'On branch main')])
  await expect(output).toHaveText('On branch main')
  await commands.getByRole('button', { name: 'cargo test completed' }).click()
  await expect(output).toHaveText('running tests warning: unused import ok')
  await publish([completed, command(5, 'git status', 'completed', 'On branch main\nclean')])
  await expect(output).toContainText('unused import')

  await page.getByRole('button', { name: /Other task/ }).click()
  await expect(commands).toHaveCount(0)
  await expect(shared).toBeVisible()
  await page.getByRole('button', { name: /Ship it/ }).click()
  await expect(output).toHaveText('On branch main clean')
})

test('shows an interactive ACP terminal only for its owning task', async ({ page }) => {
  await page.goto('/?scoped-terminals')
  const shared = page.getByRole('region', { name: 'Terminal 1', exact: true })
  await expect(shared).toBeVisible()
  const terminal: AgentTerminal = { id: 'agent-fixture', title: 'Run tests', live: true, exitCode: null }
  const publish = (terminals: AgentTerminal[]) => page.evaluate((rows) => {
    const fixture = window as unknown as { __setAgentTerminals: (taskId: string, terminals: AgentTerminal[]) => void }
    fixture.__setAgentTerminals('task-1', rows)
  }, terminals)
  await publish([terminal])
  const pane = page.getByRole('region', { name: 'agent: Run tests', exact: true })
  await expect(pane.getByLabel('Terminal output')).toHaveText('BB')
  const input = pane.getByRole('textbox', { name: 'Terminal', exact: true })
  await input.press('x')
  await expect.poll(() => page.evaluate(() => window.__writes)).toContain('x')

  await pane.getByRole('button', { name: 'Hide pane' }).click()
  await expect(pane).toHaveCount(0)
  await page.getByRole('button', { name: /agent: Run tests.*hidden/ }).click()
  await expect(input).toBeFocused()

  await page.getByRole('button', { name: /Other task/ }).click()
  await expect(pane).toHaveCount(0)
  await expect(shared).toBeVisible()
  await page.getByRole('button', { name: /Ship it/ }).click()
  await pane.getByRole('button', { name: 'Stop process' }).click()
  await expect.poll(() => page.evaluate(() => window.__closedTerminals)).toContain(terminal.id)
  await publish([{ ...terminal, live: false, exitCode: 7 }])
  await expect(pane).toContainText('Process ended with exit code 7')
  await publish([])
  await expect(pane).toHaveCount(0)
})

test('forwards browser IME commits once and discards cancelled preedit', async ({ page }) => {
  await page.goto('/')
  await expect(page.getByLabel('Terminal output')).toHaveText('BB')
  const input = page.getByRole('textbox', { name: 'Terminal', exact: true })
  await input.focus()
  const browser = await page.context().newCDPSession(page)
  const compose = (text: string) => browser.send('Input.imeSetComposition', {
    text, selectionStart: text.length, selectionEnd: text.length,
  })
  const writes = () => page.evaluate(() => window.__writes)
  const trustedUpdates = await input.evaluateHandle((element) => {
    const updates: string[] = []
    element.addEventListener('compositionupdate', (event) => {
      if (event.isTrusted) updates.push((event as CompositionEvent).data)
    })
    return updates
  })

  await compose('ni')
  await expect(input).toHaveValue('ni')
  await compose('拟')
  await expect(input).toHaveValue('拟')
  expect(await trustedUpdates.jsonValue()).toEqual(['ni', '拟'])
  expect(await writes()).toEqual([])
  await browser.send('Input.insertText', { text: '你' })
  await expect.poll(writes).toEqual(['你'])
  await expect(input).toHaveValue('')

  await compose('hao')
  await expect(input).toHaveValue('hao')
  await compose('')
  await expect(input).toHaveValue('')
  await input.press('a')
  await expect.poll(writes).toEqual(['你', 'a'])
})

test('keeps terminal find open when Escape belongs to an IME composition', async ({ page }) => {
  await page.goto('/')
  const terminal = page.getByRole('textbox', { name: 'Terminal', exact: true })
  const modifier = await page.evaluate(() => navigator.platform.startsWith('Mac') ? 'Meta' : 'Control+Shift')
  await terminal.press(`${modifier}+f`)
  const search = page.getByRole('searchbox', { name: 'Find in terminal' })
  await expect(search).toBeFocused()
  await search.fill('unmatched')
  await search.press('Enter')
  await expect(page.getByText('No matches', { exact: true })).toBeVisible()

  await search.dispatchEvent('keydown', { key: 'Escape', isComposing: true, cancelable: true, bubbles: true })
  await expect(search).toBeFocused()
  await search.press('Escape')
  await expect(search).toHaveCount(0)
  await expect(terminal).toBeFocused()
})

test('clears a terminal bell when its focused window returns', async ({ page }) => {
  await page.goto('/?scoped-terminals')
  const input = page.getByRole('region', { name: 'Task shell' }).getByRole('textbox')
  const attention = page.locator('#inspector-tab-terminals').getByRole('img', { name: 'Terminal needs attention' })
  await input.focus()

  await page.evaluate(() => {
    Object.defineProperty(document, 'hasFocus', { configurable: true, value: () => false })
    window.dispatchEvent(new FocusEvent('blur'))
    const fixture = window as unknown as { __emitCoreEvent: (event: ShepherdEvent) => void }
    fixture.__emitCoreEvent({
      type: 'terminalBell',
      data: { projectId: 'project-1', taskId: 'task-1', terminalId: 'terminal-task' },
    })
  })
  await expect(attention).toBeVisible()
  await expect(input).toBeFocused()

  await page.evaluate(() => {
    Object.defineProperty(document, 'hasFocus', { configurable: true, value: () => true })
    window.dispatchEvent(new FocusEvent('focus'))
  })
  await expect(attention).toHaveCount(0)
  await expect(input).toBeFocused()
})

test('preserves the other terminal when a saved pane is missing', async ({ page }) => {
  await page.goto('/?missing-terminal')

  const live = page.getByRole('region', { name: 'Terminal 1' })
  const unavailable = page.getByRole('region', { name: 'Unavailable terminal' })
  await expect(live.getByRole('textbox')).toBeVisible()
  await expect(unavailable).toContainText('The saved terminal no longer exists.')
  await expect(page.getByRole('separator', { name: 'Resize terminal split' })).toHaveAttribute('aria-valuenow', '65')
  await unavailable.getByRole('button', { name: 'Hide pane' }).click()
  await expect(unavailable).toHaveCount(0)
  await expect(live.getByRole('textbox')).toBeVisible()
  expect(await page.evaluate(() =>
    (window as unknown as { __terminalLayout: () => TerminalLayout }).__terminalLayout().root,
  )).toEqual({ type: 'pane', terminalId: 'terminal-1' })
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
  const sharedInput = page.getByRole('region', { name: 'Terminal 1' }).getByRole('textbox')
  const scopedInput = scoped.getByRole('textbox')
  const terminalsTab = page.getByRole('tab', { name: 'Terminals' })
  const reviewTab = page.getByRole('tab', { name: 'Review', exact: true })
  const modifier = await page.evaluate(() => navigator.platform.startsWith('Mac') ? 'Meta' : 'Control')
  await sharedInput.press(`${modifier}+Backquote`)
  await expect(scopedInput).toBeFocused()
  await scopedInput.press(`${modifier}+Backquote`)
  await expect(terminalsTab).toBeFocused()
  await terminalsTab.press(`${modifier}+Shift+Backquote`)
  await expect(scopedInput).toBeFocused()
  await scopedInput.press(`${modifier}+Backquote`)
  await terminalsTab.press(`${modifier}+Backquote`)
  await expect(sharedInput).toBeFocused()
  expect(await page.evaluate(() => (window as unknown as { __writes: string[] }).__writes)).toEqual([])

  await terminalsTab.press('ArrowRight')
  await expect(reviewTab).toBeFocused()
  await expect(reviewTab).toHaveAttribute('aria-selected', 'true')
  await expect(terminalsTab).toHaveAttribute('tabindex', '-1')
  await reviewTab.press('Home')
  await expect(terminalsTab).toBeFocused()
  await expect(terminalsTab).toHaveAttribute('aria-selected', 'true')

  await scoped.getByRole('button', { name: 'Hide pane' }).click()
  await expect(scoped).toHaveCount(0)
  await page.getByRole('button', { name: /Task shell.*hidden/ }).click()
  await expect(scoped.getByRole('textbox')).toBeFocused()
})

test('resizes the inspector and restores the saved width for the selected task', async ({ page }) => {
  await page.goto('/?scoped-terminals')
  const divider = page.getByRole('separator', { name: 'Resize inspector' })
  const width = async () => Number(await divider.getAttribute('aria-valuenow'))
  const inspectorHeader = page.locator('.shell__header-inspector')
  const inspector = page.getByRole('complementary', { name: 'Inspector', exact: true })
  const columnBounds = async () => {
    const header = await inspectorHeader.boundingBox()
    const pane = await inspector.boundingBox()
    return header && pane ? [header.x - pane.x, header.width - pane.width] : null
  }
  await expect(inspectorHeader.getByRole('tab')).toHaveCount(5)
  await expect.poll(width).toBe(360)
  await expect.poll(columnBounds).toEqual([0, 0])
  await divider.press('ArrowLeft')
  await expect.poll(width).toBe(392)
  const handle = await divider.boundingBox()
  if (!handle) throw new Error('Inspector divider has no bounds')
  const x = handle.x + handle.width / 2
  const y = handle.y + handle.height / 2
  await page.mouse.move(x, y)
  await page.mouse.down()
  await page.mouse.move(x - 64, y, { steps: 4 })
  await page.mouse.up()
  await expect.poll(width).toBe(456)
  await expect.poll(columnBounds).toEqual([0, 0])
  await page.getByRole('button', { name: /Other task/ }).click()
  await expect.poll(width).toBe(360)
  await page.getByRole('button', { name: /Ship it/ }).click()
  await expect.poll(width).toBe(456)
})

test('saves Quad and Single layouts while keeping the terminal instances', async ({ page }) => {
  await page.goto('/?terminal-presets')
  const readLayout = () => page.evaluate(() =>
    (window as unknown as { __terminalLayout: () => TerminalLayout }).__terminalLayout().root)
  const leaf = (number: number | 'later') => ({ type: 'pane', terminalId: `terminal-${number}` })
  const split = (axis: 'vertical' | 'horizontal', first: unknown, second: unknown) =>
    ({ type: 'split', axis, ratio: 500, first, second })

  await page.getByRole('button', { name: 'Quad', exact: true }).click()
  await expect.poll(readLayout).toEqual(split('horizontal', split('vertical',
    split('horizontal', leaf(1), leaf(2)),
    split('horizontal', leaf(3), leaf(4))), leaf('later')))
  await page.getByRole('button', { name: /Terminal 3.*live/ }).click()
  await page.getByRole('button', { name: 'Single', exact: true }).click()
  await expect.poll(readLayout).toEqual(split('horizontal', leaf(3), leaf('later')))
  await expect(page.getByRole('region', { name: 'Terminal 3' }).getByRole('textbox')).toBeFocused()
  await expect(page.getByRole('button', { name: /Terminal 1.*live.*hidden/ })).toBeVisible()
  await page.getByRole('button', { name: /Other task/ }).click()
  await expect(page.getByRole('region', { name: 'Other shell' }).getByRole('textbox')).toBeVisible()
})

test('moves focus by direction between the Terminals tab and live splits', async ({ page }) => {
  await page.goto('/?terminal-presets')
  await page.getByRole('button', { name: 'Quad', exact: true }).click()

  const pane = (number: number) => page.getByRole('region', { name: `Terminal ${number}` })
  const header = (number: number) => pane(number).getByRole('button', { name: 'Hide pane' })
  const tab = page.getByRole('tab', { name: 'Terminals' })
  await tab.press('ArrowDown')
  await expect(header(1)).toBeFocused()
  await page.keyboard.press('ArrowRight')
  await expect(header(1)).toBeFocused()
  await page.keyboard.press('ArrowDown')
  await expect(header(3)).toBeFocused()
  await page.keyboard.press('ArrowRight')
  await expect(header(4)).toBeFocused()
  await page.keyboard.press('ArrowLeft')
  await expect(header(3)).toBeFocused()
  await page.keyboard.press('ArrowUp')
  await expect(header(1)).toBeFocused()
  await page.keyboard.press('ArrowUp')
  await expect(tab).toBeFocused()

  const input = pane(1).getByRole('textbox')
  await input.press('ArrowDown')
  await expect(input).toBeFocused()
  await expect.poll(() => page.evaluate(() => window.__writes)).toEqual(['\x1bOB'])
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

test('reports a Kitty release after its shortcut modifier is released', async ({ page }) => {
  await page.goto('/?kitty-shortcuts')
  const input = page.getByRole('textbox', { name: 'Terminal', exact: true })
  await expect(page.getByText('BB', { exact: true })).toBeAttached()
  await input.focus()

  await page.keyboard.down('Control')
  await page.keyboard.down('a')
  await page.keyboard.up('Control')
  await page.keyboard.up('a')
  await input.press('Control+Shift+Tab')

  await expect.poll(() => page.evaluate(() => window.__writes))
    .toEqual(['\x1b[97;5u', '\x1b[97;1:3u', '\x1b[9;6u', '\x1b[9;6:3u'])
})

test('releases reported Kitty keys when focus leaves a terminal', async ({ page }) => {
  await page.goto('/?kitty-terminal')
  const input = page.getByRole('textbox', { name: 'Terminal', exact: true })
  const writes = () => page.evaluate(() => window.__writes)
  const firstKeys = ['\x1b[97;;97u', '\x1b[98;;98u', '\x1b[97;1:3u', '\x1b[98;1:3u']
  await expect(page.getByText('BB', { exact: true })).toBeAttached()
  await input.focus()

  await page.keyboard.down('a')
  await page.keyboard.down('b')
  await input.evaluate((element) => (element as HTMLTextAreaElement).blur())
  await expect.poll(writes).toEqual(firstKeys)

  await input.focus()
  await page.keyboard.up('a')
  await page.keyboard.up('b')
  await page.keyboard.press('c')
  await expect.poll(writes).toEqual([...firstKeys, '\x1b[99;;99u', '\x1b[99;1:3u'])
})
