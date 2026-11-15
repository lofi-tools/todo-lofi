import { For, type JSX } from 'solid-js'

type Inline =
  | { type: 'text'; value: string }
  | { type: 'code'; value: string }
  | { type: 'strong'; children: Inline[] }
  | { type: 'em'; children: Inline[] }
  | { type: 'strike'; children: Inline[] }
  | { type: 'link'; href: string; children: Inline[] }

type Block =
  | { type: 'code'; value: string }
  | { type: 'heading'; value: string; depth: number }
  | { type: 'paragraph'; value: string }
  | { type: 'list'; ordered: boolean; items: string[] }
  | { type: 'quote'; value: string }
  | { type: 'table'; headers: string[]; alignments: ('left' | 'center' | 'right')[]; rows: string[][] }
  | { type: 'rule' }

// Underscore emphasis is deliberately unsupported so `snake_case` names survive.
const INLINE_SOURCE = '`([^`]+)`|\\*\\*(?!\\s)([\\s\\S]+?)(?<!\\s)\\*\\*|~~(?!\\s)([\\s\\S]+?)(?<!\\s)~~|\\[([^\\]]*)\\]\\(([^)\\s]+)\\)|\\*(?!\\s)([^*\\n]+?)(?<!\\s)\\*'
const FENCE = /^\s*(```|~~~)/
const HEADING = /^(#{1,6})\s+(.*)$/
const RULE = /^\s*(?:-{3,}|\*{3,}|_{3,})\s*$/
const QUOTE = /^>\s?(.*)$/
const LIST = /^\s*(?:([-*+])|(\d{1,9})[.)])\s+(.*)$/
const FLAT_TABLE = /\|\s+\|\s*:?-{3,}/

function parseInline(source: string): Inline[] {
  const nodes: Inline[] = []
  const pattern = new RegExp(INLINE_SOURCE, 'g')
  let start = 0
  for (let match = pattern.exec(source); match; match = pattern.exec(source)) {
    if (match.index > start) nodes.push({ type: 'text', value: source.slice(start, match.index) })
    const [whole, code, strong, strike, label, href, emphasis] = match
    if (code !== undefined) nodes.push({ type: 'code', value: code })
    else if (strong !== undefined) nodes.push({ type: 'strong', children: parseInline(strong) })
    else if (strike !== undefined) nodes.push({ type: 'strike', children: parseInline(strike) })
    else if (href !== undefined) nodes.push({ type: 'link', href, children: parseInline(label ?? '') })
    else if (emphasis !== undefined) nodes.push({ type: 'em', children: parseInline(emphasis) })
    start = match.index + whole.length
  }
  if (start < source.length) nodes.push({ type: 'text', value: source.slice(start) })
  return nodes
}

function isBlockStart(line: string) {
  return FENCE.test(line) || HEADING.test(line) || RULE.test(line) || QUOTE.test(line) || LIST.test(line)
}

function tableCells(line: string): string[] {
  let text = line.trim()
  if (text.startsWith('|')) text = text.slice(1)
  if (text.endsWith('|') && !text.endsWith('\\|')) text = text.slice(0, -1)
  const cells: string[] = []
  let cell = ''
  let code = false
  for (let index = 0; index < text.length; index += 1) {
    const character = text[index]
    if (character === '\\' && text[index + 1] === '|') {
      cell += '|'
      index += 1
    } else if (character === '`') {
      code = !code
      cell += character
    } else if (character === '|' && !code) {
      cells.push(cell.trim())
      cell = ''
    } else cell += character
  }
  cells.push(cell.trim())
  return cells
}

function tableHeader(lines: string[], index: number) {
  if (!lines[index]?.includes('|') || !lines[index + 1]?.includes('|')) return undefined
  const headers = tableCells(lines[index])
  const separators = tableCells(lines[index + 1])
  if (headers.length !== separators.length || !separators.every((cell) => /^:?-{3,}:?$/.test(cell))) return undefined
  const alignments = separators.map((cell): 'left' | 'center' | 'right' =>
    cell.startsWith(':') && cell.endsWith(':') ? 'center' : cell.endsWith(':') ? 'right' : 'left')
  return { headers, alignments }
}

function flatTableRows(line: string): string[] | undefined {
  if (!line.trimStart().startsWith('|') || !FLAT_TABLE.test(line)) return undefined
  const rows = line.split(/(?<=\|)\s+(?=\|)/)
  return tableHeader(rows, 0) ? rows : undefined
}

function parseBlocks(source: string): Block[] {
  const lines = source.replace(/\r\n?/g, '\n').split('\n')
  const blocks: Block[] = []
  let index = 0
  while (index < lines.length) {
    let line = lines[index]
    if (!line.trim()) {
      index += 1
      continue
    }
    if (FENCE.test(line)) {
      const body: string[] = []
      index += 1
      while (index < lines.length && !FENCE.test(lines[index])) {
        body.push(lines[index])
        index += 1
      }
      if (index < lines.length) index += 1
      blocks.push({ type: 'code', value: body.join('\n') })
      continue
    }
    const rows = flatTableRows(line)
    if (rows) {
      lines.splice(index, 1, ...rows)
      line = lines[index]
    }
    const table = tableHeader(lines, index)
    if (table) {
      const rows: string[][] = []
      index += 2
      while (index < lines.length && lines[index].trim() && lines[index].includes('|')) {
        const cells = tableCells(lines[index])
        rows.push(table.headers.map((_, column) => cells[column] ?? ''))
        index += 1
      }
      blocks.push({ type: 'table', ...table, rows })
      continue
    }
    const heading = HEADING.exec(line)
    if (heading) {
      blocks.push({ type: 'heading', depth: heading[1].length, value: heading[2].trim() })
      index += 1
      continue
    }
    if (RULE.test(line)) {
      blocks.push({ type: 'rule' })
      index += 1
      continue
    }
    if (QUOTE.test(line)) {
      const body: string[] = []
      while (index < lines.length) {
        const quoted = QUOTE.exec(lines[index])
        if (!quoted) break
        body.push(quoted[1].trim())
        index += 1
      }
      blocks.push({ type: 'quote', value: body.join(' ').trim() })
      continue
    }
    const listed = LIST.exec(line)
    if (listed) {
      const ordered = listed[2] !== undefined
      const items: string[] = []
      while (index < lines.length) {
        const item = LIST.exec(lines[index])
        if (!item || (item[2] !== undefined) !== ordered) break
        items.push(item[3].trim())
        index += 1
      }
      blocks.push({ type: 'list', ordered, items })
      continue
    }
    const paragraph: string[] = []
    while (index < lines.length && lines[index].trim() && !isBlockStart(lines[index]) && !tableHeader(lines, index) && !flatTableRows(lines[index])) {
      paragraph.push(lines[index].trim())
      index += 1
    }
    blocks.push({ type: 'paragraph', value: paragraph.join(' ') })
  }
  return blocks
}

function safeHref(href: string) {
  return /^(?:https?:\/\/|mailto:|#|\/)/i.test(href)
}

function renderInline(nodes: Inline[], renderText: (value: string) => JSX.Element): JSX.Element[] {
  return nodes.map((node) => {
    switch (node.type) {
      case 'text': return renderText(node.value)
      case 'code': return <code class="md__inline-code">{node.value}</code>
      case 'strong': return <strong>{renderInline(node.children, renderText)}</strong>
      case 'em': return <em>{renderInline(node.children, renderText)}</em>
      case 'strike': return <del>{renderInline(node.children, renderText)}</del>
      case 'link': return safeHref(node.href)
        ? <a class="md__link" href={node.href} target="_blank" rel="noreferrer noopener">{renderInline(node.children, renderText)}</a>
        : <span>{renderInline(node.children, renderText)}</span>
    }
  })
}

export function Markdown(props: { source: string; renderText: (value: string) => JSX.Element }) {
  const blocks = () => parseBlocks(props.source)
  const inline = (value: string) => renderInline(parseInline(value), props.renderText)
  return (
    <For each={blocks()}>
      {(block) => {
        switch (block.type) {
          case 'code': return <pre class="md__code-block"><code>{block.value}</code></pre>
          case 'heading': return <p class={`md__heading md__heading-${block.depth}`}>{inline(block.value)}</p>
          case 'list': return block.ordered
            ? <ol class="md__list">{block.items.map((item) => <li>{inline(item)}</li>)}</ol>
            : <ul class="md__list">{block.items.map((item) => <li>{inline(item)}</li>)}</ul>
          case 'quote': return <blockquote class="md__quote">{inline(block.value)}</blockquote>
          case 'table': return <div class="md__table-scroll" tabIndex={0} role="region" aria-label="Table">
            <table class="md__table">
              <thead><tr>{block.headers.map((header, index) => <th scope="col" style={{ 'text-align': block.alignments[index] }}>{inline(header)}</th>)}</tr></thead>
              <tbody>{block.rows.map((row) => <tr>{row.map((cell, index) => <td style={{ 'text-align': block.alignments[index] }}>{inline(cell)}</td>)}</tr>)}</tbody>
            </table>
          </div>
          case 'rule': return <hr class="md__rule" />
          case 'paragraph': return <p class="md__paragraph">{inline(block.value)}</p>
        }
      }}
    </For>
  )
}
