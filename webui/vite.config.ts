import { defineConfig } from 'vite'

export default defineConfig({
  base: '/admin-assets/',
  server: {
    proxy: {
      '/api': 'http://127.0.0.1:8788',
      '/media': 'http://127.0.0.1:8788',
      '/thumb': 'http://127.0.0.1:8788',
    },
  },
})
