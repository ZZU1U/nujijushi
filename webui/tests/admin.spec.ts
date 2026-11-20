import { expect, test, type Page } from '@playwright/test'
import { execFileSync, spawn, type ChildProcess } from 'node:child_process'
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises'
import { createServer } from 'node:net'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'
import { crc32, deflateSync } from 'node:zlib'

const token = 'local-browser-test-token'
const binary = resolve(process.env.NUJIJUSHI_TEST_BIN || '../target/debug/nujijushi')
let directory: string
let server: ChildProcess
let base: string

function png(height: number, color: number) {
  const width = 16
  const header = Buffer.alloc(13)
  header.writeUInt32BE(width, 0)
  header.writeUInt32BE(height, 4)
  header.set([8, 2], 8)
  const rows = Buffer.alloc(height * (1 + width * 3))
  for (let y = 0; y < height; y++) rows.fill(color, y * 49 + 1, (y + 1) * 49)
  function chunk(name: string, data: Buffer) {
    const value = Buffer.concat([Buffer.alloc(4), Buffer.from(name), data, Buffer.alloc(4)])
    value.writeUInt32BE(data.length)
    value.writeUInt32BE(crc32(value.subarray(4, -4)), value.length - 4)
    return value
  }
  return Buffer.concat([Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]),
    chunk('IHDR', header), chunk('IDAT', deflateSync(rows)), chunk('IEND', Buffer.alloc(0))])
}

async function gallery(name: string, heights: number[]) {
  const path = join(directory, name)
  await mkdir(path)
  await writeFile(join(path, 'post.json'), JSON.stringify({ id: name, title: name }))
  await Promise.all(heights.map((height, i) => writeFile(join(path, 'panel-' + i + '.png'), png(height, 30 + i * 20))))
  return path
}

async function localPort() {
  const socket = createServer()
  return new Promise<number>((resolve, reject) => {
    socket.once('error', reject).listen(0, '127.0.0.1', () => {
      const address = socket.address()
      if (!address || typeof address === 'string') return reject(new Error('No loopback port'))
      socket.close(() => resolve(address.port))
    })
  })
}

test.beforeAll(async () => {
  directory = await mkdtemp(join(tmpdir(), 'nujijushi-browser-'))
  const env = { ...process.env, NUJIJUSHI_HOME: join(directory, 'archive'),
    NUJIJUSHI_UI_TOKEN: token, TG_BOT_TOKEN: '', NUJIJUSHI_BOT_MODE: 'polling',
    NUJIJUSHI_BOT: '0', NUJIJUSHI_ADMINS: '', NUJIJUSHI_PROXY: '' }
  const cli = (...args: string[]) => execFileSync(binary, args, { env, stdio: ['ignore', 'pipe', 'pipe'] })
  cli('init')
  cli('channel', 'add', 'art', '@local_test_channel', '--no-verify')
  const valid = await gallery('Merge panels', [12, 12, 12, 12])
  const invalid = await gallery('Different heights', [12, 16])
  cli('import', valid, invalid)
  const posts = join(directory, 'posts')
  await mkdir(posts)
  await Promise.all(Array.from({ length: 54 }, (_, i) =>
    writeFile(join(posts, 'piece-' + i + '-unique.png'), png(16, i))))
  cli('import', posts)
  const port = await localPort()
  base = 'http://127.0.0.1:' + port
  server = spawn(binary, ['serve', '--bind', '127.0.0.1:' + port], { env, stdio: ['ignore', 'ignore', 'inherit'] })
  await expect.poll(async () => {
    try { return (await fetch(base + '/login')).status } catch { return 0 }
  }).toBe(200)
})

test.afterAll(async () => {
  if (server && server.exitCode === null) {
    await new Promise<void>((resolve) => { server.once('exit', () => resolve()); server.kill() })
  }
  if (directory) await rm(directory, { recursive: true, force: true })
})

async function signIn(page: Page, path: string) {
  await page.goto(base + path)
  await page.getByLabel('ui token').fill(token)
  await page.getByRole('button', { name: 'unlock' }).click()
  await expect(page).toHaveURL(base + path)
}
function countDocuments(page: Page) {
  let documents = 0
  page.on('request', (request) => { if (request.resourceType() === 'document') documents++ })
  return () => documents
}

test('queue on page 3 and return from a post in the same browser document', async ({ page }) => {
  await page.setViewportSize({ width: 900, height: 520 })
  const documents = countDocuments(page)
  const path = '/posts?page=3&per_page=24&source=local'
  await signIn(page, path)
  await expect(page.getByText('Page 3 of 3')).toBeVisible()
  const card = page.locator('.card.post').last()
  await card.locator('label.post-pick-area').click()
  await page.getByLabel('Destination channel').selectOption('1')
  await page.getByRole('button', { name: 'queue', exact: true }).click()
  await expect(card.getByText(/queued · art/i)).toBeVisible()
  await expect(page).toHaveURL(base + path)
  const post = card.getByRole('link', { name: /Open post/ })
  await post.scrollIntoViewIfNeeded()
  const savedScroll = await page.evaluate(() => scrollY)
  expect(savedScroll).toBeGreaterThan(0)
  await post.click()
  await expect(page.getByRole('heading', { name: 'Add to queue' })).toBeVisible()
  await page.getByRole('link', { name: '← Posts' }).click()
  await expect(page).toHaveURL(base + path)
  await expect(page.getByText('Page 3 of 3')).toBeVisible()
  await expect.poll(() => page.evaluate(() => scrollY)).toBe(savedScroll)
  expect(documents()).toBe(1)
})

test('a failed merge keeps selection and its error beside the controls', async ({ page }) => {
  const documents = countDocuments(page)
  await signIn(page, '/posts/2')
  await page.getByText('Media tools', { exact: true }).click()
  await page.getByLabel('Include image 1 in merge').check()
  await page.getByLabel('Include image 2 in merge').check()
  await page.getByRole('button', { name: 'Merge side by side' }).click()
  await expect(page.locator('#merge-gallery .action-error')).toContainText('same height')
  await expect(page.getByLabel('Include image 1 in merge')).toBeChecked()
  await expect(page.getByLabel('Include image 2 in merge')).toBeChecked()
  await expect(page).toHaveURL(base + '/posts/2')
  expect(documents()).toBe(1)
})

test('merge 1, 2, and 4, then hide the remaining image in place', async ({ page }) => {
  const documents = countDocuments(page)
  await signIn(page, '/posts/1')
  await page.getByText('Media tools', { exact: true }).click()
  for (const index of [1, 2, 4]) await page.getByLabel('Include image ' + index + ' in merge').check()
  await page.getByRole('button', { name: 'Merge side by side' }).click()
  await expect(page.locator('.media-grid figure')).toHaveCount(2)
  await expect(page.locator('.media-grid figure').first()).toContainText('48×12')
  await page.getByRole('button', { name: 'Hide from outgoing copies' }).nth(1).click()
  await expect(page.locator('.media-grid figure').nth(1)).toContainText('Hidden from outgoing copies')
  await page.getByRole('button', { name: 'Restore', exact: true }).click()
  await expect(page.locator('.media-grid figure').nth(1)).not.toContainText('Hidden from outgoing copies')
  expect(documents()).toBe(1)
})

for (const width of [390, 1440]) test('every admin screen fits a ' + width + 'px viewport', async ({ page }) => {
  await page.setViewportSize({ width, height: 844 })
  await signIn(page, '/')
  const routes = ['/', '/posts', '/posts/1', '/posts/1/delete', '/duplicates', '/channels',
    '/channels/1/edit', '/channels/1/queue', '/channels/1/preview', '/import', '/jobs', '/watches', '/users']
  for (const path of routes) {
    await page.goto(base + path)
    await expect(page.locator('.admin-shell main :is(h1, h2, form)').first(), { message: path }).toBeAttached()
    await expect.poll(() => page.evaluate(() => document.documentElement.scrollWidth), { message: path }).toBe(width)
  }
})
