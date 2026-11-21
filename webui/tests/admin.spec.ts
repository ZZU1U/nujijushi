import { expect, test, type Page } from '@playwright/test'
import { execFileSync, spawn, type ChildProcess } from 'node:child_process'
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises'
import { createServer } from 'node:net'
import { tmpdir } from 'node:os'
import { DatabaseSync } from 'node:sqlite'
import { join, resolve } from 'node:path'
import { crc32, deflateSync } from 'node:zlib'

const token = 'local-browser-test-token'
const binary = resolve(process.env.NUJIJUSHI_TEST_BIN || '../target/debug/nujijushi')
let directory: string
let server: ChildProcess
let base: string
let cli: (...args: string[]) => Buffer

function png(height: number, color: number, width = 16) {
  const header = Buffer.alloc(13)
  header.writeUInt32BE(width, 0)
  header.writeUInt32BE(height, 4)
  header.set([8, 2], 8)
  const stride = 1 + width * 3
  const rows = Buffer.alloc(height * stride)
  for (let y = 0; y < height; y++) rows.fill(color, y * stride + 1, (y + 1) * stride)
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
  cli = (...args: string[]) => execFileSync(binary, args, { env, stdio: ['ignore', 'pipe', 'pipe'] })
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
  await expect(page.locator('#bulk')).toHaveAttribute('data-grid-page', '3')
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
  await expect(page.locator('#bulk')).toHaveAttribute('data-grid-page', '3')
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
  await page.getByText('Edit post details', { exact: true }).click()
  const draft = page.getByLabel('Title', { exact: true })
  await draft.fill('An unsaved title')
  await page.getByText('Media tools', { exact: true }).click()
  for (const index of [1, 2, 4]) await page.getByLabel('Include image ' + index + ' in merge').check()
  await page.getByRole('button', { name: 'Merge side by side' }).click()
  await expect(page.locator('.media-grid figure')).toHaveCount(2)
  await expect(draft).toHaveValue('An unsaved title')
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
    '/channels/1/edit', '/settings', '/channels/1/queue', '/channels/1/preview', '/import', '/jobs', '/watches', '/users']
  for (const path of routes) {
    await page.goto(base + path)
    await expect(page.locator('.admin-shell main :is(h1, h2, form)').first(), { message: path }).toBeAttached()
    await expect.poll(() => page.evaluate(() => document.documentElement.scrollWidth), { message: path }).toBe(width)
  }
})

test('wide dashboard job columns scroll inside the table on mobile', async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 })
  await page.route('**/api/v1/dashboard', async (route) => {
    const response = await route.fetch()
    const dashboard = await response.json()
    const jobs = [{
      id: 123, kind: 'watch', kind_label: 'Import artwork discovered by a watched source',
      state: 'done', message: 'Imported artwork', payload: null, post_id: null,
      age: '1m', created: '2026-01-01', progress: 1, total: 1, percent: 100,
      active: false, retryable: false, track: false,
    }]
    await route.fulfill({ response, json: { ...dashboard, jobs } })
  })
  await signIn(page, '/')
  const recentJobs = page.getByRole('region', { name: 'Recent jobs' })
  await expect(recentJobs.getByRole('columnheader', { name: 'result' })).toBeAttached()
  await expect.poll(() => recentJobs.evaluate((table) => table.scrollWidth - table.clientWidth)).toBeGreaterThan(0)
  await expect.poll(() => page.evaluate(() => document.documentElement.scrollWidth)).toBe(390)
})

test('JPEG quality only appears in exact mode and remembers the draft', async ({ page }) => {
  await signIn(page, '/posts/2')
  await page.getByText('Media tools', { exact: true }).click()
  const compress = page.locator('details').filter({ has: page.locator(':scope > summary', { hasText: /^Compress image$/ }) }).first()
  await compress.locator('summary').click()
  const mode = compress.getByLabel('Mode')
  const quality = compress.getByLabel('JPEG quality', { exact: true })
  await expect(mode).toHaveValue('fit')
  await expect(quality).toBeHidden()
  await mode.selectOption('quality')
  await expect(quality).toBeVisible()
  await quality.fill('67')
  await mode.selectOption('fit')
  await expect(quality).toBeHidden()
  await expect(quality).toBeDisabled()
  await mode.selectOption('quality')
  await expect(quality).toHaveValue('67')

  await page.getByText('Merge output options', { exact: true }).click()
  const merge = page.locator('#merge-gallery')
  await expect(merge.getByLabel('Output')).toHaveValue('lossless')
  await expect(merge.getByLabel('JPEG quality', { exact: true })).toBeHidden()
  await merge.getByLabel('Output').selectOption('fit')
  await expect(merge.getByLabel('JPEG quality', { exact: true })).toBeHidden()
  await merge.getByLabel('Output').selectOption('quality')
  await expect(merge.getByLabel('JPEG quality', { exact: true })).toBeVisible()
})

test('both channel forms suggest scheduler timezones and creation preserves failed input', async ({ page }) => {
  const documents = countDocuments(page)
  await signIn(page, '/channels')
  function timezone() { return page.locator('input[name="tz"]') }
  async function suggestions() {
    return timezone().evaluate((element: HTMLInputElement) => Array.from(element.list?.options || [], (option) => option.value))
  }
  await expect(timezone()).toHaveValue('Europe/Berlin')
  const initial = await suggestions()
  expect(initial).toContain('Pacific/Chatham')
  expect(initial).toContain('Europe/Moscow')

  const form = page.locator('#add-channel form')
  await form.getByLabel('alias').fill('new-art')
  await form.getByLabel('title').fill('New Art')
  await form.locator('input[name="tg_id"]').fill('@mock_channel')
  await timezone().fill('Pacific/Chatham')
  let finish: () => void = () => {}
  const verification = new Promise<void>((resolve) => { finish = resolve })
  await page.route('**/api/v1/channels', async (route) => {
    if (route.request().method() === 'POST') await verification
    await route.continue()
  })
  await form.getByRole('button', { name: 'create channel' }).click()
  try {
    await expect(form.getByText('Checking Telegram…')).toBeVisible()
  } finally {
    finish()
  }
  await expect(form.locator('.action-error')).toContainText('Configure a Telegram bot token')
  await expect(timezone()).toHaveValue('Pacific/Chatham')
  await expect(form.getByLabel('title')).toHaveValue('New Art')
  await page.getByRole('link', { name: 'art', exact: true }).click()
  await expect(page).toHaveURL(base + '/channels/1/edit')
  await expect.poll(suggestions).toEqual(initial)
  expect(documents()).toBe(1)
})

test('a completed import displays its summary exactly once', async ({ page }) => {
  const documents = countDocuments(page)
  await signIn(page, '/import')
  await page.locator('textarea[name="targets"]').fill(join(directory, 'Different heights'))
  const response = page.waitForResponse((reply) => reply.url().endsWith('/api/v1/import') && reply.request().method() === 'POST')
  await page.getByRole('button', { name: 'start import' }).click()
  const queued = await (await response).json() as { job_ids: number[] }
  const row = page.locator('[data-job="' + queued.job_ids[0] + '"]')
  await expect(row.getByRole('link', { name: 'Open post' })).toBeVisible({ timeout: 10_000 })
  await expect(row.locator('[data-role="message"]')).toHaveText('updated post · 2 files')
  await expect(page.locator('textarea[name="targets"]')).toBeEmpty()
  expect(documents()).toBe(1)
})

test('queue reflects publishes from both tabs and scheduler delivery without reloading', async ({ page, context }) => {
  for (const post of [20, 21, 22, 23]) cli('queue', 'add', String(post), '--channel', 'art')
  const documents = countDocuments(page)
  await signIn(page, '/channels/1/queue')
  const queueRow = (tab: Page, post: number) => tab.locator('[data-queue]').filter({ has: tab.getByRole('link', { name: '#' + post, exact: true }) })
  const own = queueRow(page, 20)
  const ownId = await own.getAttribute('data-queue')
  page.once('dialog', (dialog) => void dialog.accept())
  await own.getByRole('button', { name: 'publish', exact: true }).click()
  const publishJob = (id: string | null) => page.locator('[data-publish-job]').filter({ hasText: 'Queue item #' + id })
  await expect(publishJob(ownId)).toContainText('Failed', { timeout: 12_000 })

  const other = await context.newPage()
  await other.goto(base + '/channels/1/queue')
  const elsewhere = queueRow(other, 21)
  const otherId = await elsewhere.getAttribute('data-queue')
  other.once('dialog', (dialog) => void dialog.accept())
  const queuedElsewhere = other.waitForResponse((response) =>
    response.url().endsWith('/api/v1/publish') && response.request().method() === 'POST')
  await elsewhere.getByRole('button', { name: 'publish', exact: true }).click()
  expect((await queuedElsewhere).status()).toBe(202)
  await page.bringToFront()
  await expect(publishJob(otherId)).toContainText('Failed', { timeout: 12_000 })
  await other.close()

  const running = queueRow(page, 22)
  const uncertain = queueRow(page, 23)
  const runningId = Number(await running.getAttribute('data-queue'))
  const unknownId = Number(await uncertain.getAttribute('data-queue'))
  const db = new DatabaseSync(join(directory, 'archive', 'nujijushi.sqlite3'))
  try {
    db.exec('PRAGMA busy_timeout = 5000')
    // Write the intermediate states a worker in a separate process would commit.
    db.prepare("UPDATE queue SET state = 'sending' WHERE id = ?").run(runningId)
    db.prepare("UPDATE queue SET state = 'unknown' WHERE id = ?").run(unknownId)
    await expect(running.getByText('Sending', { exact: true })).toBeVisible()
    await expect(uncertain.getByText('Delivery unconfirmed', { exact: true })).toBeVisible()
    const messageId = uncertain.getByLabel('Telegram message ID (optional)')
    await messageId.fill('4321')
    await messageId.scrollIntoViewIfNeeded()
    db.exec('BEGIN')
    db.prepare("UPDATE queue SET state = 'sent', sent_at = ? WHERE id = ?").run(new Date().toISOString(), runningId)
    db.prepare('INSERT INTO sends (channel_id, queue_id, post_id, sent_at, ok) VALUES (1, ?, 22, ?, 1)')
      .run(runningId, new Date().toISOString())
    db.exec('COMMIT')
    await expect(running).toHaveCount(0)
    await expect(page.locator('.queue-attempt')).toContainText('Sent')
    await expect(messageId).toHaveValue('4321')
  } finally {
    db.close()
  }
  expect(documents()).toBe(1)
})

test('navigation motion follows the reduced-motion preference', async ({ page }) => {
  await page.emulateMedia({ reducedMotion: 'reduce' })
  await signIn(page, '/channels')
  await expect(page.locator('#add-channel')).toBeVisible()
  await expect(page.locator('main')).toHaveCSS('animation-name', 'none')
  await page.emulateMedia({ reducedMotion: 'no-preference' })
  await page.getByRole('link', { name: 'import', exact: true }).click()
  await expect(page).toHaveURL(base + '/import')
  await expect(page.locator('main')).toHaveCSS('animation-name', 'view-in')
})

test('Posts appends batches, retains selections, and cancels an obsolete batch', async ({ page }) => {
  await page.setViewportSize({ width: 1000, height: 600 })
  const documents = countDocuments(page)
  await signIn(page, '/posts')
  const cards = page.locator('[data-post-card]')
  await expect(cards).toHaveCount(24)
  const pick = cards.first().locator('[data-post-pick]')
  await pick.check({ force: true })
  await page.locator('[data-posts-more]').scrollIntoViewIfNeeded()
  await expect(cards).toHaveCount(48)
  await expect(pick).toBeChecked()

  const post = cards.nth(32).getByRole('link', { name: /Open post/ })
  await post.scrollIntoViewIfNeeded()
  const scroll = await page.evaluate(() => scrollY)
  await post.click()
  await page.getByRole('link', { name: '← Posts' }).click()
  await expect(cards).toHaveCount(48)
  await expect(pick).toBeChecked()
  await expect.poll(() => page.evaluate(() => scrollY)).toBe(scroll)

  let started: () => void = () => {}
  let finish: () => void = () => {}
  const startedBatch = new Promise<void>((resolve) => { started = resolve })
  const releaseBatch = new Promise<void>((resolve) => { finish = resolve })
  await page.route('**/api/v1/posts/cards?*', async (route) => {
    started()
    await releaseBatch
    await route.continue().catch(() => {})
  })
  await page.locator('[data-posts-more]').scrollIntoViewIfNeeded()
  await startedBatch
  try {
    await page.getByRole('textbox', { name: 'Search posts' }).fill('Different heights')
    await page.getByRole('textbox', { name: 'Search posts' }).press('Enter')
    await expect(cards).toHaveCount(1)
    await expect(page).toHaveURL(/search=Different\+heights/)
  } finally {
    finish()
  }
  await expect(cards).toHaveCount(1)
  expect(documents()).toBe(1)
})

test('Posts applies card tags and archive queue defaults with overrides', async ({ page }) => {
  const db = new DatabaseSync(join(directory, 'archive', 'nujijushi.sqlite3'))
  try {
    db.exec("PRAGMA busy_timeout=5000; INSERT OR IGNORE INTO tags (name) VALUES ('browser_tag');")
    db.exec("INSERT OR IGNORE INTO post_tags (post_id,tag_id,origin) SELECT 2,id,'manual' FROM tags WHERE name='browser_tag';")
  } finally {
    db.close()
  }
  cli('channel', 'add', 'second', '@local_suggestion', '--no-verify')
  await signIn(page, '/settings')
  await page.getByLabel('Default queue channel').selectOption('2')
  await page.getByRole('button', { name: 'Save settings' }).click()
  await expect(page.getByText('Queue preference saved')).toBeVisible()
  await page.getByRole('link', { name: 'posts', exact: true }).click()
  await page.getByRole('combobox', { name: 'Source', exact: true }).selectOption('local')
  await expect(page).toHaveURL(/source=local/)
  await page.getByRole('textbox', { name: 'Search posts' }).fill('Different heights')
  await expect(page.locator('[data-post-card]')).toHaveCount(1)
  const card = page.locator('[data-post-card]').first()
  await card.getByRole('link', { name: '#browser_tag' }).click()
  await expect(page).toHaveURL(/source=local.*tag=browser_tag/)
  await expect(page.locator('#posts-tag-menu .tagcloud a').first()).toHaveAttribute('aria-current', 'true')
  await page.getByRole('link', { name: 'Clear tag' }).click()
  await expect(page).toHaveURL(/source=local/)
  expect(new URL(page.url()).searchParams.has('tag')).toBe(false)
  await expect(page.getByLabel('Destination channel')).toHaveValue('2')
  await card.getByRole('link', { name: /Open post/ }).click()
  const queue = page.getByLabel('Destination', { exact: true })
  await expect(queue).toHaveValue('2')
  await queue.selectOption('1')
  await page.getByRole('button', { name: 'Add to queue' }).click()
  await expect(page.getByText(/queued.*art/i)).toBeVisible()
  await page.getByRole('link', { name: 'Delete post…' }).click()
  await page.getByRole('button', { name: 'Delete post', exact: true }).click()
  await expect(page).toHaveURL(/search=Different\+heights.*source=local/)
  await expect(page.locator('[data-post-card]')).toHaveCount(0)
})

test('table headers align with their rows and stick to the table scrollport', async ({ page }) => {
  const jobs = Array.from({ length: 40 }, (_, index) => ({
    id: index + 1, kind: 'import', kind_label: 'import', state: 'done', payload: '/archive/artwork',
    message: 'Imported', progress: 1, total: 1, percent: 100, age: '1m', created: '2026-01-01',
    post_id: null, active: false, retryable: false, track: false,
  }))
  const jobPage = { jobs, parallel: 2, waiting: 0 }
  await page.route('**/api/v1/jobs', (route) => route.fulfill({ json: jobPage }))
  await page.route('**/api/v1/import', (route) => route.fulfill({ json: { ...jobPage, cookies: '/archive/cookies' } }))
  await page.setViewportSize({ width: 390, height: 740 })
  await signIn(page, '/jobs')

  const table = page.getByRole('table', { name: 'Job history' })
  const scrollport = page.getByRole('region', { name: 'Job history' })
  const header = table.getByRole('columnheader', { name: '#', exact: true })
  await expect(header).toBeVisible()
  await expect.poll(() => header.evaluate((cell) => {
    const row = cell.closest('tr')!
    return Math.abs(cell.getBoundingClientRect().top - row.getBoundingClientRect().top)
  })).toBeLessThan(1)

  await scrollport.evaluate((element) => element.scrollTo(80, 240))
  await expect.poll(() => scrollport.evaluate((element) => element.scrollTop)).toBeGreaterThan(0)
  await expect.poll(() => scrollport.evaluate((element) => element.scrollLeft)).toBeGreaterThan(0)
  const alignment = await header.evaluate((cell) => {
    const table = cell.closest('table')!
    const viewport = table.parentElement!.getBoundingClientRect()
    const firstDataCell = table.tBodies[0].rows[0].cells[0].getBoundingClientRect()
    const label = cell.getBoundingClientRect()
    return { top: Math.abs(label.top - viewport.top), left: Math.abs(label.left - firstDataCell.left) }
  })
  expect(alignment.top).toBeLessThan(1)
  expect(alignment.left).toBeLessThan(1)

  await page.setViewportSize({ width: 1440, height: 740 })
  await page.getByRole('link', { name: 'import', exact: true }).click()
  const imports = page.getByRole('region', { name: 'Import jobs' })
  const importHeader = imports.getByRole('columnheader', { name: 'state', exact: true })
  await expect(importHeader).toBeVisible()
  await imports.evaluate((element) => element.scrollTo(0, 240))
  await expect.poll(() => imports.evaluate((element) => element.scrollTop)).toBeGreaterThan(0)
  await expect.poll(() => importHeader.evaluate((cell) =>
    Math.abs(cell.getBoundingClientRect().top - cell.closest('[role=region]')!.getBoundingClientRect().top),
  )).toBeLessThan(1)
})

test('flushing a channel queue asks for confirmation and shows the job result', async ({ page }) => {
  cli('channel', 'add', 'flush-test', '@flush_test_channel', '--no-verify')
  const db = new DatabaseSync(join(directory, 'archive', 'nujijushi.sqlite3'), { readOnly: true })
  const channel = db.prepare("SELECT id FROM channels WHERE alias = 'flush-test'").get()!
  db.close()
  const path = `/channels/${channel.id}/queue`
  await signIn(page, path)
  const button = page.getByRole('button', { name: 'Flush queue', exact: true })
  await expect(button).toBeDisabled()
  cli('queue', 'add', '20', '21', '--channel', 'flush-test')
  await expect(button).toBeEnabled()

  page.once('dialog', (dialog) => {
    expect(dialog.message()).toContain('@flush_test_channel')
    void dialog.dismiss()
  })
  await button.click()
  await expect(page.locator('[data-publish-job]')).toHaveCount(0)

  page.once('dialog', (dialog) => void dialog.accept())
  const response = page.waitForResponse((reply) => reply.url().endsWith(`/api/v1/channels/${channel.id}/flush`)
    && reply.request().method() === 'POST')
  await button.click()
  expect((await response).status()).toBe(202)
  const job = page.locator('[data-publish-job]').filter({ hasText: 'Flush queue' })
  await expect(job).toContainText('Failed', { timeout: 12_000 })
  await expect(job).toContainText('0 published · 0 skipped')
  await expect(button).toBeEnabled()
})

test('the archive grid caches thumbnails and the post gallery loads full-resolution images', async ({ page }) => {
  const artwork = join(directory, 'Full resolution')
  await mkdir(artwork)
  await writeFile(join(artwork, 'post.json'), JSON.stringify({ id: 'full-resolution', title: 'Full resolution' }))
  await writeFile(join(artwork, 'artwork.png'), png(640, 200, 1024))
  cli('import', artwork)

  await signIn(page, '/posts?search=Full+resolution')
  const card = page.locator('[data-post-card]')
  const thumbnail = card.locator('img')
  await expect(thumbnail).toHaveAttribute('src', /^\/thumb\//)
  await expect.poll(() => thumbnail.evaluate((image: HTMLImageElement) => image.naturalWidth)).toBe(512)
  const thumbnailPath = (await thumbnail.getAttribute('src'))!

  await card.getByRole('link', { name: /Open post/ }).click()
  const fullSizeLink = page.getByRole('link', { name: 'Open image 1 at full size' })
  const image = fullSizeLink.locator('img')
  const mediaPath = thumbnailPath.replace('/thumb/', '/media/')
  await expect(fullSizeLink).toHaveAttribute('href', mediaPath)
  await expect(fullSizeLink).toHaveAttribute('target', '_blank')
  await expect(image).toHaveAttribute('src', mediaPath)
  await expect.poll(() => image.evaluate((element: HTMLImageElement) => element.naturalWidth)).toBe(1024)
  await expect.poll(() => page.evaluate(async (path) => {
    const cached = await fetch(path, { cache: 'only-if-cached', mode: 'same-origin' })
    return cached.status
  }, thumbnailPath)).toBe(200)
})

test('compress all updates the post and Settings cleans a confirmed sent gallery', async ({ page }) => {
  const artwork = await gallery('Cleanup example', [14, 14])
  cli('import', artwork)
  const db = new DatabaseSync(join(directory, 'archive', 'nujijushi.sqlite3'))
  db.exec('PRAGMA busy_timeout = 5000')
  const item = db.prepare("SELECT id FROM posts WHERE title = 'Cleanup example'").get()!
  const id = String(item.id)
  try {
    await signIn(page, '/posts/' + id)
    await page.getByText('Media tools', { exact: true }).click()
    await page.getByText('Compress all images', { exact: true }).click()
    await page.getByRole('button', { name: 'Compress all', exact: true }).click()
    await expect(page.getByText('2 compressed · 0 skipped', { exact: true })).toBeVisible()
    await expect(page.getByRole('button', { name: 'Undo compression' })).toHaveCount(2)

    cli('queue', 'add', id, '--channel', 'art')
    db.prepare('UPDATE posts SET imported_at = ? WHERE id = ?')
      .run(new Date(Date.now() - 32 * 86400000).toISOString(), item.id)
    db.prepare("UPDATE queue SET state = 'sent', sent_at = ?, tg_message_ids = '123' WHERE post_id = ?")
      .run(new Date(Date.now() - 31 * 86400000).toISOString(), item.id)
    await page.getByRole('link', { name: 'settings', exact: true }).click()
    const cleanup = page.getByRole('heading', { name: 'Storage cleanup' }).locator('..')
    await expect(cleanup.getByRole('spinbutton')).toHaveValue('30')
    await cleanup.getByRole('button', { name: 'Preview cleanup' }).click()
    await expect(cleanup.getByText(/1 eligible post/)).toBeVisible()
    page.once('dialog', (dialog) => void dialog.dismiss())
    await cleanup.getByRole('button', { name: 'Apply cleanup' }).click()
    await expect(cleanup.getByRole('button', { name: 'Apply cleanup' })).toBeVisible()
    page.once('dialog', (dialog) => void dialog.accept())
    await cleanup.getByRole('button', { name: 'Apply cleanup' }).click()
    await expect(cleanup.getByText(/1 post\(s\) cleaned.*0 file\(s\) to retry/)).toBeVisible()

    await page.goto(base + '/posts/' + id)
    await page.getByText('Media tools', { exact: true }).click()
    await expect(page.getByRole('button', { name: 'Undo compression' })).toHaveCount(0)
    await expect(page.locator('.media-grid figcaption').first()).toContainText('jpg')
  } finally {
    db.close()
  }
})
