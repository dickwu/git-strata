import type { NextConfig } from 'next';

// Tauri loads a static export from ../dist (tauri.conf.json frontendDist).
const nextConfig: NextConfig = {
  output: 'export',
  images: { unoptimized: true },
  distDir: 'dist',
  turbopack: { root: process.cwd() },
};

export default nextConfig;
