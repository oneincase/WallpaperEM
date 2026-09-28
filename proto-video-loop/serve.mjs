// 最小静态服务器 —— 只为原型服务，零依赖。
//
// 为什么必须有它（不能直接 file:// 打开）：
//   1. **Range 请求**：无缝循环的 pingpong 方案依赖 `video.currentTime = 0` 精确 seek。
//      浏览器对视频的 seek 走 HTTP Range（206 Partial Content）。file:// 下没有
//      Range，Safari 会把整个文件读进内存再 seek，4K 片直接把内存打满，
//      测出来的"seek 耗时"也就完全失真了。
//   2. 同源：file:// 下 fetch / WebGL 纹理上传会被当成不透明来源。
//   3. 局域网真机验证（iPhone/iPad 也是 WebKit）需要一个 http 地址。
//
// 用法：node serve.mjs [port]

import { createServer } from "node:http";
import { createReadStream, statSync, readdirSync, existsSync } from "node:fs";
import { extname, join, normalize, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { networkInterfaces } from "node:os";

const ROOT = resolve(fileURLToPath(new URL(".", import.meta.url)));
const PORT = Number(process.argv[2]) || 8787;

const MIME = {
  ".html": "text/html; charset=utf-8",
  ".mjs": "text/javascript; charset=utf-8",
  ".js": "text/javascript; charset=utf-8",
  ".css": "text/css; charset=utf-8",
  ".json": "application/json; charset=utf-8",
  ".mp4": "video/mp4",
  ".m4v": "video/mp4",
  ".mov": "video/quicktime",
  ".webm": "video/webm",
  ".mkv": "video/x-matroska",
  ".png": "image/png",
  ".jpg": "image/jpeg",
};

/** 把 URL 路径映射到 ROOT 内的真实文件；越界一律拒绝。 */
function resolveSafe(urlPath) {
  const clean = decodeURIComponent(urlPath.split("?")[0]);
  const p = normalize(join(ROOT, clean));
  if (!p.startsWith(ROOT)) return null; // 目录穿越
  return p;
}

/** 解析 `Range: bytes=a-b`，返回 {start,end} 或 null。 */
function parseRange(header, size) {
  const m = /^bytes=(\d*)-(\d*)$/.exec((header || "").trim());
  if (!m) return null;
  const [, a, b] = m;
  if (a === "" && b === "") return null;
  let start, end;
  if (a === "") {
    // bytes=-N → 末尾 N 字节
    start = Math.max(0, size - Number(b));
    end = size - 1;
  } else {
    start = Number(a);
    end = b === "" ? size - 1 : Math.min(Number(b), size - 1);
  }
  if (!Number.isFinite(start) || !Number.isFinite(end) || start > end || start >= size) return null;
  return { start, end };
}

const server = createServer((req, res) => {
  // /api/clips → clips 目录下的视频清单，前端下拉框直接用，省得手改 HTML
  if (req.url.split("?")[0] === "/api/clips") {
    const dir = join(ROOT, "clips");
    const files = existsSync(dir)
      ? readdirSync(dir)
          .filter((f) => /\.(mp4|m4v|mov|webm|mkv)$/i.test(f))
          .sort()
      : [];
    const body = JSON.stringify(files);
    res.writeHead(200, {
      "content-type": MIME[".json"],
      "content-length": Buffer.byteLength(body),
      "cache-control": "no-store",
    });
    res.end(body);
    return;
  }

  const file = req.url === "/" ? join(ROOT, "index.html") : resolveSafe(req.url);
  if (!file || !existsSync(file)) {
    res.writeHead(404, { "content-type": "text/plain" });
    res.end("404");
    return;
  }

  let st;
  try {
    st = statSync(file);
  } catch {
    res.writeHead(404).end("404");
    return;
  }
  if (st.isDirectory()) {
    res.writeHead(404).end("404");
    return;
  }

  const type = MIME[extname(file).toLowerCase()] ?? "application/octet-stream";
  const range = parseRange(req.headers.range, st.size);

  // 视频必须声明 Accept-Ranges，并正确处理 206，否则 seek 退化成全量下载
  if (range) {
    const { start, end } = range;
    res.writeHead(206, {
      "content-type": type,
      "content-length": end - start + 1,
      "content-range": `bytes ${start}-${end}/${st.size}`,
      "accept-ranges": "bytes",
      "cache-control": "no-store",
    });
    if (req.method === "HEAD") return res.end();
    createReadStream(file, { start, end }).pipe(res);
    return;
  }

  res.writeHead(200, {
    "content-type": type,
    "content-length": st.size,
    "accept-ranges": "bytes",
    "cache-control": "no-store",
  });
  if (req.method === "HEAD") return res.end();
  createReadStream(file).pipe(res);
});

server.listen(PORT, () => {
  const lan = Object.values(networkInterfaces())
    .flat()
    .filter((i) => i && i.family === "IPv4" && !i.internal)
    .map((i) => i.address);
  console.log(`[proto] http://127.0.0.1:${PORT}`);
  for (const ip of lan) console.log(`[proto] http://${ip}:${PORT}  (真机/iPhone 用这个)`);
  console.log("[proto] clips 为空就先跑：bash tools/make-clips.sh");
});
