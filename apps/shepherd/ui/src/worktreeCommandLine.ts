const controlEscapes = { n: '\n', r: '\r', t: '\t', "'": "'", '\\': '\\' } as const
const controlCharacters = { '\n': 'n', '\r': 'r', '\t': 't', "'": "'", '\\': '\\' } as const

export function formatWorktreeCommand(command: string[]): string {
  return command.map((argument) => {
    if (/^[a-z0-9_@%+=:,./-]+$/i.test(argument)) return argument
    if (/[\n\r\t]/.test(argument)) {
      return "$'" + argument.replace(/[\n\r\t'\\]/g, (character) =>
        '\\' + controlCharacters[character as keyof typeof controlCharacters]) + "'"
    }
    return "'" + argument.replaceAll("'", "'\\''") + "'"
  }).join(' ')
}

export function parseWorktreeCommand(line: string): string[] {
  const part = /\s+|\$'((?:\\[nrt'\\]|[^'\\])*)'|'([^']*)'|"((?:\\.|[^"\\])*)"|\\(.)|[|&;<>()]|(?:[^\s'"\\$|&;<>()]|\$(?!'))+/y
  const command: string[] = []
  let word: string | undefined
  while (part.lastIndex < line.length) {
    const match = part.exec(line)
    if (!match) throw new Error('Close any open quotes and check the backslash escapes.')
    const [token, escaped, literal, doubleQuoted, escapedCharacter] = match
    if (!token.trim()) {
      if (word !== undefined) command.push(word)
      word = undefined
      continue
    }
    if (token.length === 1 && '|&;<>()'.includes(token)) {
      throw new Error("Use sh -c '…' for pipes, redirects, or other shell operators.")
    }
    const value = escaped !== undefined
      ? escaped.replace(/\\(.)/g, (_, character: keyof typeof controlEscapes) => controlEscapes[character])
      : literal ?? doubleQuoted?.replace(/\\([$"`\\])/g, '$1') ?? escapedCharacter ?? token
    word = (word ?? '') + value
  }
  if (word !== undefined) command.push(word)
  return command
}
