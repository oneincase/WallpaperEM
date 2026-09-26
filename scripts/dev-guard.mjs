#!/usr/bin/env node
/**
 * dev 前置守卫：1420 被占时**明确报错并清掉本仓残留的 vite**，而不是让新实例静默起不来。
 *
 * 为什么需要（踩过两次）：`vite.config.ts` 是 strictPort，端口被占时新 vite 直接退出；
 * 但 tauri dev 只在终端里留一行错，页面照旧由**旧 vite** 提供 —— 旧进程的内存里是
 * 旧的依赖预构建结果，于是「改了 webwallgl → pnpm install 重链 → 重启 tauri dev」
 * 看起来完全没生效，实际是在跟一个早该退出的进程说话。
 *
 * 只杀「命令行里带本仓路径的 vite/node」，别的占用者只报错不动手。
 */
import { execSync } from "node:child_process";
import path from "node:path";
import { fileURLToPath } from "node:url";

const PORT = 1420;
const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

function listeners() {
  try {
    const out = execSync(`lsof -nP -iTCP:${PORT} -sTCP:LISTEN -Fpc`, { encoding: "utf8" });
    const rows = [];
    let pid = null;
    for (const line of out.split("\n")) {
      if (line.startsWith("p")) pid = Number(line.slice(1));
      else if (line.startsWith("c") && pid) rows.push({ pid, command: line.slice(1) });
    }
    // 补全命令行，判断是不是本仓的 vite
    return rows.map((r) => {
      let args = "";
      try {
        args = execSync(`ps -o command= -p ${r.pid}`, { encoding: "utf8" }).trim();
      } catch {
        /* 进程可能刚退出 */
      }
      return { ...r, args };
    });
  } catch {
    return []; // lsof 无输出 = 端口空闲
  }
}

const busy = listeners();
if (busy.length === 0) process.exit(0);

const mine = busy.filter((r) => r.args.includes(root) && /vite/.test(r.args));
for (const r of busy) {
  console.error(`[dev-guard] ${PORT} 被占用：pid=${r.pid} ${r.args || r.command}`);
}
if (mine.length === busy.length) {
  console.error("[dev-guard] 都是本仓残留的 vite —— 清掉后继续启动（否则新实例起不来、页面继续走旧进程）");
  for (const r of mine) {
    try {
      process.kill(r.pid, "SIGTERM");
    } catch {
      /* 已退出 */
    }
  }
  // 给 SIGTERM 一点时间释放端口
  const until = Date.now() + 3000;
  while (Date.now() < until && listeners().length > 0) {
    try {
      execSync("sleep 0.2");
    } catch {
      /* ignore */
    }
  }
  if (listeners().length === 0) process.exit(0);
  console.error(`[dev-guard] 清理后 ${PORT} 仍被占用，请手工处理`);
}
process.exit(1);
