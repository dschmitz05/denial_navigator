import React from 'react'
import ReactDOM from 'react-dom/client'
import App from './App'
import { installAuthFetch } from './lib/authFetch'
import './styles/main.css'

installAuthFetch()

const root = document.getElementById('root')
if (!root) {
  throw new Error('The application root element is missing')
}

ReactDOM.createRoot(root).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
)
