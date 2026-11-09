import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';

const api = process.env.BOBA_API ?? 'http://localhost:7777';

export default defineConfig({
  plugins: [react()],
  // Real build is served from the origin root (SPA fallback for /invite/*); the static mock
  // demo uses a relative base so it works from any sub-path.
  base: process.env.VITE_MOCK === '1' ? './' : '/',
  server: {
    port: 5173,
    proxy: { '/api': { target: api, changeOrigin: true, ws: true } },
  },
  build: { outDir: 'dist', emptyOutDir: true, chunkSizeWarningLimit: 900 },
});
