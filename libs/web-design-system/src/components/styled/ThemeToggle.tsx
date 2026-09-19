import type { JSX } from 'solid-js'
import { createSignal, onMount } from 'solid-js'
import { Moon, Sun } from '../../icons'
import { IconButton } from './IconButton'

type Theme = 'dark' | 'light'

const STORAGE_KEY = 'wds-theme'

/**
 * Flips `data-theme` on the document element and remembers the choice.
 *
 * The attribute is set before paint by an inline script in the host page (see
 * the showcase layout), so this component only reads the current value and
 * switches it — it never causes a flash of the wrong theme.
 */
export function ThemeToggle(): JSX.Element {
  const [theme, setTheme] = createSignal<Theme>('dark')

  onMount(() => {
    const stored = window.localStorage.getItem(STORAGE_KEY)
    const initial: Theme =
      stored === 'light' || stored === 'dark'
        ? stored
        : document.documentElement.dataset.theme === 'light'
          ? 'light'
          : 'dark'
    setTheme(initial)
  })

  const toggle = () => {
    const next: Theme = theme() === 'dark' ? 'light' : 'dark'
    setTheme(next)
    document.documentElement.dataset.theme = next
    window.localStorage.setItem(STORAGE_KEY, next)
  }

  return (
    <IconButton
      aria-label={theme() === 'dark' ? 'Switch to light theme' : 'Switch to dark theme'}
      title="Toggle theme"
      onClick={toggle}
    >
      {theme() === 'dark' ? <Sun size={15} /> : <Moon size={15} />}
    </IconButton>
  )
}

export default ThemeToggle
