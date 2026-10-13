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
  // webwallgl 自 3.0.0 起是**npm 依赖**（registry 上的 webwallgl@^3.0.0），不再是
  // file:../webwallgl-github/dist/lib。历史上踩的坑是：file: 依赖做依赖预构建后会一直发
  // 缓存里的旧包（vite 预构建缓存只按锁文件/配置哈希失效、不看内容），当时用
  // optimizeDeps.exclude 绕开。换成 npm 依赖后内容只随版本号变，预构建缓存随之失效，
  // 这个绕法不需要了 —— 库本体 1.4 MB 且静态 import mediabunny，交给 Vite 预构建反而更快。
  // 另注：**残留的旧 vite 会占住 1420**（strictPort），新实例静默起不来、页面继续走旧进程，
  // 重启前先确认没有旧的 vite 在跑。
  server: {
    port: 1420,
    strictPort: true,
    host: host || true,
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
