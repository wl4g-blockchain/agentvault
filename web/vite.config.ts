/// <reference types="vitest/config" />
import { defineConfig, loadEnv } from 'vite'
import react from '@vitejs/plugin-react'

export default defineConfig(({ mode }) => {
  const env = loadEnv(mode, '.', '')
  const apiTarget = env.AGENTVAULT_API_TARGET || 'http://127.0.0.1:8080'
  const managementTarget = env.AGENTVAULT_MANAGEMENT_TARGET || 'http://127.0.0.1:9091'

  return {
    plugins: [react()],
    server: {
      host: '127.0.0.1',
      port: 4173,
      proxy: {
        '/api/v1': apiTarget,
        '/agentvault-management': {
          target: managementTarget,
          rewrite: path => path.replace(/^\/agentvault-management/, ''),
        },
      },
    },
    test: {
      environment: 'jsdom',
      include: ['tests/**/*_test.{ts,tsx}'],
      restoreMocks: true,
    },
  }
})
