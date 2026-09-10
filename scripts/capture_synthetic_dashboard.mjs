#!/usr/bin/env node
// Captures a Dashboard screenshot using a temporary Chromium profile. It is
// intentionally for local release evidence only; no credentials are persisted.
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { dirname, join } from 'node:path'
import { spawn } from 'node:child_process'

const appUrl = process.env.DEMO_APP_URL ?? 'https://127.0.0.1:3444/'
const apiUrl = process.env.API_BASE_URL ?? 'http://127.0.0.1:18000'
const username = process.env.E2E_USERNAME ?? 'admin'
const password = process.env.E2E_PASSWORD
const output = process.env.DEMO_SCREENSHOT ?? 'docs/demo/synthetic-dashboard.png'

if (!password) throw new Error('Set E2E_PASSWORD to the local test user password')

const delay = (milliseconds) => new Promise((resolve) => setTimeout(resolve, milliseconds))

async function eventually(get, label) {
  let error
  for (let attempt = 0; attempt < 40; attempt += 1) {
    try { return await get() } catch (caught) { error = caught; await delay(250) }
  }
  throw new Error(`Timed out waiting for ${label}: ${error}`)
}

async function main() {
  const login = await fetch(`${apiUrl}/api/v1/auth/login`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ username, password }),
  })
  if (!login.ok) throw new Error(`Local login failed (${login.status})`)
  const { access_token: token } = await login.json()
  if (typeof token !== 'string') throw new Error('Local login did not return an access token')

  const profile = await mkdtemp(join(tmpdir(), 'openclaim-demo-'))
  const browser = spawn('chromium', [
    '--headless=new', '--no-sandbox', '--ignore-certificate-errors',
    '--remote-debugging-port=9229', `--user-data-dir=${profile}`,
    '--window-size=1440,1080', 'about:blank',
  ], { stdio: 'ignore' })

  try {
    const page = await eventually(async () => {
      const response = await fetch('http://127.0.0.1:9229/json/list')
      if (!response.ok) throw new Error(String(response.status))
      const targets = await response.json()
      const target = targets.find((candidate) => candidate.type === 'page')
      if (!target) throw new Error('no page target')
      return target
    }, 'Chromium DevTools')

    const socket = new WebSocket(page.webSocketDebuggerUrl)
    await new Promise((resolve, reject) => {
      socket.addEventListener('open', resolve, { once: true })
      socket.addEventListener('error', reject, { once: true })
    })
    let sequence = 0
    const pending = new Map()
    socket.addEventListener('message', ({ data }) => {
      const message = JSON.parse(data)
      const resolver = pending.get(message.id)
      if (!resolver) return
      pending.delete(message.id)
      message.error ? resolver.reject(new Error(message.error.message)) : resolver.resolve(message.result)
    })
    const cdp = (method, params = {}) => new Promise((resolve, reject) => {
      const id = ++sequence
      pending.set(id, { resolve, reject })
      socket.send(JSON.stringify({ id, method, params }))
    })

    await cdp('Page.enable')
    await cdp('Page.navigate', { url: appUrl })
    await delay(1200)
    await cdp('Runtime.evaluate', {
      expression: `localStorage.setItem('auth_token', ${JSON.stringify(token)})`,
      awaitPromise: true,
    })
    await cdp('Page.navigate', { url: new URL('/dashboard', appUrl).toString() })
    await delay(2200)
    const image = await cdp('Page.captureScreenshot', { format: 'png', captureBeyondViewport: true })
    await mkdir(dirname(output), { recursive: true })
    await writeFile(output, Buffer.from(image.data, 'base64'))
    socket.close()
    console.log(`Wrote synthetic dashboard screenshot to ${output}`)
  } finally {
    if (browser.exitCode === null) {
      const stopped = new Promise((resolve) => browser.once('exit', resolve))
      browser.kill('SIGTERM')
      await stopped
    }
    await rm(profile, { recursive: true, force: true })
  }
}

await main()
