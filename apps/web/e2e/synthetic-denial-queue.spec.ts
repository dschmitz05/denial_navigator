import { expect, test } from '@playwright/test'

// The companion shell E2E seeds only checked-in synthetic fixtures. This test
// verifies the browser half of that same supported path; it never uploads or
// reads production data. Run with E2E_PASSWORD and the local stack running.
test('synthetic 835 denial is visible in the browser queue', async ({ page }) => {
  const password = process.env.E2E_PASSWORD
  test.skip(!password, 'E2E_PASSWORD is required for the local synthetic user')

  await page.goto('/login')
  await page.locator('input').nth(0).fill(process.env.E2E_USERNAME || 'admin')
  await page.locator('input[type=password]').fill(password!)
  await page.getByRole('button', { name: 'Sign in' }).click()
  await page.waitForURL('**/')

  await page.goto('/denials')
  await expect(page.getByText('PAT001', { exact: true }).first()).toBeVisible()
  await page.getByText('PAT001', { exact: true }).first().click()
  await expect(page.getByText('Denial Details — PAT001')).toBeVisible()
  await expect(page.getByText('197', { exact: true }).first()).toBeVisible()
})
