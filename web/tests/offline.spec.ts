import { expect, test, type Page } from '@playwright/test';

const LIST = 'super';

async function serverTasks(page: Page) {
  const resp = await page.request.post('api/sync', { data: { device: 'e2e', ops: [] } });
  const body = await resp.json();
  const snap = body.snapshots.find((s: { list: string }) => s.list === LIST);
  return (snap?.tasks ?? []) as { title: string; done?: boolean }[];
}

async function addTask(page: Page, title: string) {
  await page.getByTestId('add-input').fill(title);
  await page.getByTestId('add-input').press('Enter');
}

const task = (page: Page, title: string) => page.getByTestId('task').filter({ hasText: title });

test.beforeAll(async ({ request }) => {
  await request.post('api/lists', { data: { id: LIST, name: 'Súper' } });
});

test('en el súper sin señal: la lista sigue ahí y se puede usar', async ({ page, context }) => {
  await page.goto('./');
  await page.getByTestId('list-link').filter({ hasText: 'Súper' }).click();
  await addTask(page, 'leche');
  await addTask(page, 'pan');
  await expect(task(page, 'leche')).toBeVisible();
  await expect(page.getByTestId('sync-status')).toHaveText('Al día');
  await expect.poll(async () => (await serverTasks(page)).map((t) => t.title)).toEqual(['leche', 'pan']);

  // Esperar a que el service worker controle la página (app shell en cache).
  await page.evaluate(async () => {
    await navigator.serviceWorker.ready;
  });
  await page.reload();
  await expect.poll(() => page.evaluate(() => !!navigator.serviceWorker.controller)).toBe(true);

  // Se corta la señal y se reabre la app.
  await context.setOffline(true);
  await page.reload();
  // Abre directo en la última lista, con los datos cacheados.
  await expect(task(page, 'leche')).toBeVisible();
  await expect(task(page, 'pan')).toBeVisible();
  await expect(page.getByTestId('sync-status')).toContainText('Sin conexión');

  // Tildar y agregar sin conexión.
  await task(page, 'leche').getByRole('checkbox').check();
  await addTask(page, 'huevos');
  await expect(task(page, 'huevos')).toBeVisible();
  await expect(task(page, 'huevos')).toHaveClass(/pending/);
  await expect(page.getByTestId('open-tasks').getByTestId('task')).toHaveCount(2);

  // Vuelve la señal: se envía todo.
  await context.setOffline(false);
  await page.evaluate(() => window.dispatchEvent(new Event('online')));
  await expect(page.getByTestId('sync-status')).toHaveText('Al día', { timeout: 15_000 });
  await expect(task(page, 'huevos')).not.toHaveClass(/pending/);
  const tasks = await serverTasks(page);
  expect(tasks.map((t) => t.title)).toEqual(['leche', 'pan', 'huevos']);
  expect(tasks.find((t) => t.title === 'leche')?.done).toBe(true);
});

test('red lenta: la UI no espera al servidor', async ({ page }) => {
  await page.goto(`./#/l/${LIST}`);
  await expect(task(page, 'pan')).toBeVisible();
  await expect(page.getByTestId('sync-status')).toHaveText('Al día');

  // Cada sync tarda 30 s (más que el timeout del cliente).
  await page.route('**/api/sync', async (route) => {
    await new Promise((r) => setTimeout(r, 30_000));
    await route.continue().catch(() => {});
  });
  await page.reload();
  // La lista aparece al instante desde el cache, mientras sincroniza.
  await expect(task(page, 'pan')).toBeVisible({ timeout: 2_000 });
  await expect(page.getByTestId('sync-status')).toHaveText('Sincronizando…');
  const t0 = Date.now();
  await addTask(page, 'café');
  await expect(task(page, 'café')).toBeVisible();
  await task(page, 'pan').getByRole('checkbox').check();
  expect(Date.now() - t0).toBeLessThan(2_000);
  // Tras el timeout, informa que el servidor no responde, sin perder nada.
  await expect(page.getByTestId('sync-status')).toContainText('Sin conexión', { timeout: 15_000 });
  await expect(task(page, 'café')).toHaveClass(/pending/);

  await page.unroute('**/api/sync');
  await page.evaluate(() => window.dispatchEvent(new Event('online')));
  await expect(page.getByTestId('sync-status')).toHaveText('Al día', { timeout: 15_000 });
  expect((await serverTasks(page)).map((t) => t.title)).toContain('café');
});

test('dos pestañas no se pisan', async ({ context, page }) => {
  await page.goto(`./#/l/${LIST}`);
  const other = await context.newPage();
  await other.goto(`./#/l/${LIST}`);
  await expect(task(other, 'huevos')).toBeVisible();
  await context.setOffline(true);
  await addTask(page, 'tab1');
  await addTask(other, 'tab2');
  await expect(task(page, 'tab2')).toBeVisible();
  await expect(task(other, 'tab1')).toBeVisible();
  await context.setOffline(false);
  await page.evaluate(() => window.dispatchEvent(new Event('online')));
  await expect
    .poll(async () => (await serverTasks(page)).map((t) => t.title), { timeout: 15_000 })
    .toEqual(expect.arrayContaining(['tab1', 'tab2']));
});
