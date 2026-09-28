//! 场景工程结构化体检（MCP `scene_inspect` 工具）。
//!
//! 为什么要有它：`project_validate` 只回答「能不能打包」，回答不了「现在这个场景长什么
//! 样、哪里可能不对」。agent 拿不到渲染器的逐帧反馈（诊断只留一段历史），只能靠截图
//! 判断画面 —— 而「贴图是 4 张 4K、粒子 3 万、一半素材没人用」这类问题在截图上完全看
//! 不出来，却直接决定壁纸跑不跑得动。
//!
//! 输出刻意做成**机器可读 + 可数**：图层树、每张贴图的尺寸/体积/引用者、未使用素材、
//! 属性绑定命中表、粒子的支持/不支持组件、以及一份粗粒度性能提示。校验结果（errors/
//! warnings）原样附带，省掉一次 `project_validate` 往返。
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::Path;

use serde_json::{json, Value};

use crate::workspace::{
    self, image_source_dims, is_builtin_shader, parse_pair, parse_triple, read_project_json,
    rel_display, safe_join, PARTICLE_EMITTERS, PARTICLE_INITIALIZERS, PARTICLE_OPERATORS,
    PARTICLE_RENDERERS,
};

/// 单张源图超过这个边长就提醒（显存与解码时间都随面积线性涨）
const BIG_TEXTURE_EDGE: u32 = 4096;
/// 素材总量提醒阈值
const BIG_TEXTURE_BYTES: u64 = 96 << 20;

pub fn inspect(app: &tauri::AppHandle, name: &str) -> Result<Value, String> {
    let dir = workspace::project_dir(app, name)?;
    if !dir.is_dir() {
        return Err(format!("工程不存在: {}", name.trim()));
    }
    Ok(inspect_dir(&dir, name.trim()))
}

/// 体检主体（不碰 AppHandle，便于直接对目录单测）
pub fn inspect_dir(dir: &Path, name: &str) -> Value {
    let report = workspace::validate_dir(dir, name);
    let project = read_project_json(dir).unwrap_or(Value::Null);
    let scene = std::fs::read_to_string(dir.join("scene.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok());

    // 属性表 + 绑定命中
    let props: BTreeMap<String, Value> = project
        .get("general")
        .and_then(|g| g.get("properties"))
        .and_then(|p| p.as_object())
        .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
        .unwrap_or_default();

    let empty = Vec::new();
    let objects = scene
        .as_ref()
        .and_then(|s| s.get("objects"))
        .and_then(|o| o.as_array())
        .unwrap_or(&empty);

    let mut layers: Vec<Value> = Vec::new();
    let mut textures: BTreeMap<String, TextureInfo> = BTreeMap::new();
    let mut particles: Vec<Value> = Vec::new();
    let mut referenced_materials: BTreeSet<String> = BTreeSet::new();
    let mut bound: BTreeMap<String, u64> = BTreeMap::new();
    let mut anim_summary = 0u64;

    for obj in objects {
        let label = obj
            .get("name")
            .and_then(|n| n.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "（无名图层）".into());
        let image = obj.get("image").and_then(|v| v.as_str());
        let particle = obj.get("particle").and_then(|v| v.as_str());
        let mut layer_bindings: Vec<String> = Vec::new();
        collect_bindings(obj, &label, &mut layer_bindings, &mut bound, 0);

        let mut anim = Vec::new();
        collect_animation(obj, &label, &mut anim, 0);
        if !anim.is_empty() {
            anim_summary += anim.len() as u64;
        }

        // 图层内容：模型 → 材质 → 贴图；粒子 → 预设 → 材质
        let mut mat_rel: Option<String> = None;
        if let Some(rel) = image {
            if !rel.starts_with("models/util/") {
                if let Some(m) = model_material(dir, rel) {
                    mat_rel = Some(m);
                }
            }
        }
        let mut layer_tex: Vec<String> = Vec::new();
        if let Some(m) = mat_rel.clone() {
            referenced_materials.insert(m.clone());
            for t in material_textures(dir, &m) {
                layer_tex.push(t.clone());
                let e = textures.entry(t).or_default();
                e.builtin = e.builtin || is_builtin_texture(&layer_tex[layer_tex.len() - 1]);
                e.used_by.insert(label.clone());
            }
        }
        // 效果链引用的材质（`effects/<名字>.json` 的 passes[].material）也要算"被用到"。
        // 漏了这一步，凡带效果的工程都会把 materials/effects/*.json 误报成未使用素材。
        if let Some(effs) = obj.get("effects").and_then(|e| e.as_array()) {
            for eff in effs {
                let Some(f) = eff.get("file").and_then(|f| f.as_str()) else {
                    continue;
                };
                let Some(doc) = read_json(dir, f) else { continue };
                for pass in doc.get("passes").and_then(|p| p.as_array()).into_iter().flatten() {
                    let Some(m) = pass.get("material").and_then(|m| m.as_str()) else {
                        continue;
                    };
                    referenced_materials.insert(m.to_string());
                    // 效果材质里的贴图同样要登记到 textures（"_rt_xxx" 是渲染目标，跳过）
                    if let Ok(mj) = safe_join(dir, m) {
                        if let Some(doc2) = std::fs::read_to_string(&mj)
                            .ok()
                            .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
                        {
                            for pass2 in doc2
                                .get("passes")
                                .and_then(|p| p.as_array())
                                .into_iter()
                                .flatten()
                            {
                                for t in pass2
                                    .get("textures")
                                    .and_then(|t| t.as_array())
                                    .into_iter()
                                    .flatten()
                                    .filter_map(|t| t.as_str())
                                {
                                    if t.starts_with("_rt_") {
                                        continue;
                                    }
                                    let e = textures.entry(t.to_string()).or_default();
                                    e.builtin = e.builtin || is_builtin_texture(t);
                                    e.used_by.insert(label.clone());
                                }
                            }
                        }
                    }
                }
            }
        }
        if let Some(prel) = particle {
            if let Some(p) = read_json(dir, prel) {
                let pmat = p.get("material").and_then(|m| m.as_str()).unwrap_or("");
                if !pmat.is_empty() {
                    referenced_materials.insert(pmat.to_string());
                    for t in material_textures(dir, pmat) {
                        let builtin = is_builtin_texture(&t);
                        let e = textures.entry(t).or_default();
                        e.builtin = e.builtin || builtin;
                        e.used_by.insert(label.clone());
                    }
                }
                let (mut ok, mut bad) = (0u64, Vec::<String>::new());
                for (key, list) in [
                    ("emitter", PARTICLE_EMITTERS.as_slice()),
                    ("initializer", PARTICLE_INITIALIZERS.as_slice()),
                    ("operator", PARTICLE_OPERATORS.as_slice()),
                    ("renderer", PARTICLE_RENDERERS.as_slice()),
                ] {
                    for it in p.get(key).and_then(|v| v.as_array()).unwrap_or(&empty) {
                        let Some(n) = it.get("name").and_then(|n| n.as_str()) else {
                            continue;
                        };
                        if list.iter().any(|s| *s == n.to_ascii_lowercase()) {
                            ok += 1;
                        } else {
                            bad.push(format!("{key}:{n}"));
                        }
                    }
                }
                particles.push(json!({
                    "layer": label,
                    "preset": prel,
                    "material": pmat,
                    "maxcount": p.get("maxcount").and_then(|v| v.as_i64()).unwrap_or(0),
                    "count": p.get("maxcount").and_then(|v| v.as_i64()).unwrap_or(0),
                    "componentsSupported": ok,
                    "componentsUnsupported": bad,
                    "hasControlPoints": p.get("controlpoint").and_then(|v| v.as_array()).is_some(),
                }));
            }
        }

        layers.push(json!({
            "id": obj.get("id").and_then(|v| v.as_i64()),
            "name": label,
            "kind": layer_kind(image, particle),
            "image": image,
            "particle": particle,
            "material": mat_rel,
            "textures": layer_tex,
            "origin": obj.get("origin"),
            "size": obj.get("size"),
            "scale": obj.get("scale"),
            "visible": obj.get("visible").and_then(|v| v.as_bool()).unwrap_or(true),
            "parent": obj.get("parent").and_then(|v| v.as_i64()),
            "parallaxDepth": obj.get("parallaxDepth"),
            "colorBlendMode": obj.get("colorBlendMode").and_then(|v| v.as_i64()),
            "effects": obj.get("effects").and_then(|v| v.as_array()).map(|a| {
                a.iter().filter_map(|e| e.get("file").and_then(|f| f.as_str())).collect::<Vec<_>>()
            }).unwrap_or_default(),
            "animations": anim,
            "bindings": layer_bindings,
        }));
    }

    // 贴图体检：尺寸/体积/来源
    let mut tex_rows: Vec<Value> = Vec::new();
    let mut tex_bytes: u64 = 0;
    let mut big: Vec<String> = Vec::new();
    for (tex, info) in textures.iter() {
        let row = texture_row(dir, tex, info);
        // 内置贴图没有文件，不该算进体积/超大提示
        if !info.builtin {
            tex_bytes += row.1;
            if row.2 {
                big.push(tex.clone());
            }
        }
        tex_rows.push(row.0);
    }

    let (file_count, file_bytes) = dir_totals(dir);
    let pkg = pkg_info(dir);
    let unused = unused_materials(dir, &referenced_materials, &textures);
    let declared: Vec<String> = props.keys().cloned().collect();
    let unbound: Vec<String> = declared
        .iter()
        .filter(|k| !bound.contains_key(*k) && !workspace::unbound_ok_prop(k))
        .cloned()
        .collect();

    let mut notes: Vec<String> = Vec::new();
    if !big.is_empty() {
        notes.push(format!(
            "{} 张贴图边长超过 {BIG_TEXTURE_EDGE}（{}）—— 显存与首次解码时间随面积线性涨",
            big.len(),
            big.join(" / ")
        ));
    }
    if tex_bytes > BIG_TEXTURE_BYTES {
        notes.push(format!(
            "素材总量 {:.1} MB 偏大，场景壁纸冷启动会明显变慢",
            tex_bytes as f64 / 1048576.0
        ));
    }
    let particle_total: i64 = particles
        .iter()
        .map(|p| p.get("count").and_then(|v| v.as_i64()).unwrap_or(0))
        .sum();
    if particle_total > 2000 {
        notes.push(format!("粒子总数 {particle_total}，中低端机会掉帧"));
    }
    if !unused.is_empty() {
        notes.push(format!(
            "有 {} 份素材没有任何图层引用（会在包里白占体积）：{}",
            unused.len(),
            unused.join(" / ")
        ));
    }

    json!({
        "project": name,
        "type": report.get("type").cloned().unwrap_or(json!("unknown")),
        "title": report.get("title").cloned().unwrap_or(json!("")),
        "dir": dir.to_string_lossy(),
        "files": { "count": file_count, "bytes": file_bytes },
        "pkg": pkg,
        "design": scene
            .as_ref()
            .and_then(|s| s.get("general"))
            .and_then(|g| g.get("orthogonalprojection"))
            .map(|o| json!({
                "width": o.get("width").and_then(|v| v.as_f64()).unwrap_or(0.0),
                "height": o.get("height").and_then(|v| v.as_f64()).unwrap_or(0.0),
            })),
        "layerCount": layers.len(),
        "layers": layers,
        "textures": tex_rows,
        "textureBytes": tex_bytes,
        "unusedAssets": unused,
        "properties": { "declared": declared, "bound": bound, "unbound": unbound },
        "particles": particles,
        "shaders": shader_set(dir),
        "animationCount": anim_summary,
        "perf": {
            "notes": notes,
            "particleTotal": particle_total,
        },
        "errors": report.get("errors").cloned().unwrap_or(json!([])),
        "warnings": report.get("warnings").cloned().unwrap_or(json!([])),
        "ok": report.get("ok").cloned().unwrap_or(json!(false)),
    })
}

#[derive(Default)]
struct TextureInfo {
    used_by: BTreeSet<String>,
    /// 只出现在材质里但工程里既没有 .tex 也没有源图
    missing: bool,
    /// 内置贴图（`util/` `particle/` `_rt_`）：由渲染库程序化生成，工程里没有文件
    builtin: bool,
}

/// 内置贴图（不需要工程里带文件）：与校验器 `check_model_chain` 的跳过口径一致
fn is_builtin_texture(name: &str) -> bool {
    name.starts_with("util/") || name.starts_with("particle/") || name.starts_with("_rt_")
}

/// 图层类型：跟渲染器的分支口径一致（内置模型 → 纯色/分组/全屏后期）
fn layer_kind(image: Option<&str>, particle: Option<&str>) -> &'static str {
    if particle.is_some() {
        return "particle";
    }
    match image {
        Some(p) if p.ends_with("solidlayer.json") => "solid",
        Some(p) if p.ends_with("composelayer.json") => "compose",
        Some(p) if p.ends_with("projectlayer.json") || p.ends_with("fullscreenlayer.json") => {
            "fullscreen-effect"
        }
        Some(_) => "image",
        None => "empty",
    }
}

fn read_json(dir: &Path, rel: &str) -> Option<Value> {
    let p = safe_join(dir, rel).ok()?;
    std::fs::read_to_string(p)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
}

/// 模型 → 材质相对路径
fn model_material(dir: &Path, model_rel: &str) -> Option<String> {
    read_json(dir, model_rel)?
        .get("material")
        .and_then(|m| m.as_str())
        .map(|s| s.to_string())
}

/// 材质 → 贴图名列表（跳过内置 util/ 与 _rt_）
fn material_textures(dir: &Path, mat_rel: &str) -> Vec<String> {
    let Some(mat) = read_json(dir, mat_rel) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for pass in mat.get("passes").and_then(|p| p.as_array()).unwrap_or(&Vec::new()) {
        for t in pass.get("textures").and_then(|t| t.as_array()).unwrap_or(&Vec::new()) {
            if let Some(name) = t.as_str() {
                if name.is_empty() || name.starts_with("util/") || name.starts_with("_rt_") {
                    continue;
                }
                if !out.contains(&name.to_string()) {
                    out.push(name.to_string());
                }
            }
        }
    }
    out
}

/// 贴图一行：(json, 字节数, 是否超大)
fn texture_row(dir: &Path, name: &str, info: &TextureInfo) -> (Value, u64, bool) {
    if info.builtin {
        return (
            json!({
                "name": name,
                "source": Value::Null,
                "bytes": 0,
                "width": Value::Null,
                "height": Value::Null,
                "handWrittenTex": false,
                "usedBy": info.used_by.iter().cloned().collect::<Vec<_>>(),
                "missing": false,
                "builtin": true,
            }),
            0,
            false,
        );
    }
    let mut source = None;
    let mut bytes = 0u64;
    let mut dims: Option<(u32, u32)> = None;
    let mut has_tex = false;
    for ext in ["png", "jpg", "jpeg"] {
        let rel = format!("materials/{name}.{ext}");
        if let Ok(p) = safe_join(dir, &rel) {
            if p.is_file() {
                bytes = p.metadata().map(|m| m.len()).unwrap_or(0);
                source = Some(rel.clone());
                dims = image_source_dims(dir, name).ok().map(|(_, w, h)| (w, h));
                break;
            }
        }
    }
    if let Ok(p) = safe_join(dir, &format!("materials/{name}.tex")) {
        if p.is_file() {
            has_tex = true;
            if bytes == 0 {
                bytes = p.metadata().map(|m| m.len()).unwrap_or(0);
                source = Some(format!("materials/{name}.tex"));
            }
        }
    }
    let big = dims.map(|(w, h)| w.max(h) > BIG_TEXTURE_EDGE).unwrap_or(false);
    (
        json!({
            "name": name,
            "source": source,
            "bytes": bytes,
            "width": dims.map(|d| d.0),
            "height": dims.map(|d| d.1),
            "handWrittenTex": has_tex,
            "usedBy": info.used_by.iter().cloned().collect::<Vec<_>>(),
            "missing": info.missing,
            "builtin": false,
        }),
        bytes,
        big,
    )
}

/// materials/ 下没有被任何材质/预设引用的素材（启发式，只提示不阻断）
fn unused_materials(
    dir: &Path,
    referenced_materials: &BTreeSet<String>,
    textures: &BTreeMap<String, TextureInfo>,
) -> Vec<String> {
    let mut out = Vec::new();
    let root = dir.join("materials");
    let mut stack = vec![root.clone()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                stack.push(p);
                continue;
            }
            let rel = rel_display(dir, &p);
            let ext = p
                .extension()
                .and_then(|x| x.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            let used = match ext.as_str() {
                // 材质/预设本身
                "json" => referenced_materials.contains(&rel),
                // 贴图源：名字要在 textures 表里（.tex 与源图同名算同一份）
                _ => {
                    let name = rel
                        .trim_start_matches("materials/")
                        .rsplit_once('.')
                        .map(|(stem, _)| stem.to_string())
                        .unwrap_or_default();
                    textures.get(&name).map(|t| !t.builtin).unwrap_or(false)
                }
            };
            if !used {
                out.push(rel);
            }
        }
    }
    out.sort();
    out
}

fn dir_totals(dir: &Path) -> (u64, u64) {
    let mut count = 0u64;
    let mut bytes = 0u64;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with('.') {
                continue;
            }
            let p = e.path();
            if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                stack.push(p);
            } else {
                count += 1;
                bytes += e.metadata().map(|m| m.len()).unwrap_or(0);
            }
        }
    }
    (count, bytes)
}

fn pkg_info(dir: &Path) -> Value {
    let p = dir.join("scene.pkg");
    match p.metadata() {
        Ok(m) => json!({
            "exists": true,
            "bytes": m.len(),
            "stale": is_stale(dir, &m),
        }),
        Err(_) => json!({ "exists": false, "bytes": 0, "stale": true }),
    }
}

/// 包比任何源文件旧 = 该重新 `scene_pack` 了（write_project_file 会直接删包，
/// 这里兜住「手改工程目录」的情况）
fn is_stale(dir: &Path, pkg: &std::fs::Metadata) -> bool {
    let Ok(pkg_t) = pkg.modified() else {
        return false;
    };
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with('.') || name == "scene.pkg" || name.starts_with("preview.") {
                continue;
            }
            let p = e.path();
            if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                stack.push(p);
                continue;
            }
            if let Ok(t) = e.metadata().and_then(|m| m.modified()) {
                if t > pkg_t {
                    return true;
                }
            }
        }
    }
    false
}

/// 收集 `{"user": "..."}` 绑定（`out` 是「字段路径→属性名」的人类可读串）
fn collect_bindings(
    node: &Value,
    path: &str,
    out: &mut Vec<String>,
    bound: &mut BTreeMap<String, u64>,
    depth: usize,
) {
    if depth > 10 {
        return;
    }
    match node {
        Value::Object(map) => {
            if let Some(u) = map.get("user").and_then(|v| v.as_str()) {
                out.push(format!("{path} → {u}"));
                *bound.entry(u.to_string()).or_insert(0) += 1;
            }
            for (k, v) in map {
                collect_bindings(v, &format!("{path}.{k}"), out, bound, depth + 1);
            }
        }
        Value::Array(arr) => {
            for (i, v) in arr.iter().enumerate() {
                collect_bindings(v, &format!("{path}[{i}]"), out, bound, depth + 1);
            }
        }
        _ => {}
    }
}

/// 收集关键帧动画摘要
fn collect_animation(node: &Value, path: &str, out: &mut Vec<Value>, depth: usize) {
    if depth > 10 {
        return;
    }
    match node {
        Value::Object(map) => {
            if let Some(anim) = map.get("animation") {
                let frames = anim
                    .get("c0")
                    .and_then(|c| c.as_array())
                    .map(|a| a.len())
                    .unwrap_or(0);
                let opts = anim.get("options");
                out.push(json!({
                    "field": path,
                    "keyframes": frames,
                    "fps": opts.and_then(|o| o.get("fps")).cloned(),
                    "length": opts.and_then(|o| o.get("length")).cloned(),
                    "mode": opts.and_then(|o| o.get("mode")).cloned(),
                }));
            }
            for (k, v) in map {
                collect_animation(v, &format!("{path}.{k}"), out, depth + 1);
            }
        }
        Value::Array(arr) => {
            for (i, v) in arr.iter().enumerate() {
                collect_animation(v, &format!("{path}[{i}]"), out, depth + 1);
            }
        }
        _ => {}
    }
}

/// 渲染库里以 `switch (name)` 分派的两族名字（守卫测试断言它们都是 `case '<名>':`）
#[cfg(test)]
pub fn switch_case_names() -> Vec<&'static str> {
    let mut v: Vec<&'static str> = Vec::new();
    v.extend(PARTICLE_INITIALIZERS);
    v.extend(PARTICLE_OPERATORS);
    v
}

/// 以「字面量比较」分派的家族（发射器特判、尾迹渲染器）
#[cfg(test)]
pub fn literal_names() -> Vec<&'static str> {
    let mut v: Vec<&'static str> = Vec::new();
    v.extend(PARTICLE_EMITTERS);
    v.extend(PARTICLE_RENDERERS);
    v
}

/// 未使用（但合法）的解析辅助，避免 `parse_pair/parse_triple` 只在测试里用
#[allow(dead_code)]
fn _keep_helpers(v: &Value) -> bool {
    parse_pair(v).is_some() || parse_triple(v).is_some()
}

/// `is_builtin_shader` 的转发（材质体检里要判断「这个 pass 会不会被跳过」）
#[allow(dead_code)]
pub(crate) fn uses_builtin_shader(name: &str) -> bool {
    is_builtin_shader(name)
}

/// 材料表里的所有 shader 名（供工具输出「这个工程的 shader 覆盖面」）
pub fn material_shaders(dir: &Path, mat_rel: &str) -> Vec<String> {
    read_json(dir, mat_rel)
        .and_then(|m| m.get("passes").and_then(|p| p.as_array()).cloned())
        .map(|passes| {
            passes
                .iter()
                .filter_map(|p| p.get("shader").and_then(|s| s.as_str()).map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

/// 去重后的 shader 集合（给 `scene_inspect` 输出用）
pub fn shader_set(dir: &Path) -> Vec<String> {
    let mut set: HashSet<String> = HashSet::new();
    let mut stack = vec![dir.join("materials"), dir.join("effects")];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                stack.push(p);
                continue;
            }
            if p.extension().and_then(|x| x.to_str()) != Some("json") {
                continue;
            }
            for s in material_shaders(dir, &rel_display(dir, &p)) {
                set.insert(s);
            }
        }
    }
    let mut v: Vec<String> = set.into_iter().collect();
    v.sort();
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("wpem-inspect-{tag}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// 效果链引用的材质不能被当成"未使用素材"（回归：漏登记时带效果的工程一律误报）
    #[test]
    fn effect_chain_materials_are_not_unused() {
        let dir = tmpdir("effects");
        std::fs::create_dir_all(dir.join("materials/effects")).unwrap();
        std::fs::create_dir_all(dir.join("effects")).unwrap();
        std::fs::create_dir_all(dir.join("models")).unwrap();
        std::fs::create_dir_all(dir.join("shaders/effects")).unwrap();
        std::fs::write(
            dir.join("materials/effects/aura.json"),
            r#"{"passes":[{"shader":"effects/aura","textures":["util/noise"]}]}"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("effects/aura.json"),
            r#"{"passes":[{"material":"materials/effects/aura.json"}]}"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("scene.json"),
            r#"{"general":{},"objects":[
                 {"id":1,"name":"A","image":"models/util/solidlayer.json",
                  "effects":[{"file":"effects/aura.json","name":"a","visible":true}]}]}"#,
        )
        .unwrap();

        let mut referenced: BTreeSet<String> = BTreeSet::new();
        let textures: BTreeMap<String, TextureInfo> = BTreeMap::new();
        // 手工模拟装配期的登记（真实路径见 inspect_dir：读 effects 文档里的 material）
        let doc = read_json(&dir, "effects/aura.json").unwrap();
        for pass in doc["passes"].as_array().unwrap() {
            referenced.insert(pass["material"].as_str().unwrap().to_string());
        }
        let unused = unused_materials(&dir, &referenced, &textures);
        assert!(
            !unused.iter().any(|u| u.contains("effects/aura.json")),
            "被效果链引用的材质误报未使用: {unused:?}"
        );

        // 反证：没被任何地方引用的材质仍要报出来
        std::fs::write(dir.join("materials/orphan.json"), "{}").unwrap();
        let unused2 = unused_materials(&dir, &referenced, &textures);
        assert!(unused2.iter().any(|u| u == "materials/orphan.json"), "{unused2:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
