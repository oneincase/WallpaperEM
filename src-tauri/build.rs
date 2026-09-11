fn main() {
    // 直接 `cargo build` / `cargo check` 不会跑 tauri.conf.json 的 beforeBuildCommand
    //（只有 `pnpm tauri build` / `pnpm tauri dev` 会）。`dist/` 是 gitignore 的构建产物，
    // 被清掉之后 tauri_build 只报一句 `resource path "../dist/renderer" doesn't exist`，
    // 看不出该跑什么。这里先把该跑的命令指出来。
    let renderer = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../dist/renderer");
    if !renderer.exists() {
        println!(
            "cargo:warning=缺少前端产物 dist/renderer：先跑 `pnpm --filter @we/desktop build`；\
             用 `pnpm tauri build` / `pnpm tauri dev` 则不必（它们会自动构建前端）"
        );
    }
    tauri_build::build();
    // 图标/配置变更时强制重跑 build.rs，否则 cargo 不会因 icon 文件变化而重新嵌入，
    // 导致 Dock/托盘图标仍是旧的（仅重启不生效）。
    println!("cargo:rerun-if-changed=icons");
    println!("cargo:rerun-if-changed=tauri.conf.json");
    println!("cargo:rerun-if-changed=capabilities");
}
