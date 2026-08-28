//! 工坊服务：搜索/详情 + TTL 缓存 + DB upsert（对齐 Web 版 modules/workshop/service.ts）
//!
//! 注意：DB 连接以 Arc<Mutex<Connection>> 持有，网络 await 期间绝不持锁。

use rusqlite::Connection;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::db;
use crate::steam::browse::{browse_workshop_raw, BrowseQuery, BrowseRawItem};
use crate::steam::details::get_item_details;
use crate::steam::types::{
    WorkshopItem, WorkshopItemSummary, WorkshopSearchParams, WorkshopSearchResult,
};
use crate::steam::SteamClient;

const SEARCH_TTL: Duration = Duration::from_secs(5 * 60);
const DETAIL_TTL_MS: i64 = 24 * 3600 * 1000;

const TYPE_TAG: [(&str, &str); 5] = [
    ("video", "Video"),
    ("scene", "Scene"),
    ("web", "Web"),
    ("gif", "GIF"),
    ("application", "Application"),
];

pub struct WorkshopService {
    client: SteamClient,
    db: Arc<Mutex<Connection>>,
    cache: Mutex<HashMap<String, (Instant, serde_json::Value)>>,
    /// 工作友好（默认开启）：开启后过滤成人 / NSFW 内容（Arc 内部可变，支持运行时切换）
    family_friendly: AtomicBool,
}

impl WorkshopService {
    pub fn new(client: SteamClient, db: Arc<Mutex<Connection>>) -> Self {
        let family_friendly = db::get_setting(&db.lock().unwrap(), "family_friendly")
            .map(|v| v != "false" && v != "0")
            .unwrap_or(true);
        Self {
            client,
            db,
            cache: Mutex::new(HashMap::new()),
            family_friendly: AtomicBool::new(family_friendly),
        }
    }

    /// 列表摘要是否应被过滤（工作友好开启 + 命中成人特征）
    fn is_adult_summary(&self, it: &WorkshopItemSummary) -> bool {
        if !self.family_friendly.load(Ordering::Relaxed) {
            return false;
        }
        crate::sfw::is_adult_text(&it.title) || crate::sfw::is_adult_tags(&it.tags)
    }

    fn cache_get(&self, key: &str, ttl: Duration) -> Option<serde_json::Value> {
        let map = self.cache.lock().unwrap();
        map.get(key)
            .filter(|(at, _)| at.elapsed() < ttl)
            .map(|(_, v)| v.clone())
    }

    fn cache_set(&self, key: &str, value: &impl serde::Serialize) {
        if let Ok(v) = serde_json::to_value(value) {
            let mut map = self.cache.lock().unwrap();
            map.insert(key.to_string(), (Instant::now(), v));
            if map.len() > 300 {
                map.retain(|_, (at, _)| at.elapsed() < Duration::from_secs(30 * 60));
            }
        }
    }

    /// 搜索/筛选工坊列表（SSR 解析 + 详情批量补齐 + 二次过滤 + 5min 缓存）
    pub async fn search(&self, params: WorkshopSearchParams) -> Result<WorkshopSearchResult, String> {
        let key = format!("search:{params:?}");
        if let Some(v) = self.cache_get(&key, SEARCH_TTL) {
            return serde_json::from_value(v).map_err(|e| e.to_string());
        }

        let mut required_tags: Vec<String> = Vec::new();
        if let Some(t) = params.r#type.as_deref() {
            if !t.is_empty() && t != "unknown" {
                if let Some(tag) = TYPE_TAG.iter().find(|(k, _)| *k == t).map(|(_, v)| *v) {
                    required_tags.push(tag.to_string());
                }
            }
        }
        if let Some(tag) = params.tag.as_deref() {
            if !tag.is_empty() {
                required_tags.push(tag.to_string());
            }
        }

        let raw = browse_workshop_raw(
            &self.client,
            &BrowseQuery {
                query: params.query.clone(),
                sort: params.sort.clone(),
                page: params.page,
                required_tags,
            },
        )
        .await?;

        let items = self.enrich(&raw.items).await?;

        // 工作友好（默认开启）：自动过滤成人内容
        let items = items
            .into_iter()
            .filter(|i| !self.is_adult_summary(i))
            .collect::<Vec<_>>();

        // 服务端 requiredtags 可能不完整生效，本地二次过滤兜底
        let items = items
            .into_iter()
            .filter(|i| {
                let ok_type = match params.r#type.as_deref() {
                    Some(t) if !t.is_empty() => i.r#type == t,
                    _ => true,
                };
                let ok_tag = match params.tag.as_deref() {
                    Some(t) if !t.is_empty() => i.tags.iter().any(|x| x == t),
                    _ => true,
                };
                ok_type && ok_tag
            })
            .collect::<Vec<_>>();

        // 本地排序（Steam SSR 只对 totaluniquesubscribers 真正服务端排序，其余用元数据补齐）
        let items = Self::sort_items(items, params.sort.as_deref());

        let page = params.page.unwrap_or(1);
        let has_more = if raw.total > 0 {
            page < 1000 && (page as usize) * 30 < raw.total
        } else {
            raw.has_more
        };
        let has_more = has_more && !items.is_empty();

        let result = WorkshopSearchResult {
            items,
            total: raw.total,
            page,
            page_size: raw.items.len(),
            has_more,
        };
        self.cache_set(&key, &result);
        Ok(result)
    }

    /// 随机壁纸推荐：先取第 1 页解析总数，再随机选一页（绕过缓存，保证每次不同）
    pub async fn random(&self, sort: &str) -> Result<WorkshopSearchResult, String> {
        let sort_owned = sort.to_string();
        // 第 1 页：用于解析 total 总数（也直接作为候选，若 random 失败可回退）
        let raw1 = browse_workshop_raw(
            &self.client,
            &BrowseQuery {
                sort: Some(sort_owned.clone()),
                page: Some(1),
                ..Default::default()
            },
        )
        .await?;
        let total = raw1.total;

        if total == 0 {
            // 未解析到总数：直接返回第 1 页（不缓存）
            let items = self.enrich(&raw1.items).await?;
            let items: Vec<_> = items
                .into_iter()
                .filter(|i| !self.is_adult_summary(i))
                .collect();
            let page_size = items.len();
            let has_more = page_size > 0;
            return Ok(WorkshopSearchResult {
                items,
                total,
                page: 1,
                page_size,
                has_more,
            });
        }

        let max_page = ((total / 30).min(1000).max(1)) as u32;
        let page = rand::Rng::gen_range(&mut rand::thread_rng(), 1..=max_page);

        let raw = browse_workshop_raw(
            &self.client,
            &BrowseQuery {
                sort: Some(sort_owned.clone()),
                page: Some(page),
                ..Default::default()
            },
        )
        .await?;
        let items = self.enrich(&raw.items).await?;
        let items: Vec<_> = items
            .into_iter()
            .filter(|i| !self.is_adult_summary(i))
            .collect();
        let page_size = items.len();
        let has_more = page_size > 0;
        Ok(WorkshopSearchResult {
            items,
            total,
            page,
            page_size,
            has_more,
        })
    }

    /// 单条目完整元数据（24h 缓存 + upsert）
    pub async fn detail(&self, id: &str) -> Result<Option<WorkshopItem>, String> {        let key = format!("detail:{id}");
        if let Some(v) = self.cache_get(&key, Duration::from_millis(DETAIL_TTL_MS as u64)) {
            return serde_json::from_value(v).map_err(|e| e.to_string());
        }
        let details = get_item_details(&self.client, &[id.to_string()]).await?;
        let item = details.into_iter().next();
        // 工作友好（默认开启）：详情命中成人内容 → 视为不存在（自动过滤）
        if self.family_friendly.load(Ordering::Relaxed) {
            let adult = item.as_ref().is_some_and(|it| {
                crate::sfw::is_adult_full(
                    &it.title,
                    &it.description,
                    it.creator.as_deref(),
                    &it.tags,
                )
            });
            if adult {
                return Ok(None);
            }
        }
        if let Some(it) = &item {
            let conn = self.db.lock().map_err(|e| e.to_string())?;
            db::upsert_workshop_item(&conn, it)?;
            drop(conn);
            self.cache_set(&key, it);
        }
        Ok(item)
    }

    /// 为列表条目补齐类型/标签/订阅数：本地库 24h 内优先，缺失批量调详情 API 并入库
    async fn enrich(&self, raw: &[BrowseRawItem]) -> Result<Vec<WorkshopItemSummary>, String> {
        let ids: Vec<String> = raw.iter().map(|r| r.id.clone()).collect();
        let known = {
            let conn = self.db.lock().map_err(|e| e.to_string())?;
            db::find_workshop_items(&conn, &ids, DETAIL_TTL_MS)?
        };
        let mut known = known;
        let missing: Vec<String> = ids
            .iter()
            .filter(|id| !known.contains_key(*id))
            .cloned()
            .collect();
        if !missing.is_empty() {
            let details = get_item_details(&self.client, &missing).await?;
            let conn = self.db.lock().map_err(|e| e.to_string())?;
            for d in &details {
                db::upsert_workshop_item(&conn, d)?;
            }
            drop(conn);
            for d in details {
                known.insert(d.id.clone(), d);
            }
        }
        Ok(raw
            .iter()
            .map(|r| {
                let d = known.get(&r.id);
                WorkshopItemSummary {
                    id: r.id.clone(),
                    title: d
                        .map(|d| d.title.clone())
                        .unwrap_or_else(|| r.title.clone()),
                    preview_url: d
                        .map(|d| d.preview_url.clone())
                        .unwrap_or_else(|| r.preview_url.clone()),
                    tags: d.map(|d| d.tags.clone()).unwrap_or_default(),
                    r#type: d
                        .map(|d| d.r#type.clone())
                        .unwrap_or_else(|| "unknown".into()),
                    subscriptions: d.and_then(|d| d.subscriptions),
                    favorited: d.and_then(|d| d.favorited),
                    time_created: d.and_then(|d| d.time_created),
                }
            })
            .collect())
    }

    /// 按请求的排序方式本地排序。
    ///
    /// Steam 当前 SSR 页只有 `totaluniquesubscribers` 会真正按服务端排序；
    /// `totalfavorited` / `timecreated` 会原样返回 trend 列表。这里用 enrich 到的
    /// 订阅/收藏/创建时间元数据做本地排序，保证四种排序都得到正确顺序（不再是“随机/未变”）。
    fn sort_items(items: Vec<WorkshopItemSummary>, sort: Option<&str>) -> Vec<WorkshopItemSummary> {
        let mut items = items;
        match sort.unwrap_or("trend") {
            "totaluniquesubscribers" => items.sort_by(|a, b| {
                b.subscriptions.unwrap_or(0).cmp(&a.subscriptions.unwrap_or(0))
            }),
            "totalfavorited" => items.sort_by(|a, b| {
                b.favorited.unwrap_or(0).cmp(&a.favorited.unwrap_or(0))
            }),
            "timecreated" => {
                items.sort_by_key(|a| std::cmp::Reverse(a.time_created.unwrap_or(0)))
            }
            _ => {} // trend 等：保持 Steam 返回顺序
        }
        items
    }
}

// ---------- Tauri 命令 ----------

#[tauri::command]
pub async fn workshop_search(
    svc: tauri::State<'_, Arc<WorkshopService>>,
    params: WorkshopSearchParams,
) -> Result<WorkshopSearchResult, String> {
    svc.search(params).await
}

#[tauri::command]
pub async fn workshop_random(
    svc: tauri::State<'_, Arc<WorkshopService>>,
    sort: Option<String>,
) -> Result<WorkshopSearchResult, String> {
    svc.random(sort.as_deref().unwrap_or("trend")).await
}

#[tauri::command]
pub async fn workshop_item(
    svc: tauri::State<'_, Arc<WorkshopService>>,
    id: String,
) -> Result<Option<WorkshopItem>, String> {
    svc.detail(&id).await
}

/// 切换「工作友好」（自动过滤成人内容）。返回当前开关状态（默认开启）。
#[tauri::command]
pub async fn workshop_set_family_friendly(
    svc: tauri::State<'_, Arc<WorkshopService>>,
    enabled: bool,
) -> Result<bool, String> {
    {
        let conn = svc.db.lock().map_err(|e| e.to_string())?;
        db::set_setting(&conn, "family_friendly", if enabled { "true" } else { "false" })?;
    }
    svc.cache
        .lock()
        .unwrap()
        .retain(|k, _| !k.starts_with("search:"));
    svc.family_friendly.store(enabled, Ordering::Relaxed);
    Ok(enabled)
}
