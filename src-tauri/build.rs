fn main() {
    tauri_build::build();
    // 图标/配置变更时强制重跑 build.rs，否则 cargo 不会因 icon 文件变化而重新嵌入，
    // 导致 Dock/托盘图标仍是旧的（仅重启不生效）。
    println!("cargo:rerun-if-changed=icons");
    println!("cargo:rerun-if-changed=tauri.conf.json");
    println!("cargo:rerun-if-changed=capabilities");
}
