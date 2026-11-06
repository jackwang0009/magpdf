import { defineConfig } from 'vitest/config';

// 根配置只负责把各子包聚合成项目；环境等设置在各子包的 vite.config.ts / vitest.config.ts 里。
export default defineConfig({
  test: {
    projects: ['apps/*', 'packages/*'],
  },
});
