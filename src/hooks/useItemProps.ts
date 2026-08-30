// 壁纸自定义属性可用性：读 project.json（经 library_item_props）判断壁纸
// 是否声明了可自定义配置项。卡片/详情页用它决定「属性」入口是否展示，
// 弹窗打开后再拉取完整定义。模块级缓存，会话内不重复读盘。
import { useEffect, useState } from "react";
import { api, type WebPropDef } from "../api/steam";

const cache = new Map<string, Promise<WebPropDef[]>>();

/** 取属性定义（缓存命中直接复用；读取失败按「无属性」处理） */
export function fetchItemPropsCached(itemId: string): Promise<WebPropDef[]> {
  let p = cache.get(itemId);
  if (!p) {
    p = api.libraryItemProps(itemId).catch(() => [] as WebPropDef[]);
    cache.set(itemId, p);
  }
  return p;
}

/** 弹窗保存后刷新缓存里该条目的 overridden 状态（下次挂载的页面看到的是新值） */
export function refreshItemPropsCache(itemId: string): void {
  cache.delete(itemId);
}

/**
 * 单个壁纸的可自定义属性定义；undefined = 检测中，空数组 = project.json
 * 未声明可自定义项（或壁纸未下载）。
 */
export function useItemProps(
  itemId: string | null | undefined
): WebPropDef[] | undefined {
  const [defs, setDefs] = useState<WebPropDef[] | undefined>(undefined);
  useEffect(() => {
    if (!itemId) {
      setDefs(undefined);
      return;
    }
    let alive = true;
    setDefs(undefined);
    fetchItemPropsCached(itemId).then((list) => {
      if (alive) setDefs(list);
    });
    return () => {
      alive = false;
    };
  }, [itemId]);
  return defs;
}
