import React from 'react'
import ReactDOM from 'react-dom/client'
import App from './App'
import { installAuthFetch } from './lib/authFetch'
import './styles/main.css'

installAuthFetch()

ReactDOM.createRoot(document.getElementById('root')).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
)
