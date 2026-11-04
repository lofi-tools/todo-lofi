/* @refresh reload */
import { render } from 'solid-js/web'

import { App } from './App'
import './styles.css'

const root = document.getElementById('root')
if (!root) {
  throw new Error('the Shepherd UI is missing its #root element')
}

render(() => <App />, root)
