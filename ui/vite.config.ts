import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';

export default defineConfig({
  plugins: [svelte()],
  base: '/',
  build: { outDir: 'dist', emptyOutDir: true },
  server: {
    proxy: {
      '/api': {
        target: 'http://127.0.0.1:7845',
        changeOrigin: true,
        ws: true,
        // clipr refuses requests whose Origin isn't its own address.
        headers: { Origin: 'http://127.0.0.1:7845' },
      },
    },
  },
});
