import path from 'path';
import { defineConfig, loadEnv } from 'vite';
import react from '@vitejs/plugin-react';
import { readFileSync } from 'fs';

const appVersion: string = JSON.parse(readFileSync(path.resolve(__dirname, '../desktop/src-tauri/tauri.conf.json'), 'utf8')).version;

export default defineConfig(({ mode }) => {
  const env = loadEnv(mode, '.', '');
  return {
    server: {
      port: 3791,
      host: '0.0.0.0',
      proxy: {
        '/v1': {
          target: 'http://127.0.0.1:8791',
          changeOrigin: true,
        },
        '/setup': {
          target: 'http://127.0.0.1:8791',
          changeOrigin: true,
        },
        '/engine': {
          target: 'http://127.0.0.1:8791',
          changeOrigin: true,
        },
        '/health': {
          target: 'http://127.0.0.1:8791',
          changeOrigin: true,
        },
        // There is deliberately no proxy for the retired ACE Node service:
        // the studio talks to the native Rust server only, so a stray legacy
        // request fails loudly in development instead of silently 500-ing.
      },
    },
    optimizeDeps: {
      exclude: ['@ffmpeg/ffmpeg', '@ffmpeg/util'],
    },
    define: {
      __APP_VERSION__: JSON.stringify(appVersion),
    },
    plugins: [react()],
    resolve: {
      alias: {
        '@': path.resolve(__dirname, '.'),
      }
    }
  };
});
