// Steam 工坊描述的 BBCode → React 富文本渲染。
//
// 工坊作者用 BBCode 排版（[h1]/[b]/[url=]/[img]/[list] 等），Detail 页若按纯文本
// 展示会把标记符号原样糊给用户。这里手工做一个小解析器：
// - 产出 React 元素而非 innerHTML —— 结构上免疫 XSS，不需要白名单消毒库
// - 链接一律经 openUrl 走系统浏览器（Tauri webview 里直接导航会把应用本体顶掉）
// - 未识别的标签丢弃壳子、保留内容文本（宁可少排版，不可藏内容）
import type { ReactNode } from "react";
import { openUrl } from "@tauri-apps/plugin-opener";

type BbNode = string | { tag: string; attr?: string; children: BbNode[] };

/** 标签配对解析成树；不闭合的标签在末尾自动闭合，孤儿闭合标签按文本保留 */
function parse(input: string): BbNode[] {
  const root: BbNode[] = [];
  const stack: { tag: string; attr?: string; children: BbNode[] }[] = [];
  const cur = () => (stack.length ? stack[stack.length - 1].children : root);
  const re = /\[(\/?)([a-zA-Z0-9*]+)(?:=([^\]]*))?\]/g;
  let last = 0;
  for (const m of input.matchAll(re)) {
    if (m.index > last) cur().push(input.slice(last, m.index));
    last = m.index + m[0].length;
    const closing = m[1] === "/";
    const tag = m[2].toLowerCase();
    const attr = m[3];
    if (closing) {
      const idx = stack.map((s) => s.tag).lastIndexOf(tag);
      if (idx >= 0) {
        while (stack.length > idx) {
          const done = stack.pop()!;
          cur().push(done);
        }
      } else {
        cur().push(m[0]); // 没有对应开标签：原样显示，别吃掉
      }
    } else if (tag === "*") {
      // [list] 的列表项标记：无闭合的空节点
      cur().push({ tag: "*", children: [] });
    } else {
      stack.push({ tag, attr, children: [] });
    }
  }
  if (last < input.length) cur().push(input.slice(last));
  while (stack.length) {
    const done = stack.pop()!;
    cur().push(done);
  }
  return root;
}

/** 只允许 http(s) 链接/图片；steam://、javascript: 等一律不给过 */
function safeHttpUrl(raw: string): string | null {
  const u = raw.trim();
  return /^https?:\/\//i.test(u) ? u : null;
}

function ExternalLink({ href, children }: { href: string; children: ReactNode }) {
  const url = safeHttpUrl(href);
  if (!url) return <>{children}</>;
  return (
    <a
      href={url}
      className="text-[var(--accent-strong)] underline decoration-[var(--accent-strong)]/40 underline-offset-2 hover:decoration-[var(--accent-strong)]"
      onClick={(e) => {
        e.preventDefault();
        void openUrl(url).catch(() => {});
      }}
    >
      {children}
    </a>
  );
}

function renderNodes(nodes: BbNode[], keyPrefix: string): ReactNode[] {
  return nodes.map((n, i) => {
    const key = `${keyPrefix}-${i}`;
    if (typeof n === "string") return n;
    const inner = renderNodes(n.children, key);
    switch (n.tag) {
      case "h1":
      case "h2":
      case "h3":
        return (
          <div
            key={key}
            className={`mt-2 mb-1 font-semibold text-[var(--text-1)] first:mt-0 ${
              n.tag === "h1" ? "text-[15px]" : n.tag === "h2" ? "text-[14px]" : "text-[13px]"
            }`}
          >
            {inner}
          </div>
        );
      case "b":
        return <strong key={key}>{inner}</strong>;
      case "i":
        return <em key={key}>{inner}</em>;
      case "u":
        return (
          <span key={key} className="underline underline-offset-2">
            {inner}
          </span>
        );
      case "s":
      case "strike":
        return <s key={key}>{inner}</s>;
      case "url": {
        // 两种形态：[url=链接]文字[/url] 与 [url]链接[/url]
        const href = n.attr ?? n.children.map((c) => (typeof c === "string" ? c : "")).join("");
        return (
          <ExternalLink key={key} href={href}>
            {n.attr ? inner : href}
          </ExternalLink>
        );
      }
      case "img": {
        const src = safeHttpUrl(n.children.map((c) => (typeof c === "string" ? c : "")).join(""));
        if (!src) return null;
        return (
          <img
            key={key}
            src={src}
            alt=""
            loading="lazy"
            draggable={false}
            className="my-1.5 block max-w-full rounded-md"
          />
        );
      }
      case "list":
        return (
          <ul key={key} className="my-1 list-disc pl-5">
            <ListItems nodes={n.children} keyPrefix={key} />
          </ul>
        );
      case "olist":
        return (
          <ol key={key} className="my-1 list-decimal pl-5">
            <ListItems nodes={n.children} keyPrefix={key} />
          </ol>
        );
      case "quote":
        return (
          <blockquote
            key={key}
            className="my-1.5 border-l-2 border-[var(--separator)] pl-2.5 text-[var(--text-2)]"
          >
            {inner}
          </blockquote>
        );
      case "code":
        return (
          <code
            key={key}
            className="my-1 block whitespace-pre-wrap rounded-md bg-white/10 px-2 py-1.5 font-mono text-[12px]"
          >
            {inner}
          </code>
        );
      case "spoiler":
        return (
          <span
            key={key}
            className="cursor-pointer rounded-sm bg-[var(--separator)] px-1 blur-[3px] transition hover:blur-0"
            title="Spoiler"
          >
            {inner}
          </span>
        );
      case "hr":
        return <hr key={key} className="my-2 border-[var(--separator)]" />;
      case "noparse":
        // 语义是不解析内部 BBCode；解析器已按标签树处理了，这里退化为原样输出文本
        return (
          <span key={key}>
            {n.children.map((c) => (typeof c === "string" ? c : "")).join("")}
          </span>
        );
      default:
        // 未识别标签（table/tr/td、previewyoutube 等）：丢壳子留内容
        return <span key={key}>{inner}</span>;
    }
  });
}

/** [list] 的子项：[*] 节点是项与项之间的分隔（Steam 语法：[list][*]a[*]b[/list]） */
function ListItems({ nodes, keyPrefix }: { nodes: BbNode[]; keyPrefix: string }) {
  const items: BbNode[][] = [[]];
  for (const n of nodes) {
    if (typeof n !== "string" && n.tag === "*") {
      items.push([]);
    } else {
      items[items.length - 1].push(n);
    }
  }
  return (
    <>
      {items.map((item, i) =>
        item.length === 0 ? null : <li key={`${keyPrefix}-li-${i}`}>{renderNodes(item, `${keyPrefix}-${i}`)}</li>,
      )}
    </>
  );
}

/** 工坊描述 BBCode → React 节点。父容器记得保留 whitespace-pre-wrap（换行语义） */
export function renderBbcode(input: string): ReactNode[] {
  return renderNodes(parse(input), "bb");
}
