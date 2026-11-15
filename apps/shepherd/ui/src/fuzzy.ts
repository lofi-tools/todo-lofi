export function fuzzyScore(query: string, text: string): number | null {
  const needle = query.toLowerCase()
  const haystack = text.toLowerCase()
  let score = 0
  let cursor = 0
  let previous = -2
  for (const character of needle) {
    const found = haystack.indexOf(character, cursor)
    if (found < 0) return null
    score += found === previous + 1 ? 2 : 1
    if (found === 0 || /[\s/_\-.:]/.test(haystack.charAt(found - 1))) score += 3
    previous = found
    cursor = found + 1
  }
  return score
}

