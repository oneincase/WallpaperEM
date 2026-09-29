# 第三方插件模板（发布到 WallpaperEM 插件市场）

三样东西：一份清单、一个零依赖校验脚本、一条 CI。复制过去改个名字就能发布。

## 1. 复制模板

```text
你的插件仓库/
├── wem-plugin.json                        ← 复制自本目录，改里面的字段
├── scripts/validate-wem-plugin.mjs        ← 复制自本目录 scripts/
├── protocol-cases.json                    ← 复制自本目录（可选，给 --self-test 用）
└── .github/workflows/validate-wem-plugin.yml  ← 复制自本目录 .github/
```

## 2. 改 `wem-plugin.json`

必填只有 `name` 与入口。字段表、能力矩阵、规则与错误码见
[`docs/plugin-protocol.md`](../plugin-protocol.md)；编辑器里可以挂
[`docs/wem-plugin.schema.json`](../wem-plugin.schema.json) 做补全与即时校验。

两种入口：

```jsonc
// 打开一个地址（外部站点/网页应用）
"capabilities": ["open-url"],
"entry": { "type": "url", "url": "https://example.com/", "open": "external" }

// 往 DeepSeek Harness 的 profile 装插件包（特权能力，用户会被要求确认）
"capabilities": ["dsh-profile"],
"entry": { "type": "dsh", "packages": ["dsh-plugin-wallpaper-engine"] }
```

本地先自查：

```sh
node scripts/validate-wem-plugin.mjs wem-plugin.json
node scripts/validate-wem-plugin.mjs --self-test   # 确认脚本本身没被改坏
```

## 3. 打话题

在仓库页右上角 **Topics** 加上 `wem-plugin`。WallpaperEM 的「插件 → 插件市场」
就是按这个话题搜 GitHub 的，搜到之后用户点「安装」即把清单落到本机。

## CI 会替你挡什么

`validate-wem-plugin.yml` 会用同一个脚本校验清单：字段类型、入口协议、能力白名单、
包名白名单、`minAppVersion` 格式、保留 id…… 与宿主实现（`src-tauri/src/plugin/store.rs`）
共用一份用例（`protocol-cases.json`），所以「本地过了、装的时候被拒」这类事不会发生。

## 提醒

- 清单只描述**打开什么**，不要在描述里承诺应用没实现的能力（声明了会直接被拒）。
- 图标用单个 emoji，应用不会下载任何插件资源。
- `homepage` 是 id 冲突判定里的来源指纹：换了仓库地址就等于换了来源，同名 id 会冲突。
