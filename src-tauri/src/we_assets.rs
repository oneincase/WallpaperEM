//! Wallpaper Engine 官方素材树（`assets/`）探测与索引。
//!
//! webwallgl 1.4.1 的 local-assets 通路：工坊壁纸的效果链 / 粒子 / 渐变会按名
//! 引用 WE 安装目录自带的公共贴图（`materials/util/*`、`materials/particle/**`、
//! `materials/gradient/*` …），这些字节不在壁纸 pkg 里，库默认用程序化复刻顶上
//!（观感近似但逐像素对不上）。本机装了 Wallpaper Engine 时，内容服务器把官方
//! assets 树喂给渲染器（`/api/local-assets`），渲染与官方对齐；没有素材时库
//! 静默回落到程序化复刻，行为与未开启完全一致。
//!
//! 素材根的判定与上游 bench（host/wallpaper-host.ts）同契约：候选根下必须有
//! `materials/` 子目录才算数。

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// 素材名索引缓存：根路径 →（引擎名列表，生成时刻）。
/// 上游 bench 实测约 586 个文件，不值得每次挂载 / 每次探测都重扫。
static INDEX_CACHE: Mutex<Option<(PathBuf, Vec<String>, Instant)>> = Mutex::new(None);
const INDEX_TTL: Duration = Duration::from_secs(60);

/// 用户自定义路径（设置 `wallpaper_we_assets_dir`）有效时，作为最高优先级候选。
/// 语义与 `WE_LOCAL_ASSETS` 一致：指向 assets 根本身（其下直接有 materials/）。
pub fn candidates(custom_dir: Option<&str>) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    if let Some(dir) = custom_dir.map(str::trim).filter(|s| !s.is_empty()) {
        out.push(PathBuf::from(dir));
    }
    for lib in steam_library_roots() {
        out.push(lib.join("steamapps/common/wallpaper_engine/assets"));
    }
    // Steam 多库 / 不同安装位置可能指向同一目录，按路径去重
    out.dedup();
    out
}

/// 第一个含 `materials/` 子目录的候选根；都没有则 None（库侧走程序化复刻）。
pub fn resolve_root(custom_dir: Option<&str>) -> Option<PathBuf> {
    candidates(custom_dir)
        .into_iter()
        .find(|p| p.join("materials").is_dir())
}

/// 递归列 `materials/**/*.tex` 的引擎名（相对 materials 去掉扩展名，`/` 分隔）。
/// 目录不存在 / 不可读返回空表；结果按 TTL 缓存。
pub fn material_names(root: &Path) -> Vec<String> {
    {
        let guard = INDEX_CACHE.lock().unwrap();
        if let Some((cached_root, names, at)) = guard.as_ref() {
            if cached_root == root && at.elapsed() < INDEX_TTL {
                return names.clone();
            }
        }
    }
    let base = root.join("materials");
    let mut names = Vec::new();
    walk_tex(&base, &base, &mut names);
    names.sort();
    if let Ok(mut guard) = INDEX_CACHE.lock() {
        *guard = Some((root.to_path_buf(), names.clone(), Instant::now()));
    }
    names
}

fn walk_tex(dir: &Path, base: &Path, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_dir() {
            walk_tex(&path, base, out);
        } else if ft.is_file() {
            if path.extension().and_then(|e| e.to_str()) == Some("tex") {
                if let Ok(rel) = path.strip_prefix(base) {
                    out.push(rel.with_extension("").to_string_lossy().replace('\\', "/"));
                }
            }
        }
    }
}

// ---------- Steam 库路径探测 ----------

/// 各平台的 Steam 安装根（其下有 `steamapps/`）。只做零依赖的常见路径探测：
/// 多库通过解析每个安装根的 `steamapps/libraryfolders.vdf` 补齐。
fn steam_install_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    let push = |roots: &mut Vec<PathBuf>, p: PathBuf| {
        if !roots.contains(&p) {
            roots.push(p);
        }
    };

    #[cfg(target_os = "macos")]
    {
        if let Some(home) = home_dir() {
            push(&mut roots, home.join("Library/Application Support/Steam"));
        }
    }

    #[cfg(target_os = "windows")]
    {
        for var in ["PROGRAMFILES(X86)", "PROGRAMFILES"] {
            if let Ok(pf) = std::env::var(var) {
                let p = PathBuf::from(pf).join("Steam");
                if p.is_dir() {
                    push(&mut roots, p);
                }
            }
        }
        // 兜底：系统盘默认安装位置（环境变量被改时）
        for drive in ["C:\\Program Files (x86)\\Steam", "C:\\Program Files\\Steam"] {
            let p = PathBuf::from(drive);
            if p.is_dir() {
                push(&mut roots, p);
            }
        }
    }

    #[cfg(target_os = "linux")]
    {
        if let Some(home) = home_dir() {
            for rel in [
                ".local/share/Steam",
                ".steam/steam",
                ".var/app/com.valvesoftware.Steam/data/Steam", // Flatpak
            ] {
                let p = home.join(rel);
                if p.is_dir() {
                    push(&mut roots, p);
                }
            }
        }
    }

    roots
}

/// Steam 库根列表（安装目录 + libraryfolders.vdf 里登记的其它库）。
fn steam_library_roots() -> Vec<PathBuf> {
    let mut roots = steam_install_roots();
    let mut extra: Vec<PathBuf> = Vec::new();
    for install in &roots {
        if let Some(paths) = parse_library_folders(&install.join("steamapps/libraryfolders.vdf"))
        {
            for p in paths {
                if !roots.contains(&p) && !extra.contains(&p) {
                    extra.push(p);
                }
            }
        }
    }
    roots.append(&mut extra);
    roots
}

/// 解析 libraryfolders.vdf 里的 `"path" "...""` 行（vdf 里 Windows 反斜杠是转义双写）。
fn parse_library_folders(vdf: &Path) -> Option<Vec<PathBuf>> {
    let text = std::fs::read_to_string(vdf).ok()?;
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        // 形如 `"path"  "D:\\SteamLibrary"`：取第一段引号内容作键，第二段作值
        let quoted: Vec<&str> = line.split('"').collect();
        if quoted.len() < 4 || quoted[1] != "path" {
            continue;
        }
        let value = quoted[3].replace("\\\\", "\\").replace("\\\"", "\"");
        let p = PathBuf::from(value);
        // 不在此处过滤不存在的路径：解析只管忠实还原 vdf 登记项，
        // 库是否有效交给候选探测（materials/ 是否存在）决定
        if !out.contains(&p) {
            out.push(p);
        }
    }
    Some(out)
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_vdf_paths() {
        let tmp = std::env::temp_dir().join(format!("wallpaperem-lf-{:x}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let vdf = tmp.join("libraryfolders.vdf");
        std::fs::write(
            &vdf,
            r#"
"libraryfolders"
{
	"0"
	{
		"path"		"C:\\Program Files (x86)\\Steam"
	}
	"1"
	{
		"path"		"D:\\SteamLibrary"
		"label"		""
	}
}
"#,
        )
        .unwrap();
        let paths = parse_library_folders(&vdf).unwrap();
        assert!(paths.contains(&PathBuf::from("D:\\SteamLibrary")));
        // 用字符串后缀而非 Path::ends_with：后者按路径组件匹配，而在非 Windows
        // 主机上反斜杠不是分隔符，整条 C:\...\Steam 会被当成单个组件
        assert!(paths
            .iter()
            .any(|p| p.to_string_lossy().ends_with("Steam")));
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn missing_root_resolves_none() {
        assert!(resolve_root(Some("/nonexistent/we/assets/path-xyz")).is_none());
    }

    #[test]
    fn material_names_walks_tree() {
        let tmp = std::env::temp_dir().join(format!("wallpaperem-ma-{:x}", std::process::id()));
        let mat = tmp.join("materials");
        std::fs::create_dir_all(mat.join("particle/nature")).unwrap();
        std::fs::create_dir_all(mat.join("util")).unwrap();
        std::fs::write(mat.join("util/white.tex"), b"x").unwrap();
        std::fs::write(mat.join("particle/nature/rain1.tex"), b"x").unwrap();
        std::fs::write(mat.join("particle/notex.txt"), b"x").unwrap();
        let names = material_names(&tmp);
        assert_eq!(names, vec!["particle/nature/rain1", "util/white"]);
        std::fs::remove_dir_all(&tmp).ok();
    }
}
