import { existsSync, renameSync } from "node:fs";
import { resolve } from "node:path";
import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";
import { viteSingleFile } from "vite-plugin-singlefile";

/**
 * 构建产物落回 `server/static/console.html`，保持 Rust 侧
 * `include_str!("../../static/console.html")` 的「单文件打进二进制」特性不变。
 *
 * 代价：antd + React 全量内联成一个 HTML，体积比原来的手写版大一个数量级。
 * 这是本地控制台，gzip 后几百 KB，值。
 */
export default defineConfig({
  plugins: [
    react(),
    viteSingleFile(),
    {
      name: "emit-console-html",
      closeBundle() {
        const outDir = resolve(__dirname, "../static");
        const built = resolve(outDir, "index.html");
        if (existsSync(built)) renameSync(built, resolve(outDir, "console.html"));
      },
    },
  ],
  // 开发时代理到本地 Rust 服务。默认 `127.0.0.1:8080`
  //（`server/src/main.rs` 的 RIDER_SIM_ADDR 缺省值）。
  server: {
    port: 5173,
    proxy: {
      "/ws": { target: "ws://127.0.0.1:8080", ws: true },
      "/api": { target: "http://127.0.0.1:8080" },
      "/healthz": { target: "http://127.0.0.1:8080" },
    },
  },
  build: {
    outDir: "../static",
    emptyOutDir: false,
    target: "es2020",
    cssCodeSplit: false,
    assetsInlineLimit: 100_000_000,
    chunkSizeWarningLimit: 4096,
  },
});
