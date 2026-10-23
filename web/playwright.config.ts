import { defineConfig, devices } from '@playwright/test';

const PORT = 8798;
const DATA = '/tmp/lrtodo-e2e';

export default defineConfig({
  testDir: './tests',
  timeout: 60_000,
  workers: 1,
  use: {
    baseURL: `http://127.0.0.1:${PORT}`,
    ...devices['Pixel 7'],
  },
  webServer: {
    // Requiere `cargo build -p lrtodo-cli` con la PWA ya compilada (npm run build).
    command: `rm -rf ${DATA} && mkdir -p ${DATA} && LRTODO_CONFIG=/dev/null ../target/debug/lrtodo --data-dir ${DATA} serve --bind 127.0.0.1:${PORT}`,
    url: `http://127.0.0.1:${PORT}/api/health`,
    reuseExistingServer: false,
  },
});
