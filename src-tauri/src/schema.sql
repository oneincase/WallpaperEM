-- WE 壁纸引擎桌面版 schema v1（对应设计方案 §5）

CREATE TABLE IF NOT EXISTS users (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  steamid64 TEXT UNIQUE NOT NULL,
  nickname TEXT,
  avatar TEXT,
  created_at INTEGER NOT NULL DEFAULT (unixepoch())
);

CREATE TABLE IF NOT EXISTS sessions (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  user_id INTEGER REFERENCES users(id),
  token_hash TEXT UNIQUE NOT NULL,
  expires_at INTEGER NOT NULL,
  created_at INTEGER NOT NULL DEFAULT (unixepoch())
);

CREATE TABLE IF NOT EXISTS workshop_items (
  id TEXT PRIMARY KEY,                -- publishedfileid
  title TEXT,
  description TEXT,
  preview_url TEXT,
  file_url TEXT,
  type TEXT,                          -- video|scene|web|gif|application|unknown
  tags TEXT,                          -- JSON
  size_x INTEGER,
  size_y INTEGER,
  subscriptions INTEGER,
  favorited INTEGER,
  metadata_json TEXT,                 -- 原始元数据快照
  fetched_at INTEGER,                 -- 缓存 TTL
  created_at INTEGER NOT NULL DEFAULT (unixepoch()),
  updated_at INTEGER NOT NULL DEFAULT (unixepoch())
);

CREATE TABLE IF NOT EXISTS downloads (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  item_id TEXT REFERENCES workshop_items(id),
  status TEXT NOT NULL DEFAULT 'queued', -- queued|authenticating|downloading|installing|done|failed
  progress REAL NOT NULL DEFAULT 0,
  error_code TEXT,
  error_msg TEXT,
  waiting_guard INTEGER NOT NULL DEFAULT 0,
  target_dir TEXT,
  file_hash TEXT,
  created_at INTEGER NOT NULL DEFAULT (unixepoch()),
  started_at INTEGER,
  finished_at INTEGER
);

CREATE TABLE IF NOT EXISTS favorites (
  user_id INTEGER NOT NULL,
  item_id TEXT NOT NULL,
  created_at INTEGER NOT NULL DEFAULT (unixepoch()),
  PRIMARY KEY (user_id, item_id)
);

CREATE TABLE IF NOT EXISTS library_items (
  item_id TEXT PRIMARY KEY,
  title TEXT,
  type TEXT,
  preview_url TEXT,
  tags TEXT,                          -- JSON
  size_bytes INTEGER,
  file_count INTEGER,
  project_json TEXT,
  downloaded_at INTEGER NOT NULL DEFAULT (unixepoch()),
  hash TEXT
);

CREATE TABLE IF NOT EXISTS wallpaper_sessions (
  display_id TEXT PRIMARY KEY,
  item_id TEXT,
  config_json TEXT,
  updated_at INTEGER NOT NULL DEFAULT (unixepoch())
);

CREATE TABLE IF NOT EXISTS settings (
  key TEXT PRIMARY KEY,
  value TEXT
);

CREATE TABLE IF NOT EXISTS playlists (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  name TEXT NOT NULL,
  item_ids TEXT NOT NULL,             -- JSON
  interval_sec INTEGER NOT NULL DEFAULT 600
);
