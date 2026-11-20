import { defineConfig } from "vite";

// BASE_PATH is "/distributed-rate-limiter/" on GitHub Pages and "/" everywhere else.
export default defineConfig({
  base: process.env.BASE_PATH ?? "/",
  build: {
    target: "es2022",
    assetsInlineLimit: 0,
    reportCompressedSize: true,
  },
});
