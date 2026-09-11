//! 工坊服务：搜索/详情 + TTL 缓存 + DB upsert（对齐 Web 版 modules/workshop/service.ts）
//!
//! 注意：DB 连接以 Arc<Mutex<Connection>> 持有，网络 await 期间绝不持锁。

use rusqlite::Connection;
use std::collections::HashMap;
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
}

impl WorkshopService {
    pub fn new(client: SteamClient, db: Arc<Mutex<Connection>>) -> Self {
        Self {
            client,
            db,
            cache: Mutex::new(HashMap::new()),
        }
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

    /// 并集查询的组合数上限：组间笛卡尔积，超出时截断（防爆查询量）。
    /// 每个组合是一次独立的 Steam 浏览请求（组内并集只能这么拆），所以上限
    /// 同时是并发数上限；16 覆盖得住正常用法（各组选 1~2 个），也确实拦得住
    /// 全面多选。截断时结果会漏，必须让用户看见 —— 见 `combos_of` 的返回值。
    const MAX_COMBOS: usize = 16;

    /// 把筛选参数归一成「标签组 + 排除标签」。
    /// 组内并集（OR）、组间交集（AND）；type 入口自成一组参与交集。
    /// search 与 random 共用，保证两条路径的筛选语义完全一致。
    fn resolve_groups(params: &WorkshopSearchParams) -> (Vec<Vec<String>>, Vec<String>) {
        let mut groups: Vec<Vec<String>> = Vec::new();
        if let Some(t) = params.r#type.as_deref() {
            if !t.is_empty() && t != "unknown" {
                if let Some(tag) = TYPE_TAG.iter().find(|(k, _)| *k == t).map(|(_, v)| *v) {
                    groups.push(vec![tag.to_string()]);
                }
            }
        }
        if !params.tag_groups.is_empty() {
            // 新协议：组内 OR、组间 AND
            for g in &params.tag_groups {
                let cleaned: Vec<String> = g
                    .iter()
                    .map(|t| t.trim().to_string())
                    .filter(|t| !t.is_empty())
                    .collect();
                if cleaned.is_empty() {
                    continue;
                }
                // 与已有组完全同集则跳过（类型入口与标签面板选到同一标签时不多发查询）
                if groups.iter().any(|e| e == &cleaned) {
                    continue;
                }
                groups.push(cleaned);
            }
        } else {
            // 旧协议：平面 tags 严格 AND，等价于每个标签自成一组
            for tag in &params.tags {
                let t = tag.trim();
                if !t.is_empty() && !groups.iter().any(|g| g.len() == 1 && g[0] == t) {
                    groups.push(vec![t.to_string()]);
                }
            }
        }
        let excluded: Vec<String> = params
            .excluded_tags
            .iter()
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .collect();
        (groups, excluded)
    }

    /// 标签组 → 查询组合（组间笛卡尔积，每个组合 = 每组取一个标签的 AND）。
    /// 空条件得到一个空组合（不约束）；组合数超上限时截断并告警。
    ///
    /// 返回 `(组合, 是否被截断)`：截断意味着只查了一部分组合，结果不完整，
    /// 调用方（`search`）要把这个事实透给前端，不能假装结果是对的。
    fn combos_of(groups: &[Vec<String>]) -> (Vec<Vec<String>>, bool) {
        let mut combos: Vec<Vec<String>> = vec![Vec::new()];
        let mut truncated = false;
        for g in groups {
            let mut next: Vec<Vec<String>> = Vec::new();
            for prefix in &combos {
                for tag in g {
                    let mut c = prefix.clone();
                    c.push(tag.clone());
                    next.push(c);
                }
            }
            combos = next;
            if combos.len() > Self::MAX_COMBOS {
                tracing::warn!(
                    "标签组合数超过 {}（{} 组），截断多余组合",
                    Self::MAX_COMBOS,
                    groups.len()
                );
                combos.truncate(Self::MAX_COMBOS);
                truncated = true;
                break;
            }
        }
        (combos, truncated)
    }

    /// 搜索/筛选工坊列表（SSR 解析 + 详情批量补齐 + 二次过滤 + 5min 缓存）
    pub async fn search(
        &self,
        params: WorkshopSearchParams,
    ) -> Result<WorkshopSearchResult, String> {
        let key = format!("search:{params:?}");
        if let Some(v) = self.cache_get(&key, SEARCH_TTL) {
            return serde_json::from_value(v).map_err(|e| e.to_string());
        }

        let (groups, excluded_tags) = Self::resolve_groups(&params);
        let (combos, truncated) = Self::combos_of(&groups);

        if combos.len() > 1 {
            // 组内并集：拆成多次 AND 查询再合并去重（Steam 原生只支持严格 AND）
            let result = self
                .search_union(&params, &combos, &excluded_tags, truncated)
                .await?;
            self.cache_set(&key, &result);
            return Ok(result);
        }

        let required_tags = combos.into_iter().next().unwrap_or_default();

        let raw = browse_workshop_raw(
            &self.client,
            &BrowseQuery {
                query: params.query.clone(),
                sort: params.sort.clone(),
                page: params.page,
                required_tags: required_tags.clone(),
                excluded_tags: excluded_tags.clone(),
                days: params.days,
                created_after: params.created_after,
                created_before: params.created_before,
                updated_after: params.updated_after,
                updated_before: params.updated_before,
            },
        )
        .await?;

        let items = self.enrich(&raw.items).await?;

        // 服务端 requiredtags 可能不完整生效，本地二次过滤兜底。
        // 注意语义要与 Steam 保持一致：required 是 AND，excluded 是「命中任一即排除」。
        let items = items
            .into_iter()
            .filter(|i| {
                let ok_type = match params.r#type.as_deref() {
                    Some(t) if !t.is_empty() => i.r#type == t,
                    _ => true,
                };
                let ok_required = required_tags.iter().all(|t| i.tags.iter().any(|x| x == t));
                let ok_excluded = !excluded_tags.iter().any(|t| i.tags.iter().any(|x| x == t));
                ok_type && ok_required && ok_excluded
            })
            .collect::<Vec<_>>();

        // 本地排序（Steam SSR 只对 totaluniquesubscribers 真正服务端排序，其余用元数据补齐）
        let items = Self::sort_items(items, params.sort.as_deref());

        let page = params.page.unwrap_or(1);
        let has_more = if raw.total > 0 {
            page < 1000 && (page as usize) * crate::steam::browse::PAGE_SIZE < raw.total
        } else {
            raw.has_more
        };
        // 本页被二次过滤清空不代表后面没有了：只有 Steam 也没给出下一页时才收尾
        let has_more = has_more && !raw.items.is_empty();

        let result = WorkshopSearchResult {
            items,
            total: raw.total,
            page,
            page_size: crate::steam::browse::PAGE_SIZE,
            has_more,
            // 单组合路径不存在截断（组合数 ≤ 1）
            truncated: false,
        };
        self.cache_set(&key, &result);
        Ok(result)
    }

    /// 并集搜索：每个标签组合一次独立查询（抓第 1..=page 页），合并去重后统一
    /// 过滤/排序/分页。组合间并行；组合内页码必须顺序（第 N 页依赖前 N-1 页都抓过
    /// 才能拼出正确的并集第 N 页）。
    async fn search_union(
        &self,
        params: &WorkshopSearchParams,
        combos: &[Vec<String>],
        excluded_tags: &[String],
        truncated: bool,
    ) -> Result<WorkshopSearchResult, String> {
        const PAGE_SIZE: usize = crate::steam::browse::PAGE_SIZE;
        // 并集查询的成本随页码线性增长（组合数 × 页数），封顶防深翻页打爆 Steam
        const MAX_UNION_PAGE: u32 = 20;
        let page = params.page.unwrap_or(1).clamp(1, MAX_UNION_PAGE);

        let mut set = tokio::task::JoinSet::new();
        for combo in combos {
            let client = self.client.clone();
            let params = params.clone();
            let combo = combo.clone();
            let excluded = excluded_tags.to_vec();
            set.spawn(async move {
                let mut items: Vec<BrowseRawItem> = Vec::new();
                let mut total = 0usize;
                let mut more = false;
                for pg in 1..=page {
                    let raw = browse_workshop_raw(
                        &client,
                        &BrowseQuery {
                            query: params.query.clone(),
                            sort: params.sort.clone(),
                            page: Some(pg),
                            required_tags: combo.clone(),
                            excluded_tags: excluded.clone(),
                            days: params.days,
                            created_after: params.created_after,
                            created_before: params.created_before,
                            updated_after: params.updated_after,
                            updated_before: params.updated_before,
                        },
                    )
                    .await?;
                    let empty = raw.items.is_empty();
                    if pg == 1 {
                        total = raw.total;
                    }
                    more = if raw.total > 0 {
                        (pg as usize) * PAGE_SIZE < raw.total
                    } else {
                        raw.has_more
                    };
                    items.extend(raw.items);
                    if empty {
                        break;
                    }
                }
                Ok::<_, String>((items, total, more))
            });
        }

        let mut merged: Vec<BrowseRawItem> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let mut total = 0usize;
        let mut any_more = false;
        while let Some(res) = set.join_next().await {
            let (items, t, more) = res.map_err(|e| e.to_string())??;
            total += t;
            any_more |= more;
            for it in items {
                if seen.insert(it.id.clone()) {
                    merged.push(it);
                }
            }
        }

        let items = self.enrich(&merged).await?;

        // 本地二次过滤：命中**任一组合**即保留（组内并集语义），排除标签照旧
        let items = items
            .into_iter()
            .filter(|i| {
                let ok_type = match params.r#type.as_deref() {
                    Some(t) if !t.is_empty() => i.r#type == t,
                    _ => true,
                };
                let ok_any = combos
                    .iter()
                    .any(|c| c.iter().all(|t| i.tags.iter().any(|x| x == t)));
                let ok_excluded = !excluded_tags.iter().any(|t| i.tags.iter().any(|x| x == t));
                ok_type && ok_any && ok_excluded
            })
            .collect::<Vec<_>>();

        let items = Self::sort_items(items, params.sort.as_deref());

        // 统一分页切片（合并结果可能超过一页）
        let start = (page as usize - 1) * PAGE_SIZE;
        let has_more = any_more || items.len() > start + PAGE_SIZE;
        let items = items.into_iter().skip(start).take(PAGE_SIZE).collect();

        Ok(WorkshopSearchResult {
            items,
            // 各组合总数之和：重叠条目被重复计数，是近似值（去重只在已抓页内做）
            total,
            page,
            page_size: PAGE_SIZE,
            has_more,
            truncated,
        })
    }

    /// 随机壁纸推荐：先取第 1 页解析总数，再随机选一页（绕过缓存，保证每次不同）。
    ///
    /// 接受与 `search` 相同的筛选参数 —— 发现页与工坊页共用同一套条件，
    /// 用户在工坊里排除了成人内容，发现页也不该再推给他。
    pub async fn random(
        &self,
        params: WorkshopSearchParams,
    ) -> Result<WorkshopSearchResult, String> {
        let (groups, excluded_tags) = Self::resolve_groups(&params);
        // 随机推荐：只从组合里挑一个，截断与否都不影响本次结果，忽略标志
        let (combos, _) = Self::combos_of(&groups);
        // 并集语义：随机推荐从任一组合里取即可 —— 随机选一个组合，再按原逻辑
        // 随机页取条目，天然满足「任一命中」。均匀选组合对小组有偏，但推荐场景
        // 不需要严格按结果数加权
        let required_tags = {
            use rand::Rng;
            let idx = rand::thread_rng().gen_range(0..combos.len());
            combos[idx].clone()
        };
        let mk = |page: u32| BrowseQuery {
            query: params.query.clone(),
            sort: params.sort.clone(),
            page: Some(page),
            required_tags: required_tags.clone(),
            excluded_tags: excluded_tags.clone(),
            days: params.days,
            created_after: params.created_after,
            created_before: params.created_before,
            updated_after: params.updated_after,
            updated_before: params.updated_before,
        };
        // 第 1 页：用于解析 total 总数（也直接作为候选，若 random 失败可回退）
        let raw1 = browse_workshop_raw(&self.client, &mk(1)).await?;
        let total = raw1.total;

        if total == 0 {
            // 未解析到总数：直接返回第 1 页（不缓存）
            let items = self.enrich(&raw1.items).await?;
            let page_size = items.len();
            let has_more = page_size > 0;
            return Ok(WorkshopSearchResult {
                items,
                total,
                page: 1,
                page_size,
                has_more,
                // 随机推荐只取一个组合，与截断无关
                truncated: false,
            });
        }

        let max_page = ((total / crate::steam::browse::PAGE_SIZE).min(1000).max(1)) as u32;
        let page = rand::Rng::gen_range(&mut rand::thread_rng(), 1..=max_page);

        let raw = browse_workshop_raw(&self.client, &mk(page)).await?;
        let items = self.enrich(&raw.items).await?;
        let page_size = items.len();
        let has_more = page_size > 0;
        Ok(WorkshopSearchResult {
            items,
            total,
            page,
            page_size,
            has_more,
            truncated: false,
        })
    }

    /// 单条目完整元数据（24h 缓存 + upsert）
    pub async fn detail(&self, id: &str) -> Result<Option<WorkshopItem>, String> {
        let key = format!("detail:{id}");
        if let Some(v) = self.cache_get(&key, Duration::from_millis(DETAIL_TTL_MS as u64)) {
            return serde_json::from_value(v).map_err(|e| e.to_string());
        }
        let details = get_item_details(&self.client, &[id.to_string()]).await?;
        let item = details.into_iter().next();
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
                b.subscriptions
                    .unwrap_or(0)
                    .cmp(&a.subscriptions.unwrap_or(0))
            }),
            "totalfavorited" => {
                items.sort_by(|a, b| b.favorited.unwrap_or(0).cmp(&a.favorited.unwrap_or(0)))
            }
            "timecreated" => items.sort_by_key(|a| std::cmp::Reverse(a.time_created.unwrap_or(0))),
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
    params: Option<WorkshopSearchParams>,
) -> Result<WorkshopSearchResult, String> {
    let mut p = params.unwrap_or_default();
    if p.sort.is_none() {
        p.sort = Some("trend".into());
    }
    svc.random(p).await
}

#[tauri::command]
pub async fn workshop_item(
    svc: tauri::State<'_, Arc<WorkshopService>>,
    id: String,
) -> Result<Option<WorkshopItem>, String> {
    svc.detail(&id).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn groups(v: &[&[&str]]) -> Vec<Vec<String>> {
        v.iter()
            .map(|g| g.iter().map(|s| s.to_string()).collect())
            .collect()
    }

    /// 组间笛卡尔积：每组取一个标签组成一次 AND 查询
    #[test]
    fn combos_are_cartesian_product_of_groups() {
        let (combos, truncated) =
            WorkshopService::combos_of(&groups(&[&["Scene", "Video"], &["Anime"]]));
        assert!(!truncated);
        assert_eq!(
            combos,
            vec![
                vec!["Scene".to_string(), "Anime".to_string()],
                vec!["Video".to_string(), "Anime".to_string()],
            ]
        );
    }

    /// 无筛选 = 一个空组合（不约束），不是零个组合
    #[test]
    fn empty_groups_yield_single_empty_combo() {
        let (combos, truncated) = WorkshopService::combos_of(&[]);
        assert!(!truncated);
        assert_eq!(combos, vec![Vec::<String>::new()]);
    }

    /// 正好顶到上限不算截断
    #[test]
    fn exactly_at_cap_is_not_truncated() {
        let g = groups(&[&["a1", "a2", "a3", "a4"], &["b1", "b2", "b3", "b4"]]);
        let (combos, truncated) = WorkshopService::combos_of(&g);
        assert_eq!(combos.len(), WorkshopService::MAX_COMBOS);
        assert!(!truncated);
    }

    /// 超上限：截到上限并打标志（结果会漏，前端据此提示用户）
    #[test]
    fn oversized_product_is_truncated_and_flagged() {
        let g = groups(&[&["a1", "a2", "a3"], &["b1", "b2", "b3"], &["c1", "c2", "c3"]]);
        let (combos, truncated) = WorkshopService::combos_of(&g);
        assert!(truncated);
        assert_eq!(combos.len(), WorkshopService::MAX_COMBOS);
        // 截断后剩下的组合仍是合法组合（每组取一个）
        assert!(combos.iter().all(|c| c.len() == 3));
    }
}
