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
  | { type: 'rule' }

// Underscore emphasis is deliberately unsupported so `snake_case` names survive.
const INLINE_SOURCE = '`([^`]+)`|\\*\\*(?!\\s)([\\s\\S]+?)(?<!\\s)\\*\\*|~~(?!\\s)([\\s\\S]+?)(?<!\\s)~~|\\[([^\\]]*)\\]\\(([^)\\s]+)\\)|\\*(?!\\s)([^*\\n]+?)(?<!\\s)\\*'
const FENCE = /^\s*(```|~~~)/
const HEADING = /^(#{1,6})\s+(.*)$/
const RULE = /^\s*(?:-{3,}|\*{3,}|_{3,})\s*$/
const QUOTE = /^>\s?(.*)$/
const LIST = /^\s*(?:([-*+])|(\d{1,9})[.)])\s+(.*)$/

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

function parseBlocks(source: string): Block[] {
  const lines = source.replace(/\r\n?/g, '\n').split('\n')
  const blocks: Block[] = []
  let index = 0
  while (index < lines.length) {
    const line = lines[index]
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
    while (index < lines.length && lines[index].trim() && !isBlockStart(lines[index])) {
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
          case 'rule': return <hr class="md__rule" />
          case 'paragraph': return <p class="md__paragraph">{inline(block.value)}</p>
        }
      }}
    </For>
  )
}
