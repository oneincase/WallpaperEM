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
    // 必须先于 tauri_build::build()：它会校验 resources 里引用的文件是否存在
    //（Linux 的 bundled/libsteam_api.so 就是由这里生成的）
    copy_steam_api_dylib();
    embed_dev_info_plist();
    tauri_build::build();
    // 图标/配置变更时强制重跑 build.rs，否则 cargo 不会因 icon 文件变化而重新嵌入，
    // 导致 Dock/托盘图标仍是旧的（仅重启不生效）。
    println!("cargo:rerun-if-changed=icons");
    println!("cargo:rerun-if-changed=tauri.conf.json");
    println!("cargo:rerun-if-changed=capabilities");
    println!("cargo:rerun-if-changed=bin-info.plist");
}

/// 把 bin-info.plist 内嵌进可执行文件的 `__TEXT __info_plist` 段（仅 macOS bin 目标）。
///
/// `tauri dev` / 裸 `cargo build` 产出的是没有 App bundle 的裸二进制 —— 没有
/// Info.plist 时 TCC 弹不出归属明确的授权框，麦克风/系统音频权限永远拿不到，
/// CoreAudio 进程 Tap 只会静默送全零样本。内嵌后系统授权提示正常弹出。
/// 打包版（tauri build）不走这里：bundler 会把 src-tauri/Info.plist 合并进
/// bundle 的 Info.plist（多嵌一段也无害，这里用 -arg-bins 只对 bin 生效，干净）。
///
/// 不用 .cargo/config.toml 的 rustflags：那会应用到包括依赖 build script 在内的
/// 一切链接（它们的 cwd 不在包目录，相对路径直接炸），也无法拿到 CARGO_MANIFEST_DIR。
fn embed_dev_info_plist() {
    let triple = std::env::var("TARGET").unwrap_or_default();
    if !triple.contains("darwin") {
        return;
    }
    let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
    let plist = std::path::Path::new(&manifest).join("bin-info.plist");
    if !plist.is_file() {
        println!("cargo:warning=缺少 bin-info.plist，dev 二进制将无法弹出音频授权框");
        return;
    }
    for arg in [
        "-sectcreate".to_string(),
        "__TEXT".to_string(),
        "__info_plist".to_string(),
        plist.to_string_lossy().into_owned(),
    ] {
        println!("cargo:rustc-link-arg-bins={arg}");
    }
}

/// 把 Steam API 动态库复制到可执行文件旁边。
///
/// vendored 的 `libsteam_api.dylib` install name 是
/// `@loader_path/libsteam_api.dylib`（@loader_path = 主程序所在目录），
/// 所以运行时必须能在主程序同目录找到它：
/// - dev/`cargo build`：这里复制到位（<target>/<profile>/）
/// - macOS 打包：tauri.conf.json 的 bundle.macOS.files 把 sdk/libsteam_api.dylib
///   送进 .app 的 Contents/MacOS
/// - Windows 打包：tauri.windows.conf.json 的 resources（资源目录 = exe 同级）
/// - Linux 打包：resources + build.rs 加的 rpath（/usr/lib/<productName>）
fn copy_steam_api_dylib() {
    let triple = std::env::var("TARGET").unwrap_or_default();
    // Steam SDK 没有 ARM64 版的 Windows 库：该目标不编译工坊上传，也就无需复制
    if triple.contains("windows") && triple.contains("aarch64") {
        return;
    }
    let (subdir, names): (&str, &[&str]) = if triple.contains("windows") {
        ("win64", &["steam_api64.dll"])
    } else if triple.contains("darwin") {
        ("osx", &["libsteam_api.dylib"])
    } else if triple.contains("linux") {
        // SDK 的原生命名：x64 = linux64，ARM64 = linuxarm64
        if triple.contains("aarch64") {
            ("linuxarm64", &["libsteam_api.so"])
        } else {
            ("linux64", &["libsteam_api.so"])
        }
    } else {
        return;
    };

    // 源：优先仓库内置的 sdk/（Steamworks SDK redistributables，随应用分发），
    // 其次 STEAM_SDK_LOCATION（与 steamworks-sys 同名的环境变量约定），
    // 最后在 cargo registry 缓存里找 steamworks-sys vendored 的 redistributable_bin
    let candidates = |sub: &str| -> Vec<std::path::PathBuf> {
        let mut roots =
            vec![std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("sdk").join(sub)];
        if let Ok(sdk) = std::env::var("STEAM_SDK_LOCATION") {
            roots.push(std::path::PathBuf::from(sdk).join("redistributable_bin").join(sub));
        }
        let cargo_home = std::env::var("CARGO_HOME")
            .map(std::path::PathBuf::from)
            .or_else(|_| dirs::home_dir().map(|h| h.join(".cargo")).ok_or(()))
            .unwrap_or_default();
        let registry = cargo_home.join("registry").join("src");
        if let Ok(entries) = std::fs::read_dir(&registry) {
            for e in entries.flatten() {
                let pattern = e.path().join("steamworks-sys-*");
                if let Ok(matches) = glob::glob(&pattern.to_string_lossy()) {
                    for m in matches.flatten() {
                        roots.push(
                            m.join("lib").join("steam").join("redistributable_bin").join(sub),
                        );
                    }
                }
            }
        }
        roots
    };

    let out_dir = std::path::PathBuf::from(
        std::env::var("OUT_DIR").expect("build.rs 的 OUT_DIR 必然存在"),
    );
    // OUT_DIR = <target>/<profile>/build/<pkg>-<hash>/out → 上三级是 <target>/<profile>，
    // 即可执行文件所在目录（cargo 的约定布局）
    let bin_dir = out_dir
        .ancestors()
        .nth(3)
        .map(|p| p.to_path_buf())
        .expect("OUT_DIR 层级异常");

    for name in names {
        let mut copied = false;
        for dir in candidates(subdir) {
            let src = dir.join(name);
            if src.is_file() {
                let dest = bin_dir.join(name);
                match std::fs::copy(&src, &dest) {
                    Ok(_) => {
                        copied = true;
                        println!("cargo:rerun-if-changed={}", src.display());
                        break;
                    }
                    Err(e) => eprintln!("warning: 复制 {name} 失败（{src:?}）: {e}"),
                }
            }
        }
        if !copied {
            println!(
                "cargo:warning=未找到 Steam API 动态库 {name}（工坊上传功能运行时需要它）"
            );
        }
    }

    // Linux：把当前目标架构的库同步到 bundled/ 暂存目录，供打包资源引用
    // （tauri.linux.conf.json 的 resources 指向它）。x64 与 ARM64 的构建共用同一份
    // 配置，资源文件必须在各自构建时按目标架构生成。
    if triple.contains("linux") {
        if let Some(src) = candidates(subdir)
            .iter()
            .map(|d| d.join(names[0]))
            .find(|p| p.is_file())
        {
            let bundled_dir =
                std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("bundled");
            let _ = std::fs::create_dir_all(&bundled_dir);
            let dest = bundled_dir.join(names[0]);
            if let Err(e) = std::fs::copy(&src, &dest) {
                eprintln!("warning: 复制 {} 到 bundled/ 失败: {e}", names[0]);
            }
        }
    }

    // Linux：动态库查找不认 @loader_path（ELF 用 SONAME + rpath）。rpath 覆盖
    // dev（二进制同目录）与 deb 打包（资源目录 /usr/lib/<productName>）
    if triple.contains("linux") {
        println!("cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN");
        println!("cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN/../lib/WallpaperEM");
    }
}
