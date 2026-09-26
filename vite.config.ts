import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import { fileURLToPath } from "node:url";

const here = (p: string) => fileURLToPath(new URL(p, import.meta.url));

const host = process.env.TAURI_DEV_HOST;

// Tauri 桌面应用：主 UI 与壁纸渲染器页为两个独立入口
export default defineConfig({
  plugins: [react(), tailwindcss()],
  clearScreen: false,
  // webwallgl 是 file: 依赖（../webwallgl-github/dist/lib）。开发服务器对它做**依赖预构建**
  // 后会一直发缓存里的旧包 —— vite 的预构建缓存只按锁文件/配置哈希失效、**不看内容**，
  // 所以「改了库 → pnpm install 重链 → 甚至重启 tauri dev」都可能还在跑旧库
  // （踩过两次：三组诊断实验全白做、视频纹理修复重启后不生效）。
  // 库产物是单文件自包含（无外部 import），排除预构建即可每次按 mtime 从磁盘重读。
  // 另注：**残留的旧 vite 会占住 1420**（strictPort），新实例静默起不来、页面继续走旧进程，
  // 重启前先确认没有旧的 vite 在跑。
  optimizeDeps: { exclude: ["webwallgl"] },
  server: {
    port: 1420,
    strictPort: true,
    host: host || false,
    hmr: host ? { protocol: "ws", host, port: 1421 } : undefined,
    watch: { ignored: ["**/src-tauri/**"] },
  },
  build: {
    target: "es2022",
    rollupOptions: {
      input: {
        ui: here("index.html"),
        renderer: here("renderer/index.html"),
        props: here("props.html"),
      },
    },
  },
});
