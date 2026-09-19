// @vitest-environment jsdom
import { render } from '@solidjs/testing-library'
import { describe, expect, it } from 'vitest'
import { Button } from '../src/components/styled/Button'

describe('Button', () => {
  it('renders its label and recipe classes', () => {
    const { getByRole } = render(() => <Button variant="primary">Go</Button>)
    const button = getByRole('button') as HTMLButtonElement
    expect(button.textContent).toBe('Go')
    expect(button.className.length).toBeGreaterThan(0)
    expect(button.getAttribute('type')).toBe('button')
  })

  it('forwards the disabled state', () => {
    const { getByRole } = render(() => <Button disabled>Nope</Button>)
    expect((getByRole('button') as HTMLButtonElement).disabled).toBe(true)
  })

  it('exposes a variant class only for the requested variant', () => {
    const primary = render(() => <Button variant="primary">A</Button>)
    const ghost = render(() => <Button variant="ghost">B</Button>)
    const primaryClass = (primary.getByRole('button') as HTMLButtonElement).className
    const ghostClass = (ghost.getByRole('button') as HTMLButtonElement).className
    expect(primaryClass).not.toBe(ghostClass)
  })
})
