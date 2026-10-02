import { render } from 'solid-js/web'

// Panda's generated styles first: it declares the cascade layers the design
// system's reset and base rules are written into.
import 'styled-system/styles.css'
import 'web-design-system/styles.css'
import './styles/app.css'

import App from './App'

const root = document.getElementById('root')
if (!root) throw new Error('Missing #root')

render(() => <App />, root)
