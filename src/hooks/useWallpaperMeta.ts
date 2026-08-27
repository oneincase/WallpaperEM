// 全局壁纸元数据：各页共享的「已应用」与「已下载（本地库）」标识。
// 已应用取自 wallpaper_active_items；已下载取自 library_list。
import { useCallback, useEffect, useState } from "react";
import { api } from "../api/steam";

export function useWallpaperMeta() {
  const [appliedItems, setAppliedItems] = useState<Set<string>>(new Set());
  const [downloadedItems, setDownloadedItems] = useState<Set<string>>(new Set());

  const refresh = useCallback(async () => {
    try {
      const [applied, library] = await Promise.all([
        api.wallpaperActiveItems(),
        api.libraryList(""),
      ]);
      setAppliedItems(new Set(applied));
      setDownloadedItems(new Set(library.map((i) => i.itemId)));
    } catch {
      setAppliedItems(new Set());
      setDownloadedItems(new Set());
    }
  }, []);

  useEffect(() => {
    refresh();
  }, [refresh]);

  // 应用成功后可即时把该条目标记为已应用
  const markApplied = useCallback((id: string) => {
    setAppliedItems((prev) => {
      const next = new Set(prev);
      next.add(id);
      return next;
    });
  }, []);

  // 应用新壁纸会替换旧壁纸，旧条目应从已应用集合中去掉，故重取权威集合
  const refreshApplied = useCallback(async () => {
    try {
      const applied = await api.wallpaperActiveItems();
      setAppliedItems(new Set(applied));
    } catch {
      setAppliedItems(new Set());
    }
  }, []);

  return { appliedItems, downloadedItems, refresh, markApplied, refreshApplied };
}
