#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(target_os = "windows")]
use aes_gcm::{Aes256Gcm, KeyInit, aead::Aead};
use anyhow::{Context, anyhow};
#[cfg(target_os = "windows")]
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use chrono::{Datelike, Local};
use log::{info, warn};
use reqwest::header::{ACCEPT, CONTENT_TYPE, ORIGIN, REFERER};
#[cfg(not(target_os = "android"))]
use reqwest::header::{ETAG, IF_MATCH, IF_NONE_MATCH, USER_AGENT};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
#[cfg(desktop)]
use std::sync::atomic::AtomicBool;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager, State};
#[cfg(desktop)]
use tauri_plugin_autostart::ManagerExt as AutostartExt;
#[cfg(all(desktop, not(target_os = "windows")))]
use tauri_plugin_notification::NotificationExt;
use tauri_plugin_opener::OpenerExt;

#[cfg(all(feature = "standard", feature = "original"))]
compile_error!("Cargo features `standard` and `original` are mutually exclusive");
#[cfg(not(any(feature = "standard", feature = "original")))]
compile_error!("either Cargo feature `standard` or `original` must be enabled");

// Bangumi 标准版核心模块（契约 C2）：仅 standard edition 编译，
// Original 产物中不存在任何 Bangumi 代码。
#[cfg(feature = "standard")]
pub mod bangumi;

#[cfg(target_os = "android")]
mod mobile;

const ANILIST_API: &str = "https://graphql.anilist.co";
const OFFICIAL_BANGUMI_API: &str = "https://api.bgm.tv/v0";
const DEFAULT_BANGUMI_PROXY: &str = "https://sh1n.cc.cd/v0";
const LEGACY_BANGUMI_PROXY: &str = "https://bgmapi.anibt.net/v0";
const STATE_VERSION: i64 = 3;
const SYNC_VERSION: i64 = 1;
const CACHE_VERSION: i64 = 1;
const BANGUMI_RESOLVER_VERSION: i64 = 5;
// AniList documents a 90 req/min public limit, but can temporarily tighten
// the gateway to 30 req/min. Keep the client below the stricter envelope so a
// burst of season/authority refreshes cannot trigger a rolling 429/403.
const ANILIST_SAFE_REQUESTS_PER_MINUTE: usize = 30;
static ANILIST_REQUEST_WINDOW: LazyLock<tokio::sync::Mutex<VecDeque<Instant>>> =
    LazyLock::new(|| tokio::sync::Mutex::new(VecDeque::new()));
#[cfg(all(feature = "standard", not(target_os = "android")))]
const BANGUMI_EPISODES_CACHE_TTL_SECONDS: i64 = 24 * 60 * 60;
static DAILY_TASK_REMINDER_TIME_RE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"^([01]\d|2[0-3]):[0-5]\d$").unwrap());
#[cfg(desktop)]
static PENDING_WINDOW_ACTIVATION: AtomicBool = AtomicBool::new(false);
#[cfg(not(target_os = "android"))]
const MAX_SYNC_BYTES: usize = 5 * 1024 * 1024;
#[cfg(not(target_os = "android"))]
const WEBDAV_COLLECTION: &str = "AniLog";
#[cfg(not(target_os = "android"))]
const WEBDAV_DOCUMENT: &str = "anilog-sync.json";
#[cfg(not(target_os = "android"))]
const WEBDAV_CREDENTIAL_SERVICE: &str = "io.anilog.webdav";

#[derive(Clone)]
pub struct AppContext {
    state: Arc<Mutex<Value>>,
    runtime: Arc<Mutex<Value>>,
    data_dir: PathBuf,
    cache_dir: PathBuf,
    client: reqwest::Client,
    original: bool,
    sync_wakeup: Arc<tokio::sync::Notify>,
    webdav_wakeup: Arc<tokio::sync::Notify>,
    webdav_sync_lock: Arc<tokio::sync::Mutex<()>>,
    #[cfg(desktop)]
    main_window_opening: Arc<AtomicBool>,
    bangumi_lookup_lock: Arc<tokio::sync::Mutex<()>>,
    bangumi_unavailable_until: Arc<AtomicI64>,
    offline_bangumi: Arc<Value>,
    /// Bangumi Token 存储契约（schema §8）：Windows → KeyringTokenStore；
    /// Android → mobile 桥（Keystore）；其他平台 → UnsupportedTokenStore。
    /// AppContext derive Clone，Arc 克隆零成本。
    #[cfg(feature = "standard")]
    bangumi_tokens: Arc<dyn bangumi::BangumiTokenStore + Send + Sync>,
    /// /v0/me → username 的进程内缓存（bangumi_get_user_collections 使用；
    /// disconnect 时清空）。
    #[cfg(feature = "standard")]
    bangumi_username_cache: Arc<Mutex<Option<String>>>,
}

fn now_seconds() -> i64 {
    chrono::Utc::now().timestamp()
}
fn now_millis() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn value_string(value: Option<&Value>) -> String {
    value
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}
fn value_i64(value: Option<&Value>) -> i64 {
    value.and_then(Value::as_i64).unwrap_or_default()
}
fn value_bool(value: Option<&Value>) -> bool {
    value.and_then(Value::as_bool).unwrap_or(false)
}

fn default_state(original: bool) -> Value {
    let state = json!({
        "version": STATE_VERSION,
        "following": [],
        "tasks": [],
        "seenAiringEvents": [],
        "bangumiTitles": {},
        "settings": {
            "uiLanguage": if original { "en-US" } else { "zh-CN" },
            "pollIntervalMinutes": 5,
            "launchAtLogin": false,
            "minimizeToTray": true,
            "showTrayIcon": true,
            "notifyWhenAired": true,
            "createWatchTasks": true,
            "dailyTaskReminderEnabled": false,
            "dailyTaskReminderTime": "20:00",
            "bangumiApiBaseUrl": if original { "" } else { DEFAULT_BANGUMI_PROXY },
            "titlePreference": "auto"
        },
        "lastSyncAt": now_seconds(),
        "lastTaskReminderDate": "",
        "syncMetadata": { "followingDeletedAt": {} }
    });
    // Bangumi 设置块挂在顶层、与 settings 并列（schema 冻结决定）：
    // 仅 standard 版写入；merge_defaults 依赖 default_state 的键集合自动补齐
    // 旧 v2 状态缺失的 bangumi 块，Original 因此永不补该键。
    #[cfg(feature = "standard")]
    let state = {
        let mut state = state;
        if !original {
            if let Some(object) = state.as_object_mut() {
                object.insert(
                    "bangumi".into(),
                    serde_json::to_value(bangumi::BangumiSyncSettings::default())
                        .unwrap_or_else(|_| json!({})),
                );
                // Phase 3 任务 1：本地-only 同步状态五字段（绝不进坚果云文档，
                // 回归测试 document_from_state_* 锁定；merge_defaults 依赖此键
                // 为旧状态补齐默认值）。
                object.insert(
                    "bangumiSyncStatus".into(),
                    serde_json::to_value(bangumi::BangumiSyncStatus::default())
                        .unwrap_or_else(|_| json!({})),
                );
            }
        }
        state
    };
    state
}

fn merge_defaults(mut loaded: Value, original: bool) -> Value {
    let defaults = default_state(original);
    let Some(target) = loaded.as_object_mut() else {
        return defaults;
    };
    let Some(default_object) = defaults.as_object() else {
        return loaded;
    };
    for (key, default_value) in default_object {
        if !target.contains_key(key) {
            target.insert(key.clone(), default_value.clone());
        }
    }
    if let Some(settings) = target.get_mut("settings").and_then(Value::as_object_mut) {
        if let Some(default_settings) = defaults.get("settings").and_then(Value::as_object) {
            for (key, value) in default_settings {
                if !settings.contains_key(key) {
                    settings.insert(key.clone(), value.clone());
                }
            }
        }
    }
    target.insert("version".into(), json!(STATE_VERSION));
    ensure_sync_metadata(&mut loaded);
    // STATE_VERSION 3（schema §12）：standard 版为旧记录补 additive 默认键；
    // original 不写 source/mapping 相关新键，行为完全不变。旧 v2 following
    // 条目（无 source）视为 anilist 来源，其 id 不动；旧任务（无 episodeType）
    // 视为 "regular"。已带新键的 v3 记录原样保留（只补缺，不覆盖）。
    #[cfg(feature = "standard")]
    if !original {
        normalize_state_records_for_standard(&mut loaded);
    }
    loaded
}

/// standard 版 additive 默认键归一（schema §3/§12）：following 补
/// source/anilistId/mapping/mappingPending 与 Phase 3 的
/// bangumiStatus/rating/watchedEpisode；任务补 subjectId/episodeId/
/// episodeSortKey/episodeType。仅补缺失键，不改动任何既有业务字段。
#[cfg(feature = "standard")]
fn normalize_state_records_for_standard(state: &mut Value) {
    if let Some(following) = state.get_mut("following").and_then(Value::as_array_mut) {
        for item in following {
            if value_string(item.get("source")).is_empty() {
                item["source"] = json!("anilist");
                item["anilistId"] = Value::Null;
                item["mapping"] = Value::Null;
                item["mappingPending"] = json!(false);
            }
            // Phase 3 任务 1：收藏/评分/进度镜像字段（get_state/public_state
            // 自然带出；缺省 null）。
            if !item.get("bangumiStatus").is_some_and(Value::is_string) {
                item["bangumiStatus"] = Value::Null;
            }
            if !item.get("rating").is_some_and(Value::is_number) {
                item["rating"] = Value::Null;
            }
            if !item.get("watchedEpisode").is_some_and(Value::is_number) {
                item["watchedEpisode"] = Value::Null;
            }
        }
    }
    if let Some(tasks) = state.get_mut("tasks").and_then(Value::as_array_mut) {
        for task in tasks {
            if value_string(task.get("episodeType")).is_empty() {
                task["episodeType"] = json!("regular");
            }
            if !task.get("episodeSortKey").is_some_and(Value::is_string) {
                let episode = value_i64(task.get("episode"));
                task["episodeSortKey"] = json!(if episode > 0 {
                    episode.to_string()
                } else {
                    value_string(task.get("id"))
                });
            }
            if task.get("subjectId").is_none() {
                task["subjectId"] = Value::Null;
            }
            if task.get("episodeId").is_none() {
                task["episodeId"] = Value::Null;
            }
        }
    }
}

fn ensure_sync_metadata(state: &mut Value) {
    let object = state.as_object_mut().expect("state must be an object");
    if !object.get("following").is_some_and(Value::is_array) {
        object.insert("following".into(), json!([]));
    }
    if !object.get("tasks").is_some_and(Value::is_array) {
        object.insert("tasks".into(), json!([]));
    }
    if !object.get("seenAiringEvents").is_some_and(Value::is_array) {
        object.insert("seenAiringEvents".into(), json!([]));
    }
    if !object.get("syncMetadata").is_some_and(Value::is_object) {
        object.insert("syncMetadata".into(), json!({}));
    }
    let metadata = object
        .get_mut("syncMetadata")
        .and_then(Value::as_object_mut)
        .unwrap();
    if !metadata
        .get("followingDeletedAt")
        .is_some_and(Value::is_object)
    {
        metadata.insert("followingDeletedAt".into(), json!({}));
    }
    if let Some(following) = object.get_mut("following").and_then(Value::as_array_mut) {
        for item in following {
            if !item.get("syncUpdatedAt").is_some_and(Value::is_number) {
                let followed = value_i64(item.get("followedAt"));
                item["syncUpdatedAt"] = json!(if followed > 0 {
                    followed * 1000
                } else {
                    now_millis()
                });
            }
        }
    }
    if let Some(tasks) = object.get_mut("tasks").and_then(Value::as_array_mut) {
        for item in tasks {
            if !item.get("syncUpdatedAt").is_some_and(Value::is_number) {
                item["syncUpdatedAt"] = json!(now_millis());
            }
        }
    }
    let task_ids = object
        .get("tasks")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|task| task.get("id").and_then(Value::as_str))
        .map(str::to_string)
        .collect::<Vec<_>>();
    if let Some(events) = object
        .get_mut("seenAiringEvents")
        .and_then(Value::as_array_mut)
    {
        let mut known = events
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect::<HashSet<_>>();
        for id in task_ids {
            if known.insert(id.clone()) {
                events.push(json!(id));
            }
        }
        const MAX_SEEN_AIRING_EVENTS: usize = 2_000;
        if events.len() > MAX_SEEN_AIRING_EVENTS {
            events.drain(0..events.len() - MAX_SEEN_AIRING_EVENTS);
        }
    }
}

fn data_directory(app: &AppHandle) -> anyhow::Result<PathBuf> {
    #[cfg(target_os = "android")]
    {
        return app
            .path()
            .app_data_dir()
            .context("cannot resolve Android app data directory");
    }
    #[cfg(not(target_os = "android"))]
    {
        if cfg!(debug_assertions) {
            return app
                .path()
                .app_data_dir()
                .context("cannot resolve app data directory");
        }
        let executable = std::env::current_exe().context("cannot resolve executable path")?;
        return Ok(executable
            .parent()
            .context("executable directory is unavailable")?
            .join("data"));
    }
}

fn load_context(app: &AppHandle, original: bool) -> anyhow::Result<AppContext> {
    let data_dir = data_directory(app)?;
    fs::create_dir_all(&data_dir)?;
    let state_path = data_dir.join("anilog-state.json");
    let legacy_path = app
        .path()
        .app_data_dir()
        .ok()
        .map(|dir| dir.join("anilog-state.json"));
    if !state_path.exists() {
        if let Some(legacy) = legacy_path.filter(|path| path.exists() && path != &state_path) {
            fs::copy(legacy, &state_path).context("migrate existing AniLog state")?;
        }
    }
    let loaded = fs::read_to_string(&state_path)
        .ok()
        .and_then(|body| serde_json::from_str::<Value>(&body).ok())
        .unwrap_or_else(|| default_state(original));
    let mut state = merge_defaults(loaded, original);
    if let Some(settings) = state.get_mut("settings").and_then(Value::as_object_mut) {
        if original {
            settings.insert("bangumiApiBaseUrl".into(), json!(""));
        } else if value_string(settings.get("bangumiApiBaseUrl")) == LEGACY_BANGUMI_PROXY {
            settings.insert("bangumiApiBaseUrl".into(), json!(DEFAULT_BANGUMI_PROXY));
        }
    }
    let cache_dir = data_dir.join("season-cache");
    fs::create_dir_all(&cache_dir)?;
    let offline_bangumi: Value =
        serde_json::from_str(include_str!(concat!(env!("OUT_DIR"), "/bangumi-map.json")))
            .unwrap_or_else(|_| json!({}));
    // 加载时自动映射 + 跨键合并（standard，schema §4）：先做问题 B 的跨键
    // 去重/合并（v0.6 设备同步回来的 AniList 键记录并入 subjectId 键记录），
    // 再对剩余条目跑离线表映射（high/medium 自动绑定，多候选/无结果标
    // mappingPending 等待用户确认）。original 永不执行。
    #[cfg(feature = "standard")]
    if !original {
        reconcile_following_entries(&mut state, &offline_bangumi, original);
    }
    let context = AppContext {
        state: Arc::new(Mutex::new(state)),
        runtime: Arc::new(Mutex::new(json!({
            "isDesktop": cfg!(desktop),
            "notificationsSupported": true,
            "platform": if cfg!(target_os = "android") { "android" } else { std::env::consts::OS },
            "edition": if original { "original" } else { "standard" }
        }))),
        data_dir,
        cache_dir,
        client: reqwest::Client::builder()
            .user_agent(concat!("AniLog Tauri/", env!("CARGO_PKG_VERSION")))
            // 问题 D：月度并行拉取与 AniList 补充覆盖都复用该客户端；没有整体
            // 超时会让失败兜底（stale 缓存）被无限挂起。15s 覆盖 Bangumi 分页、
            // AniList GraphQL 与 WebDAV 请求。
            .timeout(std::time::Duration::from_secs(15))
            .build()?,
        original,
        sync_wakeup: Arc::new(tokio::sync::Notify::new()),
        webdav_wakeup: Arc::new(tokio::sync::Notify::new()),
        webdav_sync_lock: Arc::new(tokio::sync::Mutex::new(())),
        #[cfg(desktop)]
        main_window_opening: Arc::new(AtomicBool::new(false)),
        bangumi_lookup_lock: Arc::new(tokio::sync::Mutex::new(())),
        bangumi_unavailable_until: Arc::new(AtomicI64::new(0)),
        offline_bangumi: Arc::new(offline_bangumi),
        #[cfg(feature = "standard")]
        bangumi_tokens: bangumi_token_store(app),
        #[cfg(feature = "standard")]
        bangumi_username_cache: Arc::new(Mutex::new(None)),
    };
    // 覆盖安装后先用本机最近一次成功的 Bangumi 逐集缓存修复旧状态，
    // 再启动窗口与后台同步。这样分季条目的历史 AniList 全局集号/旧日期
    // 不会在首次渲染时短暂显示，也不依赖 AniList 当前可达。
    #[cfg(all(feature = "standard", not(target_os = "android")))]
    if !original {
        let cache_dir = bangumi_cache_dir(&context);
        if let Ok(mut state) = context.state.lock() {
            let _ = apply_cached_bangumi_episode_authority(
                &mut state,
                &cache_dir,
                now_seconds(),
            );
        }
    }
    #[cfg(not(target_os = "android"))]
    if let Err(error) = migrate_legacy_webdav_config(&context) {
        warn!("failed to migrate legacy WebDAV configuration: {error}");
    }
    context.save_state()?;
    Ok(context)
}

/// 按平台选择 Bangumi Token 存储实现（schema §8）：
/// Windows → Credential Manager（KeyringTokenStore）；Android → mobile 桥
/// （Keystore，决策 12：桥只做凭据存取，不发起任何 Bangumi 请求）；
/// 其他平台 → UnsupportedTokenStore。
#[cfg(feature = "standard")]
fn bangumi_token_store(app: &AppHandle) -> Arc<dyn bangumi::BangumiTokenStore + Send + Sync> {
    let _ = app;
    #[cfg(target_os = "android")]
    {
        let bridge = app.state::<mobile::MobileBridge>().inner().clone();
        return Arc::new(mobile::MobileBangumiTokenStore::new(bridge));
    }
    #[cfg(target_os = "windows")]
    {
        return Arc::new(bangumi::KeyringTokenStore::default());
    }
    #[cfg(not(any(target_os = "android", target_os = "windows")))]
    {
        Arc::new(bangumi::UnsupportedTokenStore)
    }
}

/// 主客户端基址解析（决策 11）：读顶层 `bangumi.apiBaseUrl`，为空回落
/// `settings.bangumiApiBaseUrl`，再为空用官方 `https://api.bgm.tv`。
#[cfg(feature = "standard")]
fn bangumi_base_urls(state: &Value) -> bangumi::BangumiBaseUrls {
    let from_block = value_string(
        state
            .get("bangumi")
            .and_then(|block| block.get("apiBaseUrl")),
    );
    let configured = if from_block.trim().is_empty() {
        value_string(state["settings"].get("bangumiApiBaseUrl"))
    } else {
        from_block
    };
    bangumi::resolve_base_urls(&configured)
}

/// 决策 11 的正向同步：把 `settings.bangumiApiBaseUrl` 的当前值镜像到顶层
/// `bangumi.apiBaseUrl`（update_settings 编辑旧字段时调用；
/// `bangumi_set_api_base_url` 命令则负责反向入口，两处同写）。
#[cfg(feature = "standard")]
fn sync_bangumi_api_base_url_into_block(state: &mut Value) {
    let url = value_string(state["settings"].get("bangumiApiBaseUrl"));
    if let Some(block) = state.get_mut("bangumi").and_then(Value::as_object_mut) {
        block.insert("apiBaseUrl".into(), json!(url));
    }
}

/// Original 版 `bangumi_auth_status` 的统一拒绝返回（双 edition 编译，
/// original 下 6 个 bangumi 命令运行即走拒绝路径；永不回传 Token 本体）。
fn bangumi_auth_status_rejected() -> Value {
    json!({"supported": false, "hasToken": false, "apiBaseUrl": ""})
}

/// Original 版其余 bangumi 命令的统一拒绝返回（固定文案，前端 i18n 按此映射）。
fn bangumi_command_rejected() -> Value {
    json!({"ok": false, "message": "Original 版不支持 Bangumi"})
}

impl AppContext {
    fn state_path(&self) -> PathBuf {
        self.data_dir.join("anilog-state.json")
    }
    fn save_state(&self) -> anyhow::Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow!("state lock poisoned"))?
            .clone();
        if let Some(object) = state.as_object_mut() {
            object.remove("runtime");
        }
        ensure_sync_metadata(&mut state);
        let target = self.state_path();
        let temporary = target.with_extension("json.tmp");
        fs::write(&temporary, serde_json::to_vec_pretty(&state)?)?;
        fs::rename(temporary, target)?;
        Ok(())
    }
    fn public_state(&self) -> Value {
        let mut state = self.state.lock().expect("state lock poisoned").clone();
        inject_public_state_aliases(&mut state, self.original);
        state["runtime"] = self.runtime.lock().expect("runtime lock poisoned").clone();
        state
    }
}

/// 问题 A（P0 开关假象）修复：Bangumi 设置块持久化在顶层 `bangumi` 键，而前端
/// 读取 `bangumiSyncSettings`（拉取/推送开关等）。standard 版在 public_state
/// 输出注入 `bangumiSyncSettings` 别名键（同一份设置的克隆），否则前端永远读
/// undefined（`?? true` 回退造成"保存成功但界面永不刷新"）。`bangumiSyncStatus`
/// 本就在顶层同名透传，无需处理。original 无 bangumi 块、永不注入。
fn inject_public_state_aliases(state: &mut Value, original: bool) {
    #[cfg(feature = "standard")]
    {
        if !original {
            if let Some(block) = state.get("bangumi").cloned() {
                state["bangumiSyncSettings"] = block;
            }
        }
    }
    #[cfg(not(feature = "standard"))]
    {
        let _ = (state, original);
    }
}

fn emit_state(app: &AppHandle, context: &AppContext) {
    let _ = app.emit("state-changed", context.public_state());
}

fn title_for(title: &Value, preference: &str, language: &str) -> String {
    let keys: &[&str] = match preference {
        "romaji" => &["romaji", "english", "native"],
        "native" => &["native", "romaji", "english"],
        _ => &["english", "romaji", "native"],
    };
    keys.iter()
        .find_map(|key| {
            title
                .get(*key)
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        })
        .unwrap_or_else(|| {
            if language == "en-US" {
                "Untitled anime".into()
            } else {
                "未命名番剧".into()
            }
        })
}

fn followed_title_fields(
    state: &Value,
    anime: &Value,
    original: bool,
) -> (String, &'static str, Value) {
    if !original {
        let anime_id = value_i64(anime.get("id")).to_string();
        if let Some(matched) = state["bangumiTitles"].get(&anime_id) {
            if value_string(matched.get("status")) == "matched" {
                let chinese = value_string(matched.get("nameCn"));
                if !chinese.is_empty() {
                    return (
                        chinese,
                        "bangumi",
                        matched.get("subjectId").cloned().unwrap_or(Value::Null),
                    );
                }
            }
        }
    }
    let settings = &state["settings"];
    (
        title_for(
            anime.get("title").unwrap_or(&Value::Null),
            &value_string(settings.get("titlePreference")),
            &value_string(settings.get("uiLanguage")),
        ),
        "anilist",
        Value::Null,
    )
}

fn refresh_original_followed_titles(state: &mut Value) {
    let preference = value_string(state["settings"].get("titlePreference"));
    let language = value_string(state["settings"].get("uiLanguage"));
    if let Some(following) = state["following"].as_array_mut() {
        for item in following
            .iter_mut()
            .filter(|item| value_string(item.get("titleSource")) != "custom")
        {
            item["displayTitle"] = json!(title_for(&item["title"], &preference, &language));
        }
    }
    let followed_titles = state["following"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|item| {
            (
                value_i64(item.get("id")),
                value_string(item.get("displayTitle")),
            )
        })
        .collect::<HashMap<_, _>>();
    if let Some(tasks) = state["tasks"].as_array_mut() {
        for task in tasks {
            if let Some(title) = followed_titles.get(&value_i64(task.get("animeId"))) {
                task["animeTitle"] = json!(title);
            }
        }
    }
}

fn normalize_url(input: &str, suffix: Option<&str>) -> anyhow::Result<String> {
    let value = input.trim();
    if value.is_empty() {
        return Ok(String::new());
    }
    let mut url = url::Url::parse(value).context("请输入有效的 HTTPS 地址")?;
    if url.scheme() != "https"
        || url.username() != ""
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(anyhow!("地址必须是无账号、参数或片段的 HTTPS 地址"));
    }
    let path = url.path().trim_end_matches('/').to_string();
    if let Some(required_suffix) = suffix {
        let next_path = if path.ends_with(required_suffix) {
            path
        } else {
            format!("{path}{required_suffix}")
        };
        url.set_path(&next_path);
    } else {
        url.set_path(&format!("{path}/"));
    }
    Ok(url.to_string().trim_end_matches('/').to_string() + if suffix.is_some() { "" } else { "/" })
}

fn validate_webdav_config(
    enabled: bool,
    base_url: &str,
    username: &str,
    has_password: bool,
) -> anyhow::Result<()> {
    if enabled && (base_url.is_empty() || username.trim().is_empty() || !has_password) {
        return Err(anyhow!("启用同步前请完整填写地址、用户名和密码"));
    }
    Ok(())
}

fn mark_following_changed(state: &mut Value, anime_id: i64) {
    ensure_sync_metadata(state);
    if let Some(item) = state["following"].as_array_mut().and_then(|items| {
        items
            .iter_mut()
            .find(|item| value_i64(item.get("id")) == anime_id)
    }) {
        item["syncUpdatedAt"] = json!(now_millis());
    }
    if let Some(tombstones) = state["syncMetadata"]["followingDeletedAt"].as_object_mut() {
        tombstones.remove(&anime_id.to_string());
    }
}

fn mark_following_deleted(state: &mut Value, anime_id: i64) {
    ensure_sync_metadata(state);
    state["syncMetadata"]["followingDeletedAt"][anime_id.to_string()] = json!(now_millis());
}

/// 该 id 是否存在未过期的删除墓碑（跨键重追复活判定用；与写回引擎内的
/// tombstone_exists 同语义）。
#[cfg(feature = "standard")]
fn following_tombstone_exists(state: &Value, anime_id: i64) -> bool {
    value_i64(state["syncMetadata"]["followingDeletedAt"].get(&anime_id.to_string())) > 0
}

/// 「最近取消追番队列」（Phase 3 任务 3，顶层 `pendingBangumiUnfollows`）：
/// standard 版取消追番 Bangumi 来源条目时由 [`remove_following`] 写入
/// `{subjectId, at}`，供 `push_local_changes` 写回 `PATCH type=5`；
/// 推送成功后清除。**绝不进坚果云文档**（回归测试锁定），original 不写该键。
#[cfg(feature = "standard")]
fn record_pending_bangumi_unfollow(state: &mut Value, subject_id: i64) {
    if subject_id <= 0 {
        return;
    }
    let Some(object) = state.as_object_mut() else {
        return;
    };
    let queue = object
        .entry("pendingBangumiUnfollows".to_string())
        .or_insert_with(|| json!([]));
    let Some(items) = queue.as_array_mut() else {
        *queue = json!([]);
        return;
    };
    if items
        .iter()
        .any(|item| value_i64(item.get("subjectId")) == subject_id)
    {
        return;
    }
    items.push(json!({"subjectId": subject_id, "at": now_seconds()}));
    // 防失控：只保留最近 200 条。
    const MAX_PENDING_UNFOLLOWS: usize = 200;
    if items.len() > MAX_PENDING_UNFOLLOWS {
        items.drain(0..items.len() - MAX_PENDING_UNFOLLOWS);
    }
}

/// 从「最近取消追番队列」移除一个 subject（推送成功后清除；远端驱动的
/// 取消追番在拉取引擎内即时清除以防写回循环）。
#[cfg(feature = "standard")]
fn remove_pending_bangumi_unfollow(state: &mut Value, subject_id: i64) {
    if let Some(items) = state
        .get_mut("pendingBangumiUnfollows")
        .and_then(Value::as_array_mut)
    {
        items.retain(|item| value_i64(item.get("subjectId")) != subject_id);
    }
}

fn remove_following(state: &mut Value, anime_id: i64) -> bool {
    let Some(index) = state["following"].as_array().and_then(|items| {
        items
            .iter()
            .position(|item| value_i64(item.get("id")) == anime_id)
    }) else {
        return false;
    };
    // Phase 3 任务 3：standard 版 Bangumi 来源条目取消追番时入「最近取消队列」，
    // 供写回引擎 PATCH type=5；anilist 来源与 original 不入队。
    #[cfg(feature = "standard")]
    let bangumi_sourced = value_string(state["following"][index].get("source")) == "bangumi";
    state["following"].as_array_mut().unwrap().remove(index);
    state["tasks"].as_array_mut().unwrap().retain(|task| {
        !(value_i64(task.get("animeId")) == anime_id
            && value_string(task.get("status")) == "pending")
    });
    mark_following_deleted(state, anime_id);
    #[cfg(feature = "standard")]
    if bangumi_sourced {
        record_pending_bangumi_unfollow(state, anime_id);
    }
    true
}

/// Bangumi 状态驱动追踪（产品语义门控）：`bangumiStatus` 非空且不是 `doing`
/// （wish 想看 / on_hold 搁置 / done 看过）→ 收录不追踪，不为新集创建观看
/// 任务。空/null（anilist 来源条目或从未同步过状态）→ 维持现有追踪行为。
#[cfg(feature = "standard")]
fn bangumi_status_blocks_tracking(status: &str) -> bool {
    !status.is_empty() && status != "doing"
}

// ---------------------------------------------------------------------------
// Phase 2 主键迁移：映射应用（schema §4）。仅 standard edition。
// ---------------------------------------------------------------------------

/// 把 following 条目从 AniList id 重键为 Bangumi subjectId（任务 2 契约）：
/// 1. 条目重键：id→subjectId、source="bangumi"、anilistId=旧 id、
///    bangumiId=subjectId、mapping 写入、mappingPending=false、syncUpdatedAt=now 毫秒；
/// 2. 旧 AniList id 写墓碑（mark_following_deleted 语义，防止 v0.6 设备把旧
///    记录同步回来），subjectId 上的既有墓碑清除（复活语义）；
/// 3. 该作品的**未完成**任务重键（animeId→subjectId、id→"{subjectId}-{episode}"、
///    subjectId 字段写入）；**已完成任务原样保留不动**（观看历史不重键——
///    Phase 3 关联历史改用 `following.anilistId` 反查，避免改写历史记录）；
/// 4. 幂等：对已重键条目重复 confirm 同一映射返回 false，无副作用。
/// 返回是否发生了实际变更。
#[cfg(feature = "standard")]
fn apply_mapping_with_confidence(
    state: &mut Value,
    anime_id: i64,
    subject_id: i64,
    method: &str,
    confidence: &str,
) -> bool {
    if subject_id <= 0 {
        return false;
    }
    let Some(index) = state["following"].as_array().and_then(|items| {
        items
            .iter()
            .position(|item| value_i64(item.get("id")) == anime_id)
    }) else {
        return false;
    };
    // 幂等：条目已经是该 subjectId 的 bangumi 记录 → 无副作用。
    {
        let entry = &state["following"][index];
        if value_i64(entry.get("id")) == subject_id
            && value_string(entry.get("source")) == "bangumi"
        {
            return false;
        }
    }
    let old_id = anime_id;
    let now_secs = now_seconds();
    {
        let entry = &mut state["following"][index];
        entry["id"] = json!(subject_id);
        entry["source"] = json!("bangumi");
        entry["anilistId"] = json!(old_id);
        entry["bangumiId"] = json!(subject_id);
        entry["mapping"] =
            json!({"method": method, "confidence": confidence, "updatedAt": now_secs});
        entry["mappingPending"] = json!(false);
        entry["syncUpdatedAt"] = json!(now_millis());
    }
    // 旧 AniList id 写墓碑；subjectId 若曾被删除则清墓碑（复活语义）。
    mark_following_deleted(state, old_id);
    mark_following_changed(state, subject_id);
    // 未完成任务重键；目标 id 已存在（如同集已完成历史）时删除旧 pending
    // 记录避免重复。完成任务不动。
    let stale: Vec<usize> = state["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
        .filter(|(_, task)| {
            value_i64(task.get("animeId")) == old_id
                && value_string(task.get("status")) == "pending"
        })
        .map(|(index, _)| index)
        .collect();
    for index in stale.into_iter().rev() {
        let mut task = state["tasks"].as_array_mut().unwrap().remove(index);
        let episode = value_i64(task.get("episode"));
        let new_id = format!("{subject_id}-{episode}");
        let target_exists = state["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|existing| value_string(existing.get("id")) == new_id);
        if target_exists {
            continue;
        }
        task["id"] = json!(new_id);
        task["animeId"] = json!(subject_id);
        task["subjectId"] = json!(subject_id);
        if !task.get("episodeId").is_some_and(Value::is_number) {
            task["episodeId"] = Value::Null;
        }
        if !task.get("episodeSortKey").is_some_and(Value::is_string) {
            task["episodeSortKey"] = json!(if episode > 0 {
                episode.to_string()
            } else {
                new_id.clone()
            });
        }
        if value_string(task.get("episodeType")).is_empty() {
            task["episodeType"] = json!("regular");
        }
        task["syncUpdatedAt"] = json!(now_millis());
        state["tasks"].as_array_mut().unwrap().push(task);
    }
    true
}

/// 任务 2 签名入口：手动确认（method="manual"/confidence="high"）。
#[cfg(feature = "standard")]
fn apply_mapping(state: &mut Value, anime_id: i64, subject_id: i64, manual: bool) -> bool {
    let (method, confidence) = if manual {
        ("manual", "high")
    } else {
        ("local", "high")
    };
    apply_mapping_with_confidence(state, anime_id, subject_id, method, confidence)
}

/// 放弃自动映射：mappingPending=false、mapping={method:"local",confidence:"low"}。
/// 已处于该状态时返回 false（幂等）。
#[cfg(feature = "standard")]
fn skip_mapping_entry(state: &mut Value, anime_id: i64) -> bool {
    let Some(entry) = state["following"].as_array_mut().and_then(|items| {
        items
            .iter_mut()
            .find(|item| value_i64(item.get("id")) == anime_id)
    }) else {
        return false;
    };
    let already_skipped = !value_bool(entry.get("mappingPending"))
        && entry
            .get("mapping")
            .filter(|value| !value.is_null())
            .is_some_and(|mapping| {
                value_string(mapping.get("method")) == "local"
                    && value_string(mapping.get("confidence")) == "low"
            });
    if already_skipped {
        return false;
    }
    entry["mappingPending"] = json!(false);
    entry["mapping"] = json!({"method": "local", "confidence": "low", "updatedAt": now_seconds()});
    entry["syncUpdatedAt"] = json!(now_millis());
    true
}

/// 加载时自动映射（standard，schema §4 优先级）：对每个 anilist 来源且无
/// mapping 且未 pending 的条目跑 `resolve_mapping_candidates`；Mapped 自动
/// apply（high/medium 都绑，method 按解析结果）；Candidates/None →
/// mappingPending=true（待用户确认）。返回自动绑定条数。
#[cfg(feature = "standard")]
fn auto_map_following(state: &mut Value, map: &Value) -> usize {
    let targets: Vec<i64> = state["following"]
        .as_array()
        .unwrap_or(&vec![])
        .iter()
        .filter(|item| {
            value_i64(item.get("id")) > 0
                && (value_string(item.get("source")).is_empty()
                    || value_string(item.get("source")) == "anilist")
                && item
                    .get("mapping")
                    .filter(|value| !value.is_null())
                    .is_none()
                && !value_bool(item.get("mappingPending"))
        })
        .map(|item| value_i64(item.get("id")))
        .collect();
    let mut applied = 0;
    for anime_id in targets {
        let entry = state["following"]
            .as_array()
            .and_then(|items| {
                items
                    .iter()
                    .find(|item| value_i64(item.get("id")) == anime_id)
            })
            .cloned();
        let Some(entry) = entry else { continue };
        match bangumi::resolve_mapping_candidates(map, &entry) {
            bangumi::MappingResolution::Mapped {
                subject_id,
                confidence,
                method,
            } => {
                let method = match method {
                    bangumi::MappingMethod::TitleYear => "title-year",
                    _ => "local",
                };
                let confidence = match confidence {
                    bangumi::MappingConfidence::Medium => "medium",
                    _ => "high",
                };
                if subject_id == anime_id {
                    // 离线表把 id 指向自身：无需重键，仅补 mapping 元数据。
                    if let Some(item) = state["following"].as_array_mut().and_then(|items| {
                        items
                            .iter_mut()
                            .find(|item| value_i64(item.get("id")) == anime_id)
                    }) {
                        item["mapping"] = json!({"method": method, "confidence": confidence, "updatedAt": now_seconds()});
                        item["mappingPending"] = json!(false);
                    }
                    applied += 1;
                } else if apply_mapping_with_confidence(
                    state, anime_id, subject_id, method, confidence,
                ) {
                    applied += 1;
                }
            }
            // 多候选 / 无法判定 → 标记待确认，不改 id、不删数据。
            _ => {
                if let Some(item) = state["following"].as_array_mut().and_then(|items| {
                    items
                        .iter_mut()
                        .find(|item| value_i64(item.get("id")) == anime_id)
                }) {
                    item["mappingPending"] = json!(true);
                }
            }
        }
    }
    applied
}

/// 问题 B（P0 重复追番）离线映射反查：anilistId → subjectId。优先
/// `anilistIndex` 代表项；缺失时扫描 `bySubject` 中 `.a` 匹配项，唯一命中才认
/// （多义不自动合并）。
#[cfg(feature = "standard")]
fn offline_mapped_subject_id(map: &Value, anilist_id: i64) -> Option<i64> {
    if let Some(subject_id) = map
        .get("anilistIndex")
        .and_then(|index| index.get(anilist_id.to_string()))
        .and_then(Value::as_i64)
        .filter(|id| *id > 0)
    {
        return Some(subject_id);
    }
    let by_subject = map.get("bySubject")?.as_object()?;
    let mut found: Option<i64> = None;
    for (key, entry) in by_subject {
        if value_i64(entry.get("a")) == anilist_id {
            let Ok(subject_id) = key.parse::<i64>() else {
                continue;
            };
            if subject_id <= 0 {
                continue;
            }
            if found.is_some_and(|previous| previous != subject_id) {
                return None; // 多个不同 subject 候选 → 语义不明，不自动合并。
            }
            found = Some(subject_id);
        }
    }
    found
}

/// 任务重键到 subjectId 键形（问题 B 任务合并用；与 apply_mapping 的重键语义
/// 一致：id/animeId/subjectId 改写、episodeId/episodeSortKey/episodeType 补默认、
/// syncUpdatedAt 提到 now 以在 LWW 中胜出）。
#[cfg(feature = "standard")]
fn rekey_task_to_subject(task: &mut Value, subject_id: i64) {
    let episode = value_i64(task.get("episode"));
    let new_id = format!("{subject_id}-{episode}");
    task["id"] = json!(new_id);
    task["animeId"] = json!(subject_id);
    task["subjectId"] = json!(subject_id);
    if !task.get("episodeId").is_some_and(Value::is_number) {
        task["episodeId"] = Value::Null;
    }
    if !task.get("episodeSortKey").is_some_and(Value::is_string) {
        task["episodeSortKey"] = json!(if episode > 0 {
            episode.to_string()
        } else {
            new_id.clone()
        });
    }
    if value_string(task.get("episodeType")).is_empty() {
        task["episodeType"] = json!("regular");
    }
    task["syncUpdatedAt"] = json!(now_millis());
}

/// 问题 B 跨键任务合并辅助：同侧同 episode 重复任务按 syncUpdatedAt/createdAt
/// 较新者保留。
#[cfg(feature = "standard")]
fn insert_newer_task(map: &mut HashMap<i64, Value>, episode: i64, task: Value) {
    match map.get(&episode) {
        Some(existing)
            if record_timestamp(existing, "createdAt") >= record_timestamp(&task, "createdAt") => {}
        _ => {
            map.insert(episode, task);
        }
    }
}

/// 问题 B 跨键任务合并：把 old_id（旧 AniList 键）与 subject_id（bangumi 主键）
/// 名下任务按 episode 配对裁决——completed 全保留（同集重复保留 syncUpdatedAt
/// 较新者；completed 优先于 pending），pending 较新者胜并重键到 subjectId。
#[cfg(feature = "standard")]
fn merge_cross_key_tasks(state: &mut Value, old_id: i64, subject_id: i64) {
    let mut old_tasks: HashMap<i64, Value> = HashMap::new();
    let mut new_tasks: HashMap<i64, Value> = HashMap::new();
    let mut others: Vec<Value> = Vec::new();
    if let Some(tasks) = state["tasks"].as_array_mut() {
        for task in tasks.drain(..) {
            let anime_id = value_i64(task.get("animeId"));
            let episode = value_i64(task.get("episode"));
            if anime_id == old_id && old_id != subject_id {
                insert_newer_task(&mut old_tasks, episode, task);
            } else if anime_id == subject_id {
                insert_newer_task(&mut new_tasks, episode, task);
            } else {
                others.push(task);
            }
        }
    }
    let mut episodes: Vec<i64> = old_tasks.keys().chain(new_tasks.keys()).copied().collect();
    episodes.sort_unstable();
    episodes.dedup();
    let mut kept: Vec<Value> = Vec::new();
    for episode in episodes {
        let old = old_tasks.remove(&episode);
        let new = new_tasks.remove(&episode);
        let status_of = |task: &Value| value_string(task.get("status"));
        let (mut winner, needs_rekey) = match (&old, &new) {
            (Some(old_task), Some(new_task)) => {
                let old_done = status_of(old_task) == "completed";
                let new_done = status_of(new_task) == "completed";
                let newer_is_old = record_timestamp(old_task, "createdAt")
                    > record_timestamp(new_task, "createdAt");
                if old_done && new_done {
                    // 同集重复完成记录：保留较新者。
                    (
                        newer_is_old.then(|| old_task).unwrap_or(new_task).clone(),
                        false,
                    )
                } else if old_done {
                    (old_task.clone(), false)
                } else if new_done {
                    (new_task.clone(), false)
                } else {
                    // 双 pending：较新者胜；旧键侧胜出时重键到 subjectId。
                    (
                        newer_is_old.then(|| old_task).unwrap_or(new_task).clone(),
                        newer_is_old,
                    )
                }
            }
            (Some(old_task), None) => (old_task.clone(), status_of(old_task) == "pending"),
            (None, Some(new_task)) => (new_task.clone(), false),
            (None, None) => continue,
        };
        if needs_rekey {
            rekey_task_to_subject(&mut winner, subject_id);
        }
        kept.push(winner);
    }
    others.extend(kept);
    state["tasks"] = json!(others);
}

/// 问题 B 单条跨键合并：把旧 AniList 键条目 E（id=old_id）合并进既有 bangumi
/// 条目（id=subject_id）。任务合并 + 删除 E + 对 old_id 写墓碑（时间取
/// max(记录时间, now)，保证 WebDAV 对端同键记录永不复活——问题 E 契约）+
/// 用 E 补齐 bangumi 条目缺失的展示字段 + bump subject 条目（含清 S 侧墓碑）。
#[cfg(feature = "standard")]
fn merge_cross_key_entry(state: &mut Value, old_id: i64, subject_id: i64) -> bool {
    let Some(entry) = state["following"].as_array().and_then(|items| {
        items
            .iter()
            .find(|item| value_i64(item.get("id")) == old_id)
    }) else {
        return false;
    };
    let entry = entry.clone();
    // 1. 任务合并（先于删除 E，任务以 animeId 归属）。
    merge_cross_key_tasks(state, old_id, subject_id);
    // 2. 用 E 补齐 bangumi 条目缺失的展示字段。
    if let Some(target) = state["following"].as_array_mut().and_then(|items| {
        items
            .iter_mut()
            .find(|item| value_i64(item.get("id")) == subject_id)
    }) {
        let display = value_string(target.get("displayTitle"));
        if display.is_empty() {
            let fallback = value_string(entry.get("displayTitle"));
            if !fallback.is_empty() {
                target["displayTitle"] = json!(fallback);
            }
        }
        let cover = value_string(target.get("coverImage"));
        if cover.is_empty() {
            let fallback = value_string(entry.get("coverImage"));
            if !fallback.is_empty() {
                target["coverImage"] = json!(fallback);
            }
        }
        for key in ["episodes", "format", "seasonYear", "startDate"] {
            if target.get(key).map(Value::is_null).unwrap_or(true) {
                if let Some(value) = entry.get(key).filter(|value| !value.is_null()) {
                    target[key] = value.clone();
                }
            }
        }
    }
    // 3. 删除旧键条目。
    state["following"]
        .as_array_mut()
        .unwrap()
        .retain(|item| value_i64(item.get("id")) != old_id);
    // 4. 墓碑：时间 ≥ 被合并记录的 syncUpdatedAt（问题 E：防远端旧键记录
    //    以更新时间戳复活），同时清 subject 侧墓碑（复活语义）。
    let tombstone = record_timestamp(&entry, "followedAt").max(now_millis());
    ensure_sync_metadata(state);
    state["syncMetadata"]["followingDeletedAt"][old_id.to_string()] = json!(tombstone);
    mark_following_changed(state, subject_id);
    // 5. 重键后的任务对齐 bangumi 条目标题。
    let display = state["following"]
        .as_array()
        .and_then(|items| {
            items
                .iter()
                .find(|item| value_i64(item.get("id")) == subject_id)
        })
        .and_then(|item| item.get("displayTitle"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_default();
    if !display.is_empty() {
        if let Some(tasks) = state["tasks"].as_array_mut() {
            for task in tasks
                .iter_mut()
                .filter(|task| value_i64(task.get("animeId")) == subject_id)
            {
                task["animeTitle"] = json!(display);
            }
        }
    }
    true
}

/// 问题 B 跨键去重/合并（单遍）：对每个 anilist 来源（或缺 source）条目 E
/// （id=A）查找同作品的 bangumi 键记录——显式 anilistId 绑定优先，其次离线
/// 映射（anilistIndex / bySubject 扫描）指向且已存在对应 bangumi 条目。
/// 命中 → 合并进 bangumi 条目；未命中 → 交给 auto_map_following 走单条映射。
#[cfg(feature = "standard")]
fn cross_key_following_merge(state: &mut Value, map: &Value) -> bool {
    let Some(following) = state["following"].as_array().cloned() else {
        return false;
    };
    // 显式绑定索引：anilistId → bangumi 条目 id。
    let mut bound: HashMap<i64, i64> = HashMap::new();
    for item in &following {
        if value_string(item.get("source")) == "bangumi" {
            let anilist_id = value_i64(item.get("anilistId"));
            let id = value_i64(item.get("id"));
            if anilist_id > 0 && id > 0 {
                bound.entry(anilist_id).or_insert(id);
            }
        }
    }
    let mut merged_any = false;
    for item in &following {
        let source = value_string(item.get("source"));
        if !(source.is_empty() || source == "anilist") {
            continue;
        }
        let old_id = value_i64(item.get("id"));
        if old_id <= 0 {
            continue;
        }
        let subject_id = match bound.get(&old_id) {
            Some(subject_id) => Some(*subject_id),
            None => match offline_mapped_subject_id(map, old_id) {
                // 仅当该 subjectId 已有 bangumi 键条目时才算跨键重复。
                Some(subject_id)
                    if subject_id != old_id
                        && following.iter().any(|candidate| {
                            value_i64(candidate.get("id")) == subject_id
                                && value_string(candidate.get("source")) == "bangumi"
                        }) =>
                {
                    Some(subject_id)
                }
                _ => None,
            },
        }
        .filter(|subject_id| *subject_id != old_id);
        let Some(subject_id) = subject_id else {
            continue;
        };
        // 目标条目必须真实存在于当前状态（快照可能过期）。
        let target_exists = state["following"].as_array().is_some_and(|items| {
            items.iter().any(|candidate| {
                value_i64(candidate.get("id")) == subject_id
                    && value_string(candidate.get("source")) == "bangumi"
            })
        });
        let source_exists = state["following"].as_array().is_some_and(|items| {
            items
                .iter()
                .any(|candidate| value_i64(candidate.get("id")) == old_id)
        });
        if target_exists && source_exists && merge_cross_key_entry(state, old_id, subject_id) {
            merged_any = true;
        }
    }
    merged_any
}

/// `anilistIndex` 反查（subjectId → anilistId）：仅唯一命中才认（多义不自动
/// 合并，语义同 [`offline_mapped_subject_id`] 的 bySubject 扫描分支）。
#[cfg(feature = "standard")]
fn anilist_index_reverse(map: &Value, subject_id: i64) -> i64 {
    let Some(index) = map.get("anilistIndex").and_then(Value::as_object) else {
        return 0;
    };
    let mut found = 0i64;
    for (key, value) in index {
        if value_i64(Some(value)) != subject_id {
            continue;
        }
        let Ok(anilist_id) = key.parse::<i64>() else {
            continue;
        };
        if anilist_id <= 0 {
            continue;
        }
        if found > 0 && found != anilist_id {
            return 0;
        }
        found = anilist_id;
    }
    found
}

/// 同集组内权威记录选择：按 fallback 语义时间（syncUpdatedAt 优先，缺省回退
/// fallback 字段秒值）取最新；并列时用 stable_record 决出确定性的胜者。
#[cfg(feature = "standard")]
fn newest_task_of<'a>(records: &[&'a Value], fallback: &str) -> &'a Value {
    let mut best = records[0];
    for candidate in &records[1..] {
        let best_time = record_timestamp(best, fallback);
        let time = record_timestamp(candidate, fallback);
        if time > best_time || (time == best_time && stable_record(candidate) > stable_record(best))
        {
            best = candidate;
        }
    }
    best
}

/// 同集组的权威记录构建（[`canonicalize_cross_key_tasks`] 的裁决核心）：
/// - 组内任一 completed → 权威记录为 completed，内容基准取
///   completedAt/syncUpdatedAt 最新者；completedAt/createdAt 等语义字段取全组
///   最新非空（绝不丢观看历史；completed 永不删除）；
/// - 全部 pending → 保留 syncUpdatedAt 最新的一条；
/// - 一律规范化到 subjectId 键：id="{S}-{episode}"、animeId/subjectId=S、
///   episodeSortKey 补齐；lastChangedBy 保留 winner 原值（写回防循环语义不变）；
/// - 键/字段发生变化 → syncUpdatedAt=now 毫秒（保证其他 v0.7 设备以新时间戳
///   采纳权威记录）；内容完全未变则不动（幂等）。
/// 返回 (权威记录, 是否发生了规范化变更)。
#[cfg(feature = "standard")]
fn authoritative_task(group: &[Value], subject_id: i64, now: i64) -> (Value, bool) {
    let episode = value_i64(group[0].get("episode"));
    let completed: Vec<&Value> = group
        .iter()
        .filter(|task| value_string(task.get("status")) == "completed")
        .collect();
    let any_completed = !completed.is_empty();
    let references: Vec<&Value> = if any_completed {
        completed
    } else {
        group.iter().collect()
    };
    let mut authoritative = newest_task_of(
        &references,
        if any_completed {
            "completedAt"
        } else {
            "createdAt"
        },
    )
    .clone();
    let mut changed = false;
    if any_completed {
        for key in ["completedAt", "createdAt"] {
            let latest = group
                .iter()
                .filter_map(|task| {
                    let value = value_i64(task.get(key));
                    (value > 0).then_some(value)
                })
                .max();
            if let Some(value) = latest {
                if value_i64(authoritative.get(key)) != value {
                    authoritative[key] = json!(value);
                    changed = true;
                }
            }
        }
    }
    let canonical_id = format!("{subject_id}-{episode}");
    if value_string(authoritative.get("id")) != canonical_id {
        authoritative["id"] = json!(canonical_id);
        changed = true;
    }
    if value_i64(authoritative.get("animeId")) != subject_id {
        authoritative["animeId"] = json!(subject_id);
        changed = true;
    }
    if value_i64(authoritative.get("subjectId")) != subject_id {
        authoritative["subjectId"] = json!(subject_id);
        changed = true;
    }
    let has_sort_key = authoritative
        .get("episodeSortKey")
        .is_some_and(|value| value.as_str().is_some_and(|key| !key.is_empty()));
    if !has_sort_key {
        authoritative["episodeSortKey"] = json!(if episode > 0 {
            episode.to_string()
        } else {
            canonical_id.clone()
        });
        changed = true;
    }
    if value_string(authoritative.get("episodeType")).is_empty() {
        authoritative["episodeType"] = json!("regular");
        changed = true;
    }
    if changed {
        authoritative["syncUpdatedAt"] = json!(now);
    }
    (authoritative, changed)
}

/// 问题 3 升级（standard，P0 数据污染愈合）：文档级跨键任务规范化。
///
/// 背景：旧版任务挂 anilistId 键（"21355-5"），新版 bangumi 条目按 subjectId
/// 生成（"140001-5"）；WebDAV 双端并存时同一集出现两条记录（一边 completed
/// 一边 pending，或双 completed），且任务无墓碑机制——本地单侧清理后远端脏
/// 记录会在下次合并时回来。唯一可靠的愈合点是文档级：合并后按作品身份重组
/// state.tasks，再由 document_from_state 重建上传文档，让远端文档被整体覆盖。
///
/// 对每个已知作品身份 (S=subjectId 主键, A=anilistId 旧键；A 可为 0)：
/// 1. 收集 state.tasks 中 animeId∈{S,A} 或 subjectId==S 的全部记录，按
///    episode 分组；
/// 2. 每组经 [`authoritative_task`] 产出一个权威记录，其余记录删除（删除的
///    只是重复条目，语义字段已并入权威记录）；
/// 3. 无映射关系的记录（无对应 bangumi 条目、A 不在 map）一律不动。
///
/// 身份来源（幂等，仅 standard）：
/// - following 中 source=="bangumi" 条目：id=S、anilistId=A；A 缺失时离线
///   映射兜底（bySubject[S].a 直查，或 anilistIndex 反查）；
/// - anilistIndex 中 A→S 且 following 存在 S 条目的也算。
/// 同一 S 的多个旧键别名合并进同一个身份，保证每个 (S, episode) 组恰好产出
/// 一个权威记录（completed 不会因身份拆分被误删）。
///
/// 返回是否发生任何规范化变更（幂等：再次调用返回 false）。
#[cfg(feature = "standard")]
fn canonicalize_cross_key_tasks(state: &mut Value, map: &Value, original: bool) -> bool {
    if original {
        return false;
    }
    let Some(following) = state["following"].as_array().cloned() else {
        return false;
    };
    let bangumi_subjects: HashSet<i64> = following
        .iter()
        .filter(|item| value_string(item.get("source")) == "bangumi")
        .map(|item| value_i64(item.get("id")))
        .filter(|id| *id > 0)
        .collect();
    // S → 旧键别名集合（anilistId 旧键；0 表示无已知旧键）。
    let mut subject_aliases: HashMap<i64, HashSet<i64>> = HashMap::new();
    for item in &following {
        if value_string(item.get("source")) != "bangumi" {
            continue;
        }
        let subject_id = value_i64(item.get("id"));
        if subject_id <= 0 {
            continue;
        }
        let aliases = subject_aliases.entry(subject_id).or_default();
        let mut anilist_id = value_i64(item.get("anilistId"));
        if anilist_id <= 0 {
            // 离线映射兜底：bySubject[S].a 直查，或 anilistIndex 反查。
            anilist_id = map
                .get("bySubject")
                .and_then(|by_subject| by_subject.get(subject_id.to_string()))
                .map(|entry| value_i64(entry.get("a")))
                .unwrap_or(0);
            if anilist_id <= 0 {
                anilist_id = anilist_index_reverse(map, subject_id);
            }
        }
        if anilist_id > 0 && anilist_id != subject_id {
            aliases.insert(anilist_id);
        }
    }
    for item in &following {
        let source = value_string(item.get("source"));
        if !(source.is_empty() || source == "anilist") {
            continue;
        }
        let anilist_id = value_i64(item.get("id"));
        if anilist_id <= 0 {
            continue;
        }
        let subject_id = value_i64(
            map.get("anilistIndex")
                .and_then(|index| index.get(anilist_id.to_string())),
        );
        if subject_id > 0 && subject_id != anilist_id && bangumi_subjects.contains(&subject_id) {
            subject_aliases
                .entry(subject_id)
                .or_default()
                .insert(anilist_id);
        }
    }
    if subject_aliases.is_empty() {
        return false;
    }
    let tasks = state["tasks"].as_array().cloned().unwrap_or_default();
    if tasks.is_empty() {
        return false;
    }
    let now = now_millis();
    // 任务归属：animeId 是已知 subjectId 主键、或属于该主键的旧键别名、或
    // subjectId 字段命中主键。
    let subject_of = |task: &Value| -> Option<i64> {
        let anime_id = value_i64(task.get("animeId"));
        if anime_id > 0 {
            if subject_aliases.contains_key(&anime_id) {
                return Some(anime_id);
            }
            if let Some(subject_id) = subject_aliases
                .iter()
                .find(|(_, aliases)| aliases.contains(&anime_id))
                .map(|(subject_id, _)| *subject_id)
            {
                return Some(subject_id);
            }
        }
        let task_subject = value_i64(task.get("subjectId"));
        if task_subject > 0 && subject_aliases.contains_key(&task_subject) {
            return Some(task_subject);
        }
        None
    };
    let mut output: Vec<Value> = Vec::with_capacity(tasks.len());
    let mut emitted: HashSet<(i64, i64)> = HashSet::new();
    let mut changed_any = false;
    for task in &tasks {
        let episode = value_i64(task.get("episode"));
        let subject_id = if episode > 0 { subject_of(task) } else { None };
        let Some(subject_id) = subject_id else {
            // 无身份归属（无映射关系/无追番条目）→ 原样保留。
            output.push(task.clone());
            continue;
        };
        // 每个 (身份, episode) 组只在其首个成员位置输出一次权威记录。
        if !emitted.insert((subject_id, episode)) {
            changed_any = true; // 重复条目被删除
            continue;
        }
        let group: Vec<Value> = tasks
            .iter()
            .filter(|candidate| {
                value_i64(candidate.get("episode")) == episode
                    && subject_of(candidate) == Some(subject_id)
            })
            .cloned()
            .collect();
        let (authoritative, changed) = authoritative_task(&group, subject_id, now);
        changed_any |= changed;
        output.push(authoritative);
    }
    if changed_any {
        state["tasks"] = json!(output);
    }
    changed_any
}

/// 权威数据修复（存量清理，幂等，standard only）：
/// a) pending 任务 episode > 条目 eps（eps 已知 >0）→ 删除——离线调度曾越过
///    eps 生成任务（如丧失篇 547888 eps=11 存在 ep15 任务）；该集任务由
///    AniList 权威调度在正确时点重建；
/// b) 共享 anilistId 的非主条目 → 删除其 pending 任务（防 547888-12..15 与
///    633836-12..15 这类同集双份）。completed 观看历史一律保留。
/// 任务归属：animeId 直接命中条目 id，否则回退 bangumi 条目 anilistId（旧键）。
#[cfg(feature = "standard")]
fn reconcile_anilist_authority_tasks(state: &mut Value, map: &Value) -> bool {
    let secondary = secondary_anilist_claimant_ids(state, map);
    let following = state["following"].as_array().cloned().unwrap_or_default();
    let entry_for = |anime_id: i64| -> Option<&Value> {
        following
            .iter()
            .find(|item| value_i64(item.get("id")) == anime_id)
            .or_else(|| {
                following.iter().find(|item| {
                    value_string(item.get("source")) == "bangumi"
                        && value_i64(item.get("anilistId")) == anime_id
                })
            })
    };
    let Some(tasks) = state
        .get_mut("tasks")
        .and_then(|tasks| tasks.as_array_mut())
    else {
        return false;
    };
    let before = tasks.len();
    tasks.retain(|task| {
        if value_string(task.get("status")) != "pending" {
            return true; // completed 观看历史永不删除。
        }
        let Some(entry) = entry_for(value_i64(task.get("animeId"))) else {
            return true;
        };
        let episode = value_i64(task.get("episode"));
        let eps = value_i64(entry.get("episodes"));
        if episode > 0 && eps > 0 && episode > eps {
            return false; // a) 越过 eps 的离线残留任务。
        }
        !secondary.contains(&value_i64(entry.get("id"))) // b) 非主条目 pending。
    });
    before != tasks.len()
}

/// 第 8 轮问题 3（存量洪水清理，幂等，standard only）：有 AniList 身份条目
/// 名下 pending 且 `airingAt < followedAt`（followedAt>0 才判）→ 删除。
/// 背景：权威回填曾无门槛，把用户追番前已播/看完的历史整季灌成 pending
/// （黄泉 1..16、夺还篇 1..8 等 pending 60 条）；这些集用户早已看过，本地
/// 只是无任务记录。airingAt 与 followedAt 同为权威 AniList 时间轴（秒），
/// 比较可信的前提是条目有 AniList 身份：anilist 条目 id 即 AniList id；
/// bangumi 条目须 anilistId>0——纯 Bangumi 条目的 airingAt 可能是离线推算，
/// 不判。completed 观看历史一律保留。挂载于
/// [`reconcile_following_entries`]（每次合并后运行：云端旧文档的洪水记录
/// merge 回来后当次即被清掉，幂等；上传文档自愈）。任务归属口径与
/// [`reconcile_anilist_authority_tasks`] 一致：animeId 直接命中条目 id，
/// 否则回退 bangumi 条目 anilistId（旧键）。
#[cfg(feature = "standard")]
fn purge_pre_follow_pending_tasks(state: &mut Value) -> bool {
    let following = state["following"].as_array().cloned().unwrap_or_default();
    // animeId → followedAt（条目 id 与旧键 anilistId 双键收录；两者撞键时
    // 同属一个 AniList 作品，followedAt 取后见者，语义相近无害）。
    let mut followed_at_by_anime_id: HashMap<i64, i64> = HashMap::new();
    for entry in &following {
        let id = value_i64(entry.get("id"));
        let followed_at = value_i64(entry.get("followedAt"));
        if id <= 0 || followed_at <= 0 {
            continue;
        }
        let source = value_string(entry.get("source"));
        let anilist_id = value_i64(entry.get("anilistId"));
        if source == "bangumi" && anilist_id <= 0 {
            continue; // 无 AniList 身份 → airingAt 来源不权威，不判。
        }
        followed_at_by_anime_id.insert(id, followed_at);
        if anilist_id > 0 {
            followed_at_by_anime_id.insert(anilist_id, followed_at);
        }
    }
    if followed_at_by_anime_id.is_empty() {
        return false;
    }
    let Some(tasks) = state
        .get_mut("tasks")
        .and_then(|tasks| tasks.as_array_mut())
    else {
        return false;
    };
    let before = tasks.len();
    tasks.retain(|task| {
        if value_string(task.get("status")) != "pending" {
            return true; // completed 观看历史永不删除。
        }
        let Some(&followed_at) = followed_at_by_anime_id.get(&value_i64(task.get("animeId")))
        else {
            return true;
        };
        let airing_at = value_i64(task.get("airingAt"));
        // 追番前已播的历史集不是待办；无 airingAt（未知）不误删。
        !(airing_at > 0 && airing_at < followed_at)
    });
    before != tasks.len()
}

/// 验收第 4 轮问题 1（存量清理，幂等）：删除 pending 且 airingAt > now 的
/// 任务——从未播出的集不应该是待看任务（此前离线锚点与 AniList 冲突时
/// 曾为未来集建过任务）。删除后该集播出时 sync 会按调度重建；已完成任务
/// 永不删除（观看历史保留）。挂载于 reconcile_following_entries（合并→清理
/// →上传顺序已就绪），云端脏数据由 document_from_state 重建上传愈合。
#[cfg(feature = "standard")]
fn purge_unaired_pending_tasks(state: &mut Value, now: i64) -> bool {
    let Some(tasks) = state
        .get_mut("tasks")
        .and_then(|tasks| tasks.as_array_mut())
    else {
        return false;
    };
    let before = tasks.len();
    tasks.retain(|task| {
        !(value_string(task.get("status")) == "pending" && value_i64(task.get("airingAt")) > now)
    });
    before != tasks.len()
}

/// 权威数据修复（缺口 2 无网络版任务纠偏，幂等，standard only）：条目 AniList
/// 身份（id 或 anilistId）的 nextAiringEpisode.episode 已知时，episode >=
/// next.episode 的 pending 任务删除——AniList 认为未播的集不该有票（播出后由
/// 调度管道按权威时间重建）。与 [`purge_unaired_pending_tasks`] 互补：purge
/// 只删 airingAt > now 的未来时间戳，拦不住离线锚点污染出的"过去假票"（黄泉
/// ep23@9/6、无职 ep11@9/6 23:00 已过当晚等）；这里按 AniList 的集数权威兜底，
/// 无论 airingAt 过去/未来都删。completed 观看历史一律保留。
///
/// 挂载点（第 6 轮误删事故后收敛为 Android only）：mobile merge_status
/// （Java Worker 提供的 nextEpisode 与 Rust 条目 nextAiringEpisode 同源同
/// 口径）。桌面已撤销此清理——`reconcile_following_entries` 不再调用（桌面
/// 条目 next 可能被离线锚点污染，>= next 判定曾把真实已播任务误删，且任务
/// id 已在 seenAiringEvents 中、AIRING_QUERY 窗口永不重建）；桌面任务纠偏与
/// 缺失回填由 `anilist_authority_refresh`/`apply_anilist_authority_media`
/// 的权威 schedule 承担。cfg 门控 `any(target_os = "android", test)`：Android
/// 正式构建编译（mobile.rs 调用），桌面仅在单测中编译（Android 语义纯函数
/// 层验证），桌面正式构建不编译、无 dead_code。
/// 任务归属口径与 [`reconcile_anilist_authority_tasks`] 一致：animeId 直接
/// 命中条目 id，否则回退 bangumi 条目 anilistId（旧键）。
#[cfg(all(feature = "standard", any(target_os = "android", test)))]
fn reconcile_unaired_anilist_next_tasks(state: &mut Value) -> bool {
    let following = state["following"].as_array().cloned().unwrap_or_default();
    // 任务 animeId → 已知 next.episode（条目 id 与旧键 anilistId 双键收录；
    // 两者撞键时同属一个 AniList 作品，next 值一致，覆盖无害）。
    let mut next_by_anime_id: HashMap<i64, i64> = HashMap::new();
    for entry in &following {
        let id = value_i64(entry.get("id"));
        let next_episode = value_i64(
            entry
                .get("nextAiringEpisode")
                .and_then(|next| next.get("episode")),
        );
        if id <= 0 || next_episode <= 0 {
            continue;
        }
        next_by_anime_id.insert(id, next_episode);
        let anilist_id = value_i64(entry.get("anilistId"));
        if anilist_id > 0 {
            next_by_anime_id.insert(anilist_id, next_episode);
        }
    }
    if next_by_anime_id.is_empty() {
        return false;
    }
    let Some(tasks) = state
        .get_mut("tasks")
        .and_then(|tasks| tasks.as_array_mut())
    else {
        return false;
    };
    let before = tasks.len();
    tasks.retain(|task| {
        if value_string(task.get("status")) != "pending" {
            return true; // completed 观看历史永不删除。
        }
        let next_episode = next_by_anime_id
            .get(&value_i64(task.get("animeId")))
            .copied()
            .unwrap_or(0);
        let episode = value_i64(task.get("episode"));
        !(next_episode > 0 && episode > 0 && episode >= next_episode)
    });
    before != tasks.len()
}

/// 问题 B 总入口（standard only）：跨键合并 + 既有单条自动映射。
/// 返回是否发生任何状态变更（following/tasks/syncMetadata 任一）。
/// 挂载点：load_context 尾部（升级原 auto_map_following 调用）与每次
/// WebDAV 合并之后（desktop perform_webdav_sync / Android mobile sync_webdav）。
#[cfg(feature = "standard")]
fn reconcile_following_entries(state: &mut Value, map: &Value, original: bool) -> bool {
    if original {
        return false;
    }
    let before = (
        state.get("following").cloned().unwrap_or(Value::Null),
        state.get("tasks").cloned().unwrap_or(Value::Null),
        state.get("syncMetadata").cloned().unwrap_or(Value::Null),
    );
    cross_key_following_merge(state, map);
    auto_map_following(state, map);
    // 问题 3 升级：原 cleanup_bangumi_duplicate_pendings（只删与 completed
    // 历史重复的 pending）升级为文档级跨键任务规范化 canonicalize_cross_key_tasks：
    // 按作品身份把同集记录裁决为唯一权威记录（规范化到 subjectId 键、completed
    // 优先且语义字段不丢历史、变更时间戳提到 now 保证上传后其他设备采纳），
    // 并吸收原防重语义（有观看历史的重复 pending 被删、completed 永不删除）。
    canonicalize_cross_key_tasks(state, map, original);
    // 验收第 4 轮问题 1：从未播出（airingAt > now）的 pending 任务清理，
    // 幂等；original 分支已在函数头返回，不会执行到这里。
    purge_unaired_pending_tasks(state, now_seconds());
    // 第 6 轮误删事故（100女友 ep10 已播任务被删）后，桌面撤销"无网络
    // 清理"：reconcile_unaired_anilist_next_tasks 按条目 nextAiringEpisode
    // 判定 episode >= next 即假票，但桌面 next 可能仍被离线锚点污染
    // （污染 next=ep10@9/9 时，真实已播 ep10 任务被误删，且 seenAiringEvents
    // 已含该 id、AIRING_QUERY 窗口永不重建）。桌面任务纠偏/回填改由
    // anilist_authority_refresh（权威 next 重写 + schedule 回填）承担；
    // 该清理仅保留给 Android（mobile merge_status，Java 提供的 next 同
    // 口径），见函数头说明。
    // 权威数据修复：eps 越界 pending 与共享 anilistId 非主条目 pending 清理
    // （completed 历史一律保留），幂等。
    reconcile_anilist_authority_tasks(state, map);
    // 第 8 轮问题 3：追番前已播历史 pending 洪水清理（幂等，云端合并进来
    // 的存量假票当次清掉，上传文档自愈）。
    purge_pre_follow_pending_tasks(state);
    let after = (
        state.get("following").cloned().unwrap_or(Value::Null),
        state.get("tasks").cloned().unwrap_or(Value::Null),
        state.get("syncMetadata").cloned().unwrap_or(Value::Null),
    );
    before != after
}

/// `bangumi_resolve_mapping` 的纯逻辑：不修改状态，只产出命令契约载荷。
#[cfg(feature = "standard")]
fn resolve_mapping_entry(state: &Value, map: &Value, anime_id: i64) -> Value {
    let entry = state["following"].as_array().and_then(|items| {
        items
            .iter()
            .find(|item| value_i64(item.get("id")) == anime_id)
    });
    let Some(entry) = entry else {
        return json!({"status": "unavailable", "subjectId": Value::Null, "candidates": [], "anime": Value::Null});
    };
    let anime = json!({
        "id": entry.get("id").cloned().unwrap_or(Value::Null),
        "displayTitle": entry.get("displayTitle").cloned().unwrap_or(Value::Null),
        "seasonYear": entry.get("seasonYear").cloned().unwrap_or(Value::Null),
        "format": entry.get("format").cloned().unwrap_or(Value::Null),
        "coverImage": entry.get("coverImage").cloned().unwrap_or(Value::Null),
    });
    let already_mapped = value_string(entry.get("source")) == "bangumi"
        || entry
            .get("mapping")
            .filter(|value| !value.is_null())
            .is_some_and(|mapping| {
                value_string(mapping.get("method")) == "manual"
                    && value_i64(entry.get("bangumiId")) > 0
            });
    if already_mapped {
        return json!({
            "status": "mapped",
            "subjectId": entry.get("bangumiId").cloned().unwrap_or_else(|| entry.get("id").cloned().unwrap_or(Value::Null)),
            "candidates": [],
            "anime": anime,
        });
    }
    match bangumi::resolve_mapping_candidates(map, entry) {
        bangumi::MappingResolution::Mapped { subject_id, .. } => {
            json!({"status": "mapped", "subjectId": subject_id, "candidates": [], "anime": anime})
        }
        bangumi::MappingResolution::Candidates(candidates) => {
            let candidates: Vec<Value> = candidates
                .iter()
                .map(|candidate| {
                    json!({
                        "subjectId": candidate.subject_id,
                        "name": candidate.name,
                        "nameCn": candidate.name_cn,
                        "date": candidate.date,
                        "platform": candidate.platform,
                        "begin": candidate.begin,
                        "score": candidate.score,
                    })
                })
                .collect();
            json!({"status": "pending", "subjectId": Value::Null, "candidates": candidates, "anime": anime})
        }
        bangumi::MappingResolution::None => {
            json!({"status": "pending", "subjectId": Value::Null, "candidates": [], "anime": anime})
        }
    }
}

fn record_timestamp(record: &Value, fallback: &str) -> i64 {
    let explicit = value_i64(record.get("syncUpdatedAt"));
    if explicit > 0 {
        explicit
    } else {
        value_i64(record.get(fallback)) * 1000
    }
}

fn stable_record(record: &Value) -> String {
    let Some(object) = record.as_object() else {
        return String::new();
    };
    let ordered: BTreeMap<&String, &Value> = object.iter().collect();
    serde_json::to_string(&ordered).unwrap_or_default()
}

fn choose_record(left: Option<&Value>, right: Option<&Value>, fallback: &str) -> Option<Value> {
    match (left, right) {
        (None, None) => None,
        (Some(value), None) | (None, Some(value)) => Some(value.clone()),
        (Some(left), Some(right)) => {
            let left_time = record_timestamp(left, fallback);
            let right_time = record_timestamp(right, fallback);
            if left_time != right_time {
                Some(if left_time > right_time { left } else { right }.clone())
            } else {
                Some(
                    if stable_record(left) >= stable_record(right) {
                        left
                    } else {
                        right
                    }
                    .clone(),
                )
            }
        }
    }
}

fn tombstones(value: Option<&Value>) -> BTreeMap<String, i64> {
    value
        .and_then(Value::as_object)
        .into_iter()
        .flat_map(|object| object.iter())
        .filter_map(|(id, timestamp)| {
            let id_number = id.parse::<i64>().ok()?;
            let timestamp = timestamp.as_i64()?;
            (id_number > 0 && timestamp > 0).then_some((id_number.to_string(), timestamp))
        })
        .collect()
}

fn document_from_state(state: &mut Value) -> Value {
    ensure_sync_metadata(state);
    let mut following = state["following"].as_array().cloned().unwrap_or_default();
    following.retain(|item| {
        value_i64(item.get("id")) > 0 && item.get("title").is_some_and(Value::is_object)
    });
    following.sort_by_key(|item| value_i64(item.get("id")));
    // 第 8 轮问题 1（治本）：nextAiringEpisode 转设备本地数据，不再进坚果云
    // 文档。导出时剥键 → 本端上传文档干净；读取侧 normalize_document 走同一
    // 函数 → 云端文档携带的旧污染 next（黄泉 ep24@9/13 等）在参与合并前就
    // 被剥离，webdav LWW 平局不再翻盘、本端权威重写后也不会被云端旧值复活。
    // 各设备 next 由本端调度管道/权威 refresh（桌面）/Java Worker（Android）
    // 自行维护；SYNC_VERSION 保持 1（v0.6 上传的带 next 文档读取侧忽略）。
    for item in &mut following {
        if let Some(object) = item.as_object_mut() {
            object.remove("nextAiringEpisode");
        }
    }
    let mut tasks = state["tasks"].as_array().cloned().unwrap_or_default();
    tasks.retain(|task| {
        !value_string(task.get("id")).is_empty()
            && value_i64(task.get("animeId")) > 0
            && value_i64(task.get("episode")) > 0
            && matches!(
                value_string(task.get("status")).as_str(),
                "pending" | "completed"
            )
    });
    tasks.sort_by_key(|task| value_string(task.get("id")));
    let deleted = tombstones(state["syncMetadata"].get("followingDeletedAt"));
    let updated_at = following
        .iter()
        .map(|item| record_timestamp(item, "followedAt"))
        .chain(tasks.iter().map(|task| record_timestamp(task, "createdAt")))
        .chain(deleted.values().copied())
        .max()
        .unwrap_or(0);
    json!({"version": SYNC_VERSION, "updatedAt": updated_at, "following": following, "tasks": tasks, "followingDeletedAt": deleted})
}

fn normalize_document(document: &Value) -> anyhow::Result<Value> {
    if value_i64(document.get("version")) != SYNC_VERSION {
        return Err(anyhow!("WebDAV 同步文件版本不受支持"));
    }
    let mut state = json!({
        "following": document.get("following").and_then(Value::as_array).cloned().unwrap_or_default(),
        "tasks": document.get("tasks").and_then(Value::as_array).cloned().unwrap_or_default(),
        "syncMetadata": {"followingDeletedAt": tombstones(document.get("followingDeletedAt"))}
    });
    Ok(document_from_state(&mut state))
}

fn comparable_document(document: &Value) -> anyhow::Result<String> {
    let normalized = normalize_document(document)?;
    Ok(serde_json::to_string(
        &json!({"following": normalized["following"], "tasks": normalized["tasks"], "followingDeletedAt": normalized["followingDeletedAt"]}),
    )?)
}

fn merge_document_into_state(
    state: &mut Value,
    remote: &Value,
) -> anyhow::Result<(bool, Value, bool)> {
    // 第 8 轮问题 1：合并前快照本地 nextAiringEpisode（设备本地数据）。
    // document_from_state 导出已剥键，故 local/remote 两份合并底稿都不含该
    // 键——incoming next 全程不参与合并（实现方式选"合并后从本地 pre-merge
    // 快照恢复"，因为剥键后合并产物天然无 next，必须恢复本地值）；合并落盘
    // 后按条目 id 原样还原（含显式 null；远端复活记录无本地 next → 保持无
    // 键，由本端调度/权威路径重建）。
    let local_next: HashMap<i64, Option<Value>> = state["following"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|item| {
            (
                value_i64(item.get("id")),
                item.get("nextAiringEpisode").cloned(),
            )
        })
        .collect();
    let before = comparable_document(&document_from_state(state))?;
    let local = document_from_state(state);
    let remote = normalize_document(remote)?;
    let mut deleted = tombstones(local.get("followingDeletedAt"));
    for (id, timestamp) in tombstones(remote.get("followingDeletedAt")) {
        deleted
            .entry(id)
            .and_modify(|value| *value = (*value).max(timestamp))
            .or_insert(timestamp);
    }
    let local_following: HashMap<i64, Value> = local["following"]
        .as_array()
        .unwrap()
        .iter()
        .cloned()
        .map(|item| (value_i64(item.get("id")), item))
        .collect();
    let remote_following: HashMap<i64, Value> = remote["following"]
        .as_array()
        .unwrap()
        .iter()
        .cloned()
        .map(|item| (value_i64(item.get("id")), item))
        .collect();
    let ids: HashSet<i64> = local_following
        .keys()
        .chain(remote_following.keys())
        .copied()
        .chain(deleted.keys().filter_map(|id| id.parse().ok()))
        .collect();
    let mut following = Vec::new();
    for id in ids {
        let local_record = local_following.get(&id);
        let deleted_at = *deleted.get(&id.to_string()).unwrap_or(&0);
        let local_is_refollow = local_record.is_some_and(|item| {
            let intent_at = value_i64(item.get("localFollowIntentAt"));
            let remote_at = value_i64(item.get("lastPulledFromBangumiAt"));
            intent_at > 0
                && (intent_at > deleted_at || remote_at <= 0 || intent_at > remote_at.saturating_mul(1_000))
        });
        let winner = if local_is_refollow {
            local_following.get(&id).cloned()
        } else {
            choose_record(
                local_following.get(&id),
                remote_following.get(&id),
                "followedAt",
            )
        };
        if let Some(winner) = winner {
            let deleted_at = *deleted.get(&id.to_string()).unwrap_or(&0);
            let local_refollow = {
                    let intent_at = value_i64(winner.get("localFollowIntentAt"));
                    let remote_at = value_i64(winner.get("lastPulledFromBangumiAt"));
                    intent_at > 0
                        && (intent_at > deleted_at
                            || remote_at <= 0
                            || intent_at > remote_at.saturating_mul(1_000))
                };
            if local_refollow {
                // An explicit local re-follow is a new user intent.  A stale
                // WebDAV tombstone must not erase it; clear the tombstone so
                // every device converges to the live record.
                deleted.remove(&id.to_string());
                following.push(winner);
            } else if record_timestamp(&winner, "followedAt") > deleted_at {
                following.push(winner);
            }
        }
    }
    following.sort_by_key(|item| value_i64(item.get("id")));
    let local_tasks: HashMap<String, Value> = local["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .cloned()
        .map(|item| (value_string(item.get("id")), item))
        .collect();
    let remote_tasks: HashMap<String, Value> = remote["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .cloned()
        .map(|item| (value_string(item.get("id")), item))
        .collect();
    let task_ids: HashSet<String> = local_tasks
        .keys()
        .chain(remote_tasks.keys())
        .cloned()
        .collect();
    let followed: HashMap<i64, String> = following
        .iter()
        .map(|item| {
            (
                value_i64(item.get("id")),
                value_string(item.get("displayTitle")),
            )
        })
        .collect();
    let mut tasks = Vec::new();
    for id in task_ids {
        if let Some(mut winner) =
            choose_record(local_tasks.get(&id), remote_tasks.get(&id), "createdAt")
        {
            let anime_id = value_i64(winner.get("animeId"));
            if !followed.contains_key(&anime_id) && value_string(winner.get("status")) == "pending"
            {
                continue;
            }
            if let Some(title) = followed.get(&anime_id) {
                winner["animeTitle"] = json!(title);
            }
            tasks.push(winner);
        }
    }
    tasks.sort_by(|left, right| {
        value_i64(right.get("airingAt"))
            .cmp(&value_i64(left.get("airingAt")))
            .then_with(|| value_string(left.get("id")).cmp(&value_string(right.get("id"))))
    });
    state["following"] = json!(following);
    // 第 8 轮问题 1：恢复设备本地 next（快照原样还原，云端污染值进不来）。
    if let Some(items) = state["following"].as_array_mut() {
        for item in items.iter_mut() {
            let id = value_i64(item.get("id"));
            if let Some(Some(next)) = local_next.get(&id) {
                item["nextAiringEpisode"] = next.clone();
            }
        }
    }
    state["tasks"] = json!(tasks);
    state["syncMetadata"]["followingDeletedAt"] = json!(deleted);
    let merged = document_from_state(state);
    Ok((
        before != comparable_document(&merged)?,
        merged.clone(),
        comparable_document(&remote)? != comparable_document(&merged)?,
    ))
}

const SEASON_QUERY: &str = r#"query SeasonAnime($season: MediaSeason, $year: Int, $page: Int) {
  Page(page: $page, perPage: 50) { pageInfo { lastPage }
    media(type: ANIME, season: $season, seasonYear: $year, status_not: CANCELLED, isAdult: false, sort: [POPULARITY_DESC]) {
      id title { native romaji english } coverImage { extraLarge medium color } bannerImage description(asHtml: false)
      format episodes duration status season seasonYear startDate { year month day } studios(isMain: true) { nodes { name } }
      genres averageScore popularity nextAiringEpisode { episode airingAt timeUntilAiring }
      airingSchedule(notYetAired: true, perPage: 50) { nodes { episode airingAt } } siteUrl
    }
  }
}"#;

#[cfg(not(target_os = "android"))]
const AIRING_QUERY: &str = r#"query AiredEpisodes($ids: [Int], $from: Int, $to: Int, $page: Int) {
  Page(page: $page, perPage: 50) { pageInfo { hasNextPage }
    airingSchedules(mediaId_in: $ids, airingAt_greater: $from, airingAt_lesser: $to, sort: TIME) {
      id mediaId episode airingAt media { id title { native romaji english } coverImage { medium }
      episodes nextAiringEpisode { episode airingAt timeUntilAiring } }
    }
  }
}"#;

fn season_cache_ttl_millis(season: &str, year: i64, current_year: i64, current_month: u32) -> i64 {
    let season_end_month = match season {
        "WINTER" => 3,
        "SPRING" => 6,
        "SUMMER" => 9,
        "FALL" => 12,
        _ => return 0,
    };
    let historical =
        year < current_year || (year == current_year && current_month > season_end_month);
    if historical {
        30 * 86_400_000
    } else {
        6 * 3_600_000
    }
}

async fn anilist_request(
    context: &AppContext,
    query: &str,
    variables: Value,
) -> anyhow::Result<Value> {
    anilist_request_at(&context.client, ANILIST_API, query, variables).await
}

/// 指定端点的 AniList GraphQL 请求（问题 D：季度链的 AniList 补充覆盖需要把
/// 端点指向可注入的 mock/官方地址）。
async fn anilist_request_at(
    client: &reqwest::Client,
    endpoint: &str,
    query: &str,
    variables: Value,
) -> anyhow::Result<Value> {
    if endpoint.trim_end_matches('/') == ANILIST_API {
        loop {
            let delay = {
                let mut window = ANILIST_REQUEST_WINDOW.lock().await;
                let now = Instant::now();
                while window.front().is_some_and(|timestamp| {
                    now.duration_since(*timestamp) >= Duration::from_secs(60)
                }) {
                    window.pop_front();
                }
                if window.len() < ANILIST_SAFE_REQUESTS_PER_MINUTE {
                    window.push_back(now);
                    None
                } else {
                    window.front().map(|timestamp| {
                        Duration::from_secs(60).saturating_sub(now.duration_since(*timestamp))
                    })
                }
            };
            if let Some(delay) = delay {
                tokio::time::sleep(delay).await;
            } else {
                break;
            }
        }
    }
    let response = client
        .post(endpoint)
        .header(CONTENT_TYPE, "application/json")
        .header(ACCEPT, "application/json, multipart/mixed")
        .header(ORIGIN, "https://anilist.co")
        .header(REFERER, "https://anilist.co/")
        .header(
            reqwest::header::USER_AGENT,
            "AniLog/0.7 (https://github.com/SH1N15/anilog-tracker)",
        )
        .json(&json!({"query": query, "variables": variables}))
        .send()
        .await?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        let summary = body
            .replace(['\r', '\n', '\t'], " ")
            .chars()
            .take(240)
            .collect::<String>();
        let endpoint_label = endpoint
            .split('?')
            .next()
            .unwrap_or(endpoint)
            .trim_end_matches('/');
        return Err(anyhow!(
            "AniList 请求失败（endpoint={endpoint_label}, HTTP {status}, body={summary})"
        ));
    }
    let payload: Value = response.json().await?;
    if let Some(error) = payload["errors"]
        .as_array()
        .and_then(|errors| errors.first())
    {
        return Err(anyhow!("{}", value_string(error.get("message"))));
    }
    Ok(payload["data"].clone())
}

fn season_cache_path(context: &AppContext, season: &str, year: i64) -> PathBuf {
    context.cache_dir.join(format!("{year}-{season}.json"))
}

/// 任务 4（本批只做类型与序列化兼容，季度链不切换）：standard 版 Anime 对象
/// 允许携带 `source`/`bangumiSubjectId`/`anilistId`。AniList 路径补默认值；
/// 已带这些键的记录（未来 Bangumi 季度链写入）原样保留。original 不改写。
fn annotate_anime_sources(mut anime: Vec<Value>, original: bool) -> Vec<Value> {
    #[cfg(feature = "standard")]
    if !original {
        for entry in anime.iter_mut() {
            if entry.get("source").is_none() {
                entry["source"] = json!("anilist");
            }
            if entry.get("bangumiSubjectId").is_none() {
                entry["bangumiSubjectId"] = Value::Null;
            }
            if entry.get("anilistId").is_none() {
                entry["anilistId"] = entry.get("id").cloned().unwrap_or(Value::Null);
            }
        }
        return anime;
    }
    let _ = &mut anime;
    let _ = original;
    anime
}

async fn fetch_season_network(
    context: &AppContext,
    season: &str,
    year: i64,
) -> anyhow::Result<Vec<Value>> {
    let first = anilist_request(
        context,
        SEASON_QUERY,
        json!({"season": season, "year": year, "page": 1}),
    )
    .await?;
    let page = &first["Page"];
    let last_page = value_i64(page["pageInfo"].get("lastPage")).clamp(1, 5);
    let mut all = page["media"].as_array().cloned().unwrap_or_default();
    for page_number in 2..=last_page {
        let next = anilist_request(
            context,
            SEASON_QUERY,
            json!({"season": season, "year": year, "page": page_number}),
        )
        .await?;
        all.extend(
            next["Page"]["media"]
                .as_array()
                .cloned()
                .unwrap_or_default(),
        );
    }
    Ok(all)
}

/// 现有 AniList 季度路径（原 fetch_season 逻辑原样抽取，行为零变化）：
/// 读 `season-cache/{year}-{SEASON}.json`（TTL 见 season_cache_ttl_millis），
/// 未命中则分页拉取并写缓存。返回 `(anime, fetchedAt 毫秒, 是否缓存命中)`。
/// standard 版 Bangumi 主链失败时的回落路径也走这里（见
/// fetch_season_bangumi_chain 的 AniListFallback 分支）。
async fn fetch_season_anilist_cached(
    context: &AppContext,
    season: &str,
    year: i64,
) -> anyhow::Result<(Vec<Value>, i64, bool)> {
    let cache_path = season_cache_path(context, season, year);
    if let Ok(body) = fs::read_to_string(&cache_path) {
        if let Ok(entry) = serde_json::from_str::<Value>(&body) {
            let age = now_millis() - value_i64(entry.get("fetchedAt"));
            let today = Local::now();
            let ttl =
                season_cache_ttl_millis(&season, year, i64::from(today.year()), today.month());
            if entry.get("version") == Some(&json!(CACHE_VERSION))
                && entry["anime"].is_array()
                && age < ttl
            {
                return Ok((
                    annotate_anime_sources(
                        entry["anime"].as_array().cloned().unwrap_or_default(),
                        context.original,
                    ),
                    value_i64(entry.get("fetchedAt")),
                    true,
                ));
            }
        }
    }
    let anime = annotate_anime_sources(
        fetch_season_network(context, season, year).await?,
        context.original,
    );
    let fetched_at = now_millis();
    let entry = json!({"version": CACHE_VERSION, "season": season, "year": year, "fetchedAt": fetched_at, "anime": anime});
    let temporary = cache_path.with_extension("json.tmp");
    fs::write(&temporary, serde_json::to_vec(&entry)?)?;
    fs::rename(temporary, &cache_path)?;
    Ok((anime, fetched_at, false))
}

#[tauri::command]
async fn fetch_season(
    app: AppHandle,
    context: State<'_, AppContext>,
    params: Value,
) -> Result<Vec<Value>, String> {
    let season = value_string(params.get("season"));
    let year = value_i64(params.get("year"));
    if !["WINTER", "SPRING", "SUMMER", "FALL"].contains(&season.as_str()) || year < 1900 {
        return Err("无效的季度参数".into());
    }
    // standard 版季度主链（Phase 2）：Bangumi /v0/subjects 分页 → 映射 →
    // bangumi-cache → 失败回落过期缓存 → 连缓存都没有再回落下方 AniList
    // 原路径（原逻辑零变化）。original 不进入该分支，行为完全不变。
    #[cfg(feature = "standard")]
    if !context.original {
        let (state_snapshot, base) = {
            let state = context.state.lock().map_err(|_| "状态锁不可用")?;
            let base = bangumi_base_urls(&state);
            (state.clone(), base)
        };
        let cache_dir = bangumi_cache_dir(&context);
        // Cache-first season loading: an expired but valid snapshot is useful
        // immediately.  Refresh in the background and notify the UI when the
        // new snapshot is ready instead of blocking the first screen for all
        // month pages plus AniList enrichment.
        if let Some((cached_anime, cached_at)) =
            read_bangumi_season_cache(&cache_dir.join(format!("{year}-{season}.json")), false)
        {
            if now_millis().saturating_sub(cached_at) >= BANGUMI_SEASON_TTL_MILLIS {
                let stale_anime = normalize_season_anime_episode_authority(
                    &cache_dir,
                    cached_anime,
                    now_seconds(),
                );
                let refresh_context = context.inner().clone();
                let refresh_app = app.clone();
                let refresh_season = season.clone();
                tauri::async_runtime::spawn(async move {
                    let state_snapshot = match refresh_context.state.lock() {
                        Ok(state) => state.clone(),
                        Err(_) => return,
                    };
                    let base = bangumi_base_urls(&state_snapshot);
                    let source = AniListSeasonSource {
                        client: &refresh_context.client,
                        endpoint: ANILIST_API,
                    };
                    if let SeasonFetch::Bangumi {
                        anime,
                        fetched_at,
                        stale,
                    } = fetch_season_bangumi_chain(
                        &refresh_context.client,
                        base,
                        &cache_dir,
                        &refresh_context.offline_bangumi,
                        &state_snapshot,
                        &refresh_season,
                        year,
                        Some(&source),
                    )
                    .await
                    {
                        let _ = refresh_app.emit(
                            "season-updated",
                            json!({
                                "season": refresh_season,
                                "year": year,
                                "anime": anime,
                                "fetchedAt": fetched_at,
                                "stale": stale
                            }),
                        );
                    }
                });
                return Ok(stale_anime);
            }
        }
        let anilist_source = AniListSeasonSource {
            client: &context.client,
            endpoint: ANILIST_API,
        };
        match fetch_season_bangumi_chain(
            &context.client,
            base,
            &cache_dir,
            &context.offline_bangumi,
            &state_snapshot,
            &season,
            year,
            Some(&anilist_source),
        )
        .await
        {
            SeasonFetch::Bangumi {
                anime,
                fetched_at,
                stale,
            } => {
                let _ = app.emit(
                    "season-updated",
                    json!({"season": season, "year": year, "anime": anime, "fetchedAt": fetched_at, "stale": stale}),
                );
                return Ok(anime);
            }
            // 回落说明：Bangumi 网络失败且本地无（过期）缓存可兜底时，
            // 回落现有 AniList 季度路径（原逻辑不动，见
            // fetch_season_anilist_cached）。该回落仅在 standard 版发生。
            SeasonFetch::AniListFallback => {
                warn!("Bangumi 季度链不可用且无缓存兜底，回落 AniList 季度路径（{season} {year}）");
            }
        }
    }
    let (anime, fetched_at, cached) = fetch_season_anilist_cached(&context, &season, year)
        .await
        .map_err(|error| error.to_string())?;
    if !cached {
        let _ = app.emit(
            "season-updated",
            json!({"season": season, "year": year, "anime": anime, "fetchedAt": fetched_at}),
        );
    }
    Ok(anime)
}

// ---------------------------------------------------------------------------
// Phase 2 任务 1：standard 版季度主链（Bangumi /v0/subjects 分页）。
// 链路：bangumi-cache 命中（TTL 24h）→ 三个月逐月分页（limit=50、最多 10 页/
// 月，经 HttpBangumiClient 的 Semaphore(2) 串行化）→ subjectId 合并去重 →
// map_subjects_to_anime 纯函数转换 → 写缓存。网络失败 → 过期缓存兜底（stale）；
// 连缓存都没有 → 回落现有 AniList 季度路径（SeasonFetch::AniListFallback）。
// ---------------------------------------------------------------------------

/// 季度列表缓存 TTL（schema §7：24h；按整季一份缓存，不再按月+页拆分落盘）。
#[cfg(feature = "standard")]
const BANGUMI_SEASON_TTL_MILLIS: i64 = 24 * 3_600_000;
/// 单个月份的最大分页数（limit=50 → 单月最多 500 条，防失控拉取）。
#[cfg(feature = "standard")]
const BANGUMI_SEASON_MAX_PAGES_PER_MONTH: usize = 10;

/// 问题 D ①：AniList 补充覆盖查询（按 id_in 批量，分页 ≤3 页）。
#[cfg(feature = "standard")]
const SEASON_ANILIST_ENRICH_QUERY: &str = r#"query SeasonAniListEnrich($ids: [Int], $page: Int) {
  Page(page: $page, perPage: 50) { pageInfo { lastPage }
    media(id_in: $ids, type: ANIME) {
      id status episodes duration genres averageScore bannerImage
      studios(isMain: true) { nodes { name } }
      nextAiringEpisode { episode airingAt timeUntilAiring }
      airingSchedule(notYetAired: true, perPage: 50) { nodes { episode airingAt } }
    }
  }
}"#;

/// AniList 补充覆盖的请求来源（生产 = AppContext 客户端 + 官方 GraphQL 端点；
/// 测试 = mock 服务器），使季度链可注入而不触网。
#[cfg(feature = "standard")]
struct AniListSeasonSource<'a> {
    client: &'a reqwest::Client,
    endpoint: &'a str,
}

/// 问题 D ①：季度链内 AniList 补充覆盖。对条目 anilistId（离线映射反查）批量
/// 查询 AniList：nextAiringEpisode 用 AniList 值（权威；status FINISHED/CANCELLED
/// 或 AniList 无下一期 → null，修正 bangumi-data 平台首播规则造成的错误星期/
/// 已完结番仍有下集）；airingSchedule 填入（前端星期分组依赖）；duration/
/// studios/genres/bannerImage/status/episodes/averageScore 只补缺不覆盖。
/// 查询失败静默保留 bangumi-data 计算值。
#[cfg(feature = "standard")]
async fn anilist_enrich_season_anime(
    source: &AniListSeasonSource<'_>,
    mut anime: Vec<Value>,
) -> Vec<Value> {
    let ids: Vec<i64> = anime
        .iter()
        .map(|item| value_i64(item.get("anilistId")))
        .filter(|id| *id > 0)
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    if ids.is_empty() {
        return anime;
    }
    let mut media_by_id: HashMap<i64, Value> = HashMap::new();
    for page in 1..=3usize {
        let data = match anilist_request_at(
            source.client,
            source.endpoint,
            SEASON_ANILIST_ENRICH_QUERY,
            json!({"ids": ids, "page": page}),
        )
        .await
        {
            Ok(data) => data,
            Err(error) => {
                warn!("AniList 季度补充覆盖失败（静默保留 bangumi-data 值）：{error}");
                return anime;
            }
        };
        let last_page = value_i64(data["Page"]["pageInfo"].get("lastPage")).clamp(1, 3) as usize;
        for media in data["Page"]["media"]
            .as_array()
            .cloned()
            .unwrap_or_default()
        {
            let id = value_i64(media.get("id"));
            if id > 0 {
                media_by_id.insert(id, media);
            }
        }
        if page >= last_page {
            break;
        }
    }
    if media_by_id.is_empty() {
        return anime;
    }
    for item in anime.iter_mut() {
        let anilist_id = value_i64(item.get("anilistId"));
        let Some(media) = media_by_id.get(&anilist_id) else {
            continue;
        };
        // nextAiringEpisode：AniList 权威；完结/取消或无下一期 → null。
        let status = value_string(media.get("status"));
        let finished = matches!(status.as_str(), "FINISHED" | "CANCELLED");
        let next = media
            .get("nextAiringEpisode")
            .cloned()
            .unwrap_or(Value::Null);
        item["nextAiringEpisode"] = if finished || next.is_null() {
            Value::Null
        } else {
            next
        };
        // airingSchedule：前端星期分组依赖；AniList 无数据时保留原值。
        let schedule = media.get("airingSchedule").cloned().unwrap_or(Value::Null);
        if schedule["nodes"].is_array() && !schedule["nodes"].as_array().unwrap().is_empty() {
            item["airingSchedule"] = schedule;
        }
        // 补充字段：只补缺（null / 空数组 / 空串），不覆盖 bangumi 已有值。
        let fill = |item: &mut Value, key: &str, value: &Value| {
            let missing = match item.get(key) {
                None | Some(Value::Null) => true,
                Some(Value::Array(items)) => items.is_empty(),
                Some(Value::String(text)) => text.is_empty(),
                _ => false,
            };
            if missing && !value.is_null() {
                item[key] = value.clone();
            }
        };
        fill(item, "episodes", &media["episodes"]);
        fill(item, "duration", &media["duration"]);
        fill(item, "genres", &media["genres"]);
        fill(item, "averageScore", &media["averageScore"]);
        fill(item, "status", &media["status"]);
        fill(item, "bannerImage", &media["bannerImage"]);
        if media["studios"]["nodes"]
            .as_array()
            .is_some_and(|nodes| !nodes.is_empty())
        {
            fill(item, "studios", &media["studios"]);
        }
    }
    anime
}

/// Bangumi 专属缓存目录（季度列表 / subject extras，schema §7）。放在
/// cache_dir 下以纳入 clear_cache 的清理范围。
#[cfg(feature = "standard")]
fn bangumi_cache_dir(context: &AppContext) -> PathBuf {
    let dir = context.cache_dir.join("bangumi-cache");
    let _ = fs::create_dir_all(&dir);
    dir
}

/// 季度主链的结果来源。
#[cfg(feature = "standard")]
enum SeasonFetch {
    /// Bangumi 数据（缓存命中 / 网络刷新 / 过期缓存 stale 兜底）。
    Bangumi {
        anime: Vec<Value>,
        /// 毫秒时间戳（与 AniList 缓存条目的 fetchedAt 单位一致）。
        fetched_at: i64,
        /// true = 网络失败后回落的过期缓存（stale 兜底）。
        stale: bool,
    },
    /// Bangumi 网络失败且无任何缓存 → 回落现有 AniList 季度路径。
    AniListFallback,
}

/// standard 版季度主链核心（测试经 MockBangumiServer 直调；`state` 为
/// context.state 快照，用于读 preferredBroadcastSites）。`anilist` 为问题 D ①
/// 的 AniList 补充覆盖来源（生产注入官方端点，测试可注入 mock 或 None 跳过）。
#[cfg(feature = "standard")]
async fn fetch_season_bangumi_chain(
    client: &reqwest::Client,
    base: bangumi::BangumiBaseUrls,
    cache_dir: &Path,
    offline_map: &Value,
    state: &Value,
    season: &str,
    year: i64,
    anilist: Option<&AniListSeasonSource<'_>>,
) -> SeasonFetch {
    let cache_path = cache_dir.join(format!("{year}-{season}.json"));
    // 1. 缓存命中（TTL 24h）：直接返回。
    if let Some((anime, fetched_at)) = read_bangumi_season_cache(&cache_path, true) {
        return SeasonFetch::Bangumi {
            anime: normalize_season_anime_episode_authority(cache_dir, anime, now_seconds()),
            fetched_at,
            stale: false,
        };
    }
    // 2. 网络刷新：三个月逐月分页拉取。
    let http = bangumi::HttpBangumiClient::new(client.clone(), base);
    match fetch_season_bangumi_subjects(&http, season, year).await {
        Ok(subjects) => {
            let preferred = preferred_broadcast_sites(state);
            let mut anime = map_subjects_to_anime(
                &subjects,
                offline_map,
                &preferred,
                season,
                year,
                now_seconds(),
            );
            // 问题 D ①：AniList 补充覆盖（nextAiringEpisode 权威 / airingSchedule
            // 填入 / 补充字段；失败静默保留 bangumi-data 值）。随缓存一并落盘。
            if let Some(source) = anilist {
                anime = anilist_enrich_season_anime(source, anime).await;
            }
            anime = normalize_season_anime_episode_authority(cache_dir, anime, now_seconds());
            let fetched_at = now_millis();
            let entry = json!({
                "version": CACHE_VERSION, "season": season, "year": year,
                "source": "bangumi", "fetchedAt": fetched_at, "anime": anime
            });
            let temporary = cache_path.with_extension("json.tmp");
            if let Ok(body) = serde_json::to_vec(&entry) {
                if fs::write(&temporary, &body).is_ok() {
                    let _ = fs::rename(temporary, &cache_path);
                }
            }
            SeasonFetch::Bangumi {
                anime,
                fetched_at,
                stale: false,
            }
        }
        Err(error) => {
            // 3. stale 兜底：网络失败时回读过期缓存（缓存条目自带 fetchedAt 标注
            //    数据时点，前端 season-updated 事件附加 stale=true）。
            warn!("Bangumi 季度拉取失败（{season} {year}）：{error}；尝试过期缓存兜底");
            if let Some((anime, fetched_at)) = read_bangumi_season_cache(&cache_path, false) {
                return SeasonFetch::Bangumi {
                    anime: normalize_season_anime_episode_authority(
                        cache_dir,
                        anime,
                        now_seconds(),
                    ),
                    fetched_at,
                    stale: true,
                };
            }
            // 4. 连缓存都没有 → 回落现有 AniList 季度路径。
            SeasonFetch::AniListFallback
        }
    }
}

/// 单个月份的分页拉取（每月最多 10 页；月内串行翻页）。
#[cfg(feature = "standard")]
async fn fetch_season_month_subjects(
    http: &bangumi::HttpBangumiClient,
    year: i64,
    month: u32,
) -> Result<Vec<bangumi::BangumiSubject>, String> {
    let mut subjects = Vec::new();
    let mut offset = 0u32;
    for _ in 0..BANGUMI_SEASON_MAX_PAGES_PER_MONTH {
        let page = http
            .get_season_subjects(
                year as u32,
                month,
                bangumi::SEASON_SUBJECTS_LIMIT_MAX,
                offset,
            )
            .await
            .map_err(|error| error.to_string())?;
        let count = page.data.len() as u32;
        subjects.extend(page.data);
        if count == 0 {
            break;
        }
        offset += count;
        if page.total > 0 && offset >= page.total {
            break;
        }
        if (count as usize) < bangumi::SEASON_SUBJECTS_LIMIT_MAX as usize {
            break;
        }
    }
    Ok(subjects)
}

/// `GET {v0}/subjects?type=2&year&month&limit=50&offset=`：该季 3 个月**并发**
/// 拉取（问题 D：tokio::join! 三月并行，由 HttpBangumiClient 内部
/// Semaphore(2) 全局限流，并发不超 2），每月内串行翻页；按 subjectId 合并
/// 去重（跨月边界条目去重）。任一月失败 → 整季失败（走 stale 兜底）。
#[cfg(feature = "standard")]
async fn fetch_season_bangumi_subjects(
    http: &bangumi::HttpBangumiClient,
    season: &str,
    year: i64,
) -> Result<Vec<bangumi::BangumiSubject>, String> {
    let [month_a, month_b, month_c] = bangumi::season_months(season);
    let mut results: [Option<Result<Vec<bangumi::BangumiSubject>, String>>; 3] = [None, None, None];
    // 三月并行（tokio 未启用 macros feature，手写 poll 连接；均共享 &http，
    // 由 HttpBangumiClient 内部 Semaphore(2) 限流）。
    let mut future_a = std::pin::pin!(fetch_season_month_subjects(http, year, month_a));
    let mut future_b = std::pin::pin!(fetch_season_month_subjects(http, year, month_b));
    let mut future_c = std::pin::pin!(fetch_season_month_subjects(http, year, month_c));
    let joined = std::future::poll_fn(|cx| {
        if results[0].is_none() {
            if let std::task::Poll::Ready(value) = future_a.as_mut().poll(cx) {
                results[0] = Some(value);
            }
        }
        if results[1].is_none() {
            if let std::task::Poll::Ready(value) = future_b.as_mut().poll(cx) {
                results[1] = Some(value);
            }
        }
        if results[2].is_none() {
            if let std::task::Poll::Ready(value) = future_c.as_mut().poll(cx) {
                results[2] = Some(value);
            }
        }
        if results.iter().all(Option::is_some) {
            let values: Vec<_> = results
                .iter_mut()
                .map(|slot| slot.take().expect("joined month result"))
                .collect();
            std::task::Poll::Ready(values)
        } else {
            std::task::Poll::Pending
        }
    })
    .await;
    let mut subjects = Vec::new();
    let mut seen: HashSet<i64> = HashSet::new();
    for result in joined {
        for subject in result? {
            if seen.insert(subject.id) {
                subjects.push(subject);
            }
        }
    }
    Ok(subjects)
}

/// 读取季度缓存条目（`{version, season, year, source, fetchedAt, anime}`）。
/// `fresh_only=true` 时仅 TTL 内有效；false 时任何年龄的合法条目都返回
/// （stale 兜底）。条目自带 fetchedAt（毫秒）作为数据时点标注。
#[cfg(feature = "standard")]
fn read_bangumi_season_cache(cache_path: &Path, fresh_only: bool) -> Option<(Vec<Value>, i64)> {
    let body = fs::read_to_string(cache_path).ok()?;
    let entry = serde_json::from_str::<Value>(&body).ok()?;
    if entry.get("version") != Some(&json!(CACHE_VERSION)) || !entry["anime"].is_array() {
        return None;
    }
    let fetched_at = value_i64(entry.get("fetchedAt"));
    if fetched_at <= 0 {
        return None;
    }
    if fresh_only && now_millis() - fetched_at >= BANGUMI_SEASON_TTL_MILLIS {
        return None;
    }
    Some((
        entry["anime"].as_array().cloned().unwrap_or_default(),
        fetched_at,
    ))
}

/// 将季度卡片中的 AniList 播出编号重新绑定到 Bangumi 分季的本地集号。
///
/// 季度缓存最初由 AniList 批量补充生成，分季条目可能因此携带全局/主条目
/// 集号（例如夺还篇显示 ep16），而追番与任务使用的是该 Bangumi subject 的
/// 本地 ep5。这里仅使用本机已经成功取得的逐集缓存，不触网、不制造周播时间；
/// 分钟精度仍来自卡片里已有的 AniList airingSchedule，并通过日期配对处理
/// AniList 与 Bangumi 的不同编号空间。
#[cfg(all(feature = "standard", not(target_os = "android")))]
fn normalize_season_anime_episode_authority(
    cache_dir: &Path,
    mut anime: Vec<Value>,
    now: i64,
) -> Vec<Value> {
    for item in anime.iter_mut() {
        if value_string(item.get("source")) != "bangumi" {
            continue;
        }
        let subject_id = value_i64(item.get("bangumiSubjectId").or_else(|| item.get("id")));
        if subject_id <= 0 {
            continue;
        }
        let Some(records) =
            bangumi_episode_records_from_cache(cache_dir, subject_id, now, i64::MAX)
        else {
            continue;
        };
        let mut precise = HashMap::new();
        if let Some(nodes) = item
            .get("airingSchedule")
            .and_then(|schedule| schedule.get("nodes"))
            .and_then(Value::as_array)
        {
            for node in nodes {
                let episode = value_i64(node.get("episode"));
                let airing_at = value_i64(node.get("airingAt"));
                if episode > 0 && airing_at > 0 {
                    precise.insert(episode, airing_at);
                }
            }
        }
        if let Some(next) = item.get("nextAiringEpisode") {
            let episode = value_i64(next.get("episode"));
            let airing_at = value_i64(next.get("airingAt"));
            if episode > 0 && airing_at > 0 {
                precise.insert(episode, airing_at);
            }
        }
        let schedule = bangumi_episode_schedule_with_precision(&records, now, &precise);
        let Some((episode, _, airing_at, _)) =
            schedule.iter().find(|(_, _, _, aired)| !*aired).copied()
        else {
            if !schedule.is_empty() {
                item["nextAiringEpisode"] = Value::Null;
            }
            continue;
        };
        item["nextAiringEpisode"] = json!({
            "episode": episode,
            "airingAt": airing_at,
            "timeUntilAiring": (airing_at - now).max(0),
            "source": "bangumi_episode"
        });
    }
    anime
}

#[cfg(all(feature = "standard", target_os = "android"))]
fn normalize_season_anime_episode_authority(
    _cache_dir: &Path,
    anime: Vec<Value>,
    _now: i64,
) -> Vec<Value> {
    // Android applies the same mapping in AniListScheduler against its local
    // MobileStore episode cache; keep the shared season fetch free of desktop
    // filesystem-only helpers.
    anime
}

/// 播出选站优先级（schema §3.1）：读顶层 `bangumi.preferredBroadcastSites`，
/// 缺失/为空时用默认 `["bangumi","ani_one",...]`。
#[cfg(feature = "standard")]
fn preferred_broadcast_sites(state: &Value) -> Vec<String> {
    state
        .get("bangumi")
        .and_then(|block| block.get("preferredBroadcastSites"))
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .filter(|items| !items.is_empty())
        .unwrap_or_else(bangumi::default_preferred_broadcast_sites)
}

/// Bangumi platform → AniList format（Phase 2 契约）：TV→TV、
/// 劇場版/剧场版→MOVIE、OVA→OVA、WEB（大小写不敏感）→ONA，其他 None。
#[cfg(feature = "standard")]
fn bangumi_platform_to_format(platform: Option<&str>) -> Option<&'static str> {
    let platform = platform.unwrap_or_default().trim();
    match platform {
        "TV" => Some("TV"),
        "劇場版" | "剧场版" => Some("MOVIE"),
        "OVA" => Some("OVA"),
        other if other.eq_ignore_ascii_case("WEB") => Some("ONA"),
        _ => None,
    }
}

/// 从离线映射条目（v2 `bySubject`）提取站点级播出时间源（`s`/`begin`/`broadcast`）。
#[cfg(feature = "standard")]
fn offline_broadcast_sites(entry: &Value) -> Vec<bangumi::BroadcastSite<'_>> {
    entry
        .get("sites")
        .and_then(Value::as_array)
        .map(|sites| {
            sites
                .iter()
                .filter_map(|site| {
                    Some(bangumi::BroadcastSite {
                        site: site.get("s").and_then(Value::as_str)?,
                        begin: site.get("begin").and_then(Value::as_str),
                        broadcast: site.get("broadcast").and_then(Value::as_str),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// 播出四级优先第一级（纯函数内）：由离线映射 begin/broadcast/sites 计算条目的
/// nextAiringEpisode。episode 号 = 规则起点起已播期数+1（floor((now-begin)/period)+1，
/// 夹在 1..=eps）；无任何 broadcast/begin 数据 → null（不伪造，第四级语义）。
#[cfg(feature = "standard")]
fn bangumi_next_airing_episode(
    subject: &bangumi::BangumiSubject,
    offline: Option<&Value>,
    preferred_sites: &[String],
    checked_at: i64,
) -> Value {
    let Some(offline) = offline else {
        return Value::Null;
    };
    let begin = offline.get("begin").and_then(Value::as_str);
    let broadcast = offline.get("broadcast").and_then(Value::as_str);
    let sites = offline_broadcast_sites(offline);
    if begin.is_none() && broadcast.is_none() && sites.is_empty() {
        return Value::Null;
    }
    let Some(now) =
        chrono::DateTime::from_timestamp(checked_at, 0).map(|dt| dt.with_timezone(&chrono::Utc))
    else {
        return Value::Null;
    };
    let Some(next) = bangumi::next_broadcast_after(begin, broadcast, &sites, preferred_sites, now)
    else {
        return Value::Null;
    };
    let (selected_begin, selected_rule) =
        bangumi::select_broadcast_source(begin, broadcast, &sites, preferred_sites);
    let eps = i64::from(subject.eps.unwrap_or(0));
    let episode = match selected_rule.and_then(bangumi::parse_recurrence_rule) {
        // 周期规则：floor((now-start)/period)+1（start 在未来时夹到 1）。
        Some((start, period)) if period.num_seconds() > 0 => {
            let elapsed = now.signed_duration_since(start).num_seconds();
            (elapsed / period.num_seconds() + 1).max(1)
        }
        // 一次性 begin（电影/OVA）：只有 1 期。
        _ => {
            debug_assert!(selected_begin.is_some());
            1
        }
    };
    // 问题 D 完结钳制：推算"下一期"号（floor+1）超过总集数 → 全部播完，
    // nextAiringEpisode=null（旧行为夹到 eps 会给已完结番伪造一集"下周播出"）。
    if eps > 0 && episode > eps {
        return Value::Null;
    }
    json!({
        "episode": episode,
        "airingAt": next.timestamp(),
        "timeUntilAiring": (next - now).num_seconds().max(0)
    })
}

/// Phase 2 任务 1 契约：Bangumi 季度条目 → AniList 形状 Anime（纯函数）。
/// id=subjectId；source="bangumi"；bangumiSubjectId=subjectId；anilistId 由离线
/// 映射反查；标题中文优先（name_cn 非空 → native=name_cn、romaji=name 原文）；
/// nextAiringEpisode 走播出四级优先第一级（离线 begin/broadcast）。
#[cfg(feature = "standard")]
fn map_subjects_to_anime(
    subjects: &[bangumi::BangumiSubject],
    offline_map: &Value,
    preferred_sites: &[String],
    season: &str,
    year: i64,
    checked_at: i64,
) -> Vec<Value> {
    subjects
        .iter()
        .map(|subject| {
            let subject_id = subject.id;
            let offline = offline_bangumi_subject(offline_map, subject_id);
            let anilist_id = offline
                .as_ref()
                .and_then(|entry| entry.get("a"))
                .and_then(Value::as_i64)
                .filter(|anilist_id| *anilist_id > 0);
            let name_cn = subject
                .name_cn
                .as_deref()
                .map(str::trim)
                .filter(|name| !name.is_empty());
            let native = name_cn.unwrap_or(subject.name.as_str());
            // 中文优先：name_cn 非空 → native=name_cn、romaji=name 原文；
            // name_cn 缺失 → native=name、romaji=null。
            let romaji = if name_cn.is_some() {
                json!(subject.name)
            } else {
                Value::Null
            };
            let images = subject.images.as_ref();
            let pick_image =
                |pick: fn(&bangumi::BangumiSubjectImages) -> &Option<String>| -> String {
                    images
                        .and_then(|images| pick(images).clone())
                        .filter(|url| !url.is_empty())
                        .unwrap_or_default()
                };
            // extraLarge = images.large || images.common || ""；medium = images.medium || images.common || ""。
            let extra_large = {
                let large = pick_image(|i| &i.large);
                if large.is_empty() {
                    pick_image(|i| &i.common)
                } else {
                    large
                }
            };
            let medium = {
                let medium = pick_image(|i| &i.medium);
                if medium.is_empty() {
                    pick_image(|i| &i.common)
                } else {
                    medium
                }
            };
            let start_date = subject.date.as_deref().and_then(|date| {
                let mut parts = date.split('-');
                let year: i64 = parts.next()?.parse().ok()?;
                let month: i64 = parts.next()?.parse().ok()?;
                let day: i64 = parts.next()?.parse().ok()?;
                Some(json!({"year": year, "month": month, "day": day}))
            });
            json!({
                "id": subject_id,
                "source": "bangumi",
                "bangumiSubjectId": subject_id,
                "anilistId": anilist_id.map(|id| json!(id)).unwrap_or(Value::Null),
                "nameCn": subject.name_cn,
                "title": {
                    "native": native,
                    "english": Value::Null,
                    "romaji": romaji
                },
                "coverImage": {
                    "extraLarge": extra_large,
                    "medium": medium,
                    "color": Value::Null
                },
                "description": subject.summary.clone().unwrap_or_default(),
                "episodes": subject.eps,
                "duration": Value::Null,
                "status": Value::Null,
                "season": season,
                "seasonYear": year,
                "startDate": start_date.unwrap_or(Value::Null),
                "averageScore": subject.rating.as_ref().and_then(|rating| rating.score),
                "genres": [],
                "format": bangumi_platform_to_format(subject.platform.as_deref()),
                "siteUrl": format!("https://bgm.tv/subject/{subject_id}"),
                "nextAiringEpisode": bangumi_next_airing_episode(
                    subject,
                    offline.as_ref(),
                    preferred_sites,
                    checked_at
                )
            })
        })
        .collect()
}

#[tauri::command]
fn get_state(_app: AppHandle, context: State<'_, AppContext>) -> Result<Value, String> {
    #[cfg(target_os = "android")]
    mobile::consume_events(&_app, &context).map_err(|error| error.to_string())?;
    // Phase 4 任务 2：Android 前台过期检查（进程内标志 + single-flight 防重复，
    // spawn 后台补偿、不阻塞返回；original edition 不编译此行，桌面零变化）。
    #[cfg(all(feature = "standard", target_os = "android"))]
    maybe_spawn_foreground_sync(&_app, &context);
    Ok(context.public_state())
}

fn refresh_mobile_configuration(app: &AppHandle, context: &AppContext) -> Result<(), String> {
    #[cfg(target_os = "android")]
    mobile::configure(app, context).map_err(|error| error.to_string())?;
    #[cfg(not(target_os = "android"))]
    let _ = (app, context);
    Ok(())
}

/// standard 版 Bangumi 来源追番条目形状（Phase 2 主键迁移）：id=subjectId、
/// source="bangumi"、anilistId=AniList 关联、titleSource="bangumi"、
/// siteUrl=bgm.tv 条目页、displayTitle 优先 name_cn、mapping 落 manual/high。
#[cfg(feature = "standard")]
fn bangumi_following_entry(anime: &Value, preference: &str, language: &str) -> Value {
    let id = value_i64(anime.get("id"));
    let subject_id = if value_i64(anime.get("bangumiSubjectId")) > 0 {
        value_i64(anime.get("bangumiSubjectId"))
    } else {
        id
    };
    let title = anime.get("title").cloned().unwrap_or_default();
    let name_cn = value_string(anime.get("nameCn"));
    let display_title = if !name_cn.is_empty() {
        name_cn
    } else {
        title_for(&title, preference, language)
    };
    json!({
        "id": subject_id, "source": "bangumi",
        "anilistId": anime.get("anilistId").cloned().unwrap_or(Value::Null),
        "bangumiId": subject_id,
        "title": title, "displayTitle": display_title, "titleSource": "bangumi",
        "coverImage": anime["coverImage"]["medium"].as_str().or(anime["coverImage"]["extraLarge"].as_str()).unwrap_or_default(),
        "format": anime.get("format"), "episodes": anime.get("episodes"), "seasonYear": anime.get("seasonYear"),
        "startDate": anime.get("startDate"),
        // bangumi-data 的 begin/broadcast 只是一条首播/周期锚点，不能作为
        // 已追番作品的下一集事实。真正的 next 由 /v0/episodes + 已验证
        // AniList 分钟缓存异步填入；先留空可避免新追番瞬间就排出错误闹钟。
        "nextAiringEpisode": Value::Null,
        "siteUrl": format!("https://bgm.tv/subject/{subject_id}"),
        // Phase 3 任务 1：收藏/评分/进度镜像字段（拉取引擎写入实际值）。
        "bangumiStatus": Value::Null, "rating": Value::Null, "watchedEpisode": Value::Null,
        "mapping": {"method": "manual", "confidence": "high", "updatedAt": now_seconds()},
        "mappingPending": false,
        "followedAt": now_seconds(), "syncUpdatedAt": now_millis()
    })
}

/// AniList 形状追番条目构造（原 toggle_follow else 分支抽取，两 edition 共用）。
fn anilist_following_entry(state: &Value, anime: &Value, original: bool) -> Value {
    let title = anime.get("title").cloned().unwrap_or_default();
    let (title_value, title_source, bangumi_id) = followed_title_fields(state, anime, original);
    #[cfg_attr(not(feature = "standard"), expect(unused_mut))]
    let mut entry = json!({
        "id": value_i64(anime.get("id")), "title": title, "displayTitle": title_value, "titleSource": title_source, "bangumiId": bangumi_id,
        "coverImage": anime["coverImage"]["medium"].as_str().or(anime["coverImage"]["extraLarge"].as_str()).unwrap_or_default(),
        "format": anime.get("format"), "episodes": anime.get("episodes"), "seasonYear": anime.get("seasonYear"),
        "startDate": anime.get("startDate"), "nextAiringEpisode": anime.get("nextAiringEpisode"), "siteUrl": anime.get("siteUrl"),
        "followedAt": now_seconds(), "syncUpdatedAt": now_millis()
    });
    // standard 版 AniList 条目补 additive 来源字段（original 不写）。
    #[cfg(feature = "standard")]
    if !original {
        entry["source"] = json!("anilist");
        entry["anilistId"] = Value::Null;
        entry["mapping"] = Value::Null;
        entry["mappingPending"] = json!(false);
    }
    entry
}

/// 用户主动追番 → 条目绑定 manual/high 映射（问题 C 转正/合并路径共用）。
#[cfg(feature = "standard")]
fn bind_manual_mapping(state: &mut Value, subject_id: i64) {
    if let Some(entry) = state["following"].as_array_mut().and_then(|items| {
        items
            .iter_mut()
            .find(|item| value_i64(item.get("id")) == subject_id)
    }) {
        entry["mapping"] =
            json!({"method": "manual", "confidence": "high", "updatedAt": now_seconds()});
        entry["mappingPending"] = json!(false);
    }
}

/// 用 Bangumi 卡片数据补齐已重键条目的展示字段（问题 C 转正路径；只补缺，
/// 用户自定义标题 titleSource=="custom" 不覆盖）。
#[cfg(feature = "standard")]
fn enrich_following_entry_from_anime(state: &mut Value, subject_id: i64, anime: &Value) {
    if let Some(entry) = state["following"].as_array_mut().and_then(|items| {
        items
            .iter_mut()
            .find(|item| value_i64(item.get("id")) == subject_id)
    }) {
        let name_cn = value_string(anime.get("nameCn"));
        if !name_cn.is_empty() && value_string(entry.get("titleSource")) != "custom" {
            entry["displayTitle"] = json!(name_cn);
            entry["titleSource"] = json!("bangumi");
        }
        let cover = anime["coverImage"]["medium"]
            .as_str()
            .or(anime["coverImage"]["extraLarge"].as_str())
            .unwrap_or_default();
        if !cover.is_empty() {
            // 封面是服务端数据（非用户编辑），Bangumi 卡片值优先。
            entry["coverImage"] = json!(cover);
        }
        for key in [
            "episodes",
            "format",
            "seasonYear",
            "startDate",
            "nextAiringEpisode",
        ] {
            if entry.get(key).map(Value::is_null).unwrap_or(true) {
                if let Some(value) = anime.get(key).filter(|value| !value.is_null()) {
                    entry[key] = value.clone();
                }
            }
        }
        if value_string(entry.get("siteUrl")).is_empty() {
            entry["siteUrl"] = json!(format!("https://bgm.tv/subject/{subject_id}"));
        }
        if value_i64(entry.get("bangumiId")) != subject_id {
            entry["bangumiId"] = json!(subject_id);
        }
    }
}

/// 问题 2a（验收第 2 轮，P0 追番/评分不自动写回）：追番动作是本地变更 → 置
/// lastChangedBy="local"。此前该字段只由拉取引擎写 "bangumi"，本地追番的
/// 条目从不带它，push_local_changes 只能靠 hash 幂等兜底且无拉取基线的
/// 新增条目缺方向标记。写回引擎读它（Phase 3 契约：local/webdav 可推送）。
#[cfg(feature = "standard")]
fn mark_following_local_change(state: &mut Value, subject_id: i64) {
    if let Some(entry) = state["following"].as_array_mut().and_then(|items| {
        items
            .iter_mut()
            .find(|item| value_i64(item.get("id")) == subject_id)
    }) {
        entry["lastChangedBy"] = json!("local");
    }
}

/// Mark an explicit local follow/re-follow intent.  This is deliberately
/// separate from ordinary edits such as rating, title, or Bangumi status:
/// only a follow action may override an older remote `dropped` tombstone.
#[cfg(feature = "standard")]
fn mark_following_refollow_intent(state: &mut Value, subject_id: i64) {
    mark_following_local_change(state, subject_id);
    if let Some(entry) = state["following"].as_array_mut().and_then(|items| {
        items
            .iter_mut()
            .find(|item| value_i64(item.get("id")) == subject_id)
    }) {
        entry["localFollowIntentAt"] = json!(now_millis());
    }
}

/// 跨键重追守卫的身份解析（standard）：返回该作品已知的
/// (subjectId=S, anilistId=A) 身份对（缺失侧为 0）。Bangumi 卡片以卡片值优先，
/// anilistId 缺失时经离线映射兜底（bySubject[S].a 直查或 anilistIndex 反查）；
/// AniList 卡片先查 following 内的显式 anilistId 绑定，再离线映射反查
/// （anilistIndex / bySubject 扫描，多义不认）。仅用于「取消后重追」的复活与
/// 取消队列撤销判定，不做任何状态修改。
#[cfg(feature = "standard")]
fn resolve_work_identities(state: &Value, anime: &Value, map: &Value) -> (i64, i64) {
    let id = value_i64(anime.get("id"));
    if value_string(anime.get("source")) == "bangumi" && id > 0 {
        let subject_id = if value_i64(anime.get("bangumiSubjectId")) > 0 {
            value_i64(anime.get("bangumiSubjectId"))
        } else {
            id
        };
        let mut anilist_id = value_i64(anime.get("anilistId"));
        if anilist_id <= 0 {
            anilist_id = map
                .get("bySubject")
                .and_then(|by_subject| by_subject.get(subject_id.to_string()))
                .map(|entry| value_i64(entry.get("a")))
                .unwrap_or(0);
        }
        if anilist_id <= 0 {
            anilist_id = anilist_index_reverse(map, subject_id);
        }
        if anilist_id == subject_id {
            anilist_id = 0;
        }
        (subject_id, anilist_id)
    } else if id > 0 {
        // 修复 2：显式 anilistId 绑定仅唯一认领才认——anilistId 撞车（多个
        // bangumi 条目绑定同一 anilistId，分季课程共占一个 AniList 条目）时
        // 不把任何一个撞车条目当合并/复活目标，交给离线映射锚定
        // （anilistIndex 代表项 / bySubject 唯一命中）。
        let mut bound_candidates: Vec<i64> = state["following"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|item| {
                value_string(item.get("source")) == "bangumi"
                    && value_i64(item.get("anilistId")) == id
            })
            .map(|item| value_i64(item.get("id")))
            .collect();
        bound_candidates.sort_unstable();
        bound_candidates.dedup();
        let bound = if bound_candidates.len() == 1 {
            Some(bound_candidates[0])
        } else {
            None
        };
        let subject_id = bound
            .filter(|subject_id| *subject_id > 0 && *subject_id != id)
            .or_else(|| offline_mapped_subject_id(map, id).filter(|subject_id| *subject_id != id))
            .unwrap_or(0);
        (subject_id, id)
    } else {
        (0, 0)
    }
}

/// 问题 C（P0 追番判重）：standard 版追番新增路径。跨键守卫保证同一部番绝不
/// 出现两条追番记录：
/// - follow Bangumi 卡片（subjectId=S, anilistId=A）而存在 id==A 的 anilist 键
///   条目 → apply_mapping 转正（旧 id 墓碑 + pending 任务重键）而非新增；
/// - follow Bangumi 卡片而 id==S 条目已存在 → 只补字段不重复新增；
/// - follow AniList 卡片（id=A）而存在 anilistId==A 的 bangumi 条目 → 该条目
///   绑定 manual/high 后直接复用，不新增第二条；
/// - 跨键重追复活（数据安全）：目标作品的旧身份存在墓碑且 following 已无该
///   条目（用户取消后从另一侧卡片重追）→ 不新增另一条键，而是复活原键条目
///   （bangumi_following_entry 构造 + manual/high + lastChangedBy=local）并清
///   墓碑；否则 S 墓碑与 pendingBangumiUnfollows 残留会让写回引擎把刚重追的
///   番在 Bangumi 侧 PATCH 成 type=5「抛弃」，且墓碑阻止复活语义。
/// 无论走哪条路径，最后都从 pendingBangumiUnfollows 移除该作品的全部身份
/// 队列项——重新追番即撤销未推送的取消意图。
#[cfg(feature = "standard")]
fn add_following_entry_standard(state: &mut Value, anime: &Value, map: &Value) {
    let id = value_i64(anime.get("id"));
    let preference = value_string(state["settings"].get("titlePreference"));
    let language = value_string(state["settings"].get("uiLanguage"));
    let (identity_subject, identity_anilist) = resolve_work_identities(state, anime, map);
    if value_string(anime.get("source")) == "bangumi" && id > 0 {
        let mut entry = bangumi_following_entry(anime, &preference, &language);
        let subject_id = value_i64(entry.get("id"));
        let anilist_id = value_i64(anime.get("anilistId"));
        let cross_anilist_exists = anilist_id > 0
            && anilist_id != subject_id
            && state["following"].as_array().is_some_and(|items| {
                items.iter().any(|item| {
                    value_i64(item.get("id")) == anilist_id
                            // 修复 2（跨键身份按 subject 锚定）：候选旧键条目必须
                            // 与本次卡片的作品身份同源——离线映射已把该 anilistId
                            // 唯一锚定到另一 subject（anilistId 撞车，如丧失篇
                            // 547888 与夺还篇 633836 共用 189046）时不算同一
                            // 作品，不合并，走独立新增/复活路径。
                            && offline_mapped_subject_id(map, anilist_id)
                                .is_none_or(|mapped| mapped == subject_id)
                })
            });
        let same_subject_exists = state["following"].as_array().is_some_and(|items| {
            items
                .iter()
                .any(|item| value_i64(item.get("id")) == subject_id)
        });
        if cross_anilist_exists {
            let bangumi_target_exists = state["following"].as_array().is_some_and(|items| {
                items.iter().any(|item| {
                    value_i64(item.get("id")) == subject_id
                        && value_string(item.get("source")) == "bangumi"
                })
            });
            if bangumi_target_exists {
                // 两侧都存在（跨键重复）：走合并路径，绝不产生两条 id=subjectId。
                merge_cross_key_entry(state, anilist_id, subject_id);
            } else {
                // 仅旧 AniList 键记录：转正为 bangumi 主键（墓碑 + 任务重键）。
                apply_mapping(state, anilist_id, subject_id, true);
            }
            enrich_following_entry_from_anime(state, subject_id, anime);
            bind_manual_mapping(state, subject_id);
            mark_following_changed(state, subject_id);
            mark_following_refollow_intent(state, subject_id);
        } else if same_subject_exists {
            enrich_following_entry_from_anime(state, subject_id, anime);
            bind_manual_mapping(state, subject_id);
            mark_following_changed(state, subject_id);
            mark_following_refollow_intent(state, subject_id);
        } else {
            // 问题 2a：本地追番 → lastChangedBy=local（写回引擎据此 POST）。
            entry["lastChangedBy"] = json!("local");
            entry["localFollowIntentAt"] = json!(now_millis());
            state["following"].as_array_mut().unwrap().push(entry);
            mark_following_changed(state, subject_id);
            mark_following_refollow_intent(state, subject_id);
            // 对称复活：旧 AniList 键条目此前被用户取消（A 墓碑残留）→ 重追
            // 即撤销删除意图，一并清 A 墓碑，防止残留墓碑阻止复活语义。
            if identity_anilist > 0 && following_tombstone_exists(state, identity_anilist) {
                mark_following_changed(state, identity_anilist);
            }
        }
    } else {
        // 跨键重追复活守卫：A 卡片对应作品存在 S 墓碑且 following 已无 S 条目
        // （用户取消 bangumi 条目后从 AniList 卡片重追）→ 不新增 A 键条目，
        // 复活 S 键条目并清墓碑。
        let revival = identity_subject > 0
            && !state["following"].as_array().is_some_and(|items| {
                items
                    .iter()
                    .any(|item| value_i64(item.get("id")) == identity_subject)
            })
            && following_tombstone_exists(state, identity_subject);
        if revival {
            let mut card = anime.clone();
            card["id"] = json!(identity_subject);
            card["bangumiSubjectId"] = json!(identity_subject);
            card["source"] = json!("bangumi");
            card["anilistId"] = json!(identity_anilist);
            // 离线映射有 Bangumi 元数据 → 补 name_cn（displayTitle 优先中文名）。
            if let Some(subject) = offline_bangumi_subject(map, identity_subject) {
                let name_cn = value_string(subject.get("c"));
                if !name_cn.is_empty() {
                    card["nameCn"] = json!(name_cn);
                }
            }
            let mut entry = bangumi_following_entry(&card, &preference, &language);
            entry["lastChangedBy"] = json!("local");
            entry["localFollowIntentAt"] = json!(now_millis());
            state["following"].as_array_mut().unwrap().push(entry);
            // mark_following_changed 语义：清 S 墓碑（复活）。
            mark_following_changed(state, identity_subject);
            mark_following_refollow_intent(state, identity_subject);
            // A 键若也残留墓碑（该作品曾以 A 键被取消）一并清除。
            if identity_anilist > 0 && following_tombstone_exists(state, identity_anilist) {
                mark_following_changed(state, identity_anilist);
            }
        } else {
            // AniList 卡片：存在同作品的 bangumi 键条目（anilistId==id）→ 转正
            // 绑定 manual/high，不新增。
            let existing_bangumi_id = state["following"].as_array().and_then(|items| {
                items
                    .iter()
                    .find(|item| {
                        value_string(item.get("source")) == "bangumi"
                            && value_i64(item.get("anilistId")) == id
                    })
                    .map(|item| value_i64(item.get("id")))
            });
            if let Some(entry_id) = existing_bangumi_id.filter(|entry_id| *entry_id > 0) {
                if let Some(entry) = state["following"].as_array_mut().and_then(|items| {
                    items
                        .iter_mut()
                        .find(|item| value_i64(item.get("id")) == entry_id)
                }) {
                    entry["mapping"] = json!({"method": "manual", "confidence": "high", "updatedAt": now_seconds()});
                    entry["mappingPending"] = json!(false);
                    entry["syncUpdatedAt"] = json!(now_millis());
                    entry["lastChangedBy"] = json!("local");
                    entry["localFollowIntentAt"] = json!(now_millis());
                }
                mark_following_changed(state, entry_id);
                mark_following_refollow_intent(state, entry_id);
            } else {
                let entry = anilist_following_entry(state, anime, false);
                state["following"].as_array_mut().unwrap().push(entry);
                mark_following_changed(state, id);
            }
        }
    }
    // 无论走哪条路径：用户重新追番即撤销未推送的取消意图——从
    // pendingBangumiUnfollows 移除该作品的全部身份队列项（S 与 A），绝不留
    // 残留队列让写回引擎把刚重追的番 PATCH 成 type=5。
    remove_pending_bangumi_unfollow(state, identity_subject);
    if identity_anilist > 0 {
        remove_pending_bangumi_unfollow(state, identity_anilist);
    }
}

/// 追番新增入口（toggle_follow 的加路径）：standard 走跨键守卫路径（含跨键
/// 重追复活），original 保持原行为完全不变。
fn add_following_entry(state: &mut Value, anime: &Value, original: bool, offline_map: &Value) {
    #[cfg(feature = "standard")]
    if !original {
        add_following_entry_standard(state, anime, offline_map);
        return;
    }
    #[cfg(not(feature = "standard"))]
    let _ = offline_map;
    let id = value_i64(anime.get("id"));
    let entry = anilist_following_entry(state, anime, original);
    state["following"].as_array_mut().unwrap().push(entry);
    mark_following_changed(state, id);
}

#[tauri::command]
fn toggle_follow(
    app: AppHandle,
    context: State<'_, AppContext>,
    anime: Value,
) -> Result<Value, String> {
    let id = value_i64(anime.get("id"));
    let mut state = context.state.lock().map_err(|_| "状态锁不可用")?;
    if !remove_following(&mut state, id) {
        add_following_entry(
            &mut state,
            &anime,
            context.original,
            &context.offline_bangumi,
        );
    }
    drop(state);
    context.save_state().map_err(|error| error.to_string())?;
    context.webdav_wakeup.notify_one();
    // 问题 2b：追番/取消追番均为本地变更 → 唤醒桌面自动同步（写回收藏或
    // type=5）。函数内部按编译目标判空。
    notify_bangumi_sync_wakeup(true);
    refresh_mobile_configuration(&app, &context)?;
    emit_state(&app, &context);
    Ok(context.public_state())
}

#[tauri::command]
fn update_follow_title(
    app: AppHandle,
    context: State<'_, AppContext>,
    anime_id: i64,
    display_title: String,
) -> Result<Value, String> {
    let title = display_title.trim();
    if title.is_empty() {
        return Ok(context.public_state());
    }
    let mut state = context.state.lock().map_err(|_| "状态锁不可用")?;
    if let Some(followed) = state["following"].as_array_mut().and_then(|items| {
        items
            .iter_mut()
            .find(|item| value_i64(item.get("id")) == anime_id)
    }) {
        followed["displayTitle"] = json!(title);
        followed["titleSource"] = json!("custom");
        if let Some(tasks) = state["tasks"].as_array_mut() {
            for task in tasks
                .iter_mut()
                .filter(|task| value_i64(task.get("animeId")) == anime_id)
            {
                task["animeTitle"] = json!(title);
            }
        }
        mark_following_changed(&mut state, anime_id);
    }
    drop(state);
    context.save_state().map_err(|error| error.to_string())?;
    context.webdav_wakeup.notify_one();
    refresh_mobile_configuration(&app, &context)?;
    emit_state(&app, &context);
    Ok(context.public_state())
}

/// toggle_task 的纯内核：翻转完成状态并维护 completedAt/syncUpdatedAt；
/// 返回是否从 pending 翻转为 completed。
/// 问题 2a（验收第 2 轮）：subjectId 齐备的 bangumi 任务完成时置
/// lastChangedBy="local"——写回引擎据它区分本地完成（可上传）与拉取完成
/// （lastChangedBy=bangumi 不上传）；anilist 键任务与 original 不写该字段
/// （行为不变）。
fn toggle_task_status(task: &mut Value) -> bool {
    let completed = value_string(task.get("status")) == "completed";
    task["status"] = json!(if completed { "pending" } else { "completed" });
    task["completedAt"] = if completed {
        Value::Null
    } else {
        json!(now_seconds())
    };
    task["syncUpdatedAt"] = json!(now_millis());
    let newly_completed = !completed;
    #[cfg(feature = "standard")]
    if newly_completed && value_i64(task.get("subjectId")) > 0 {
        task["lastChangedBy"] = json!("local");
    }
    newly_completed
}

/// 状态驱动追踪（任务 3）完结自动转「看过」内核：任务 newly_completed 后检查
/// 条目是否已到最后一话（episodes 已知且 task.episode >= episodes），且条目
/// 当前处于追踪中（bangumiStatus 为 doing 或空/null——wish/on_hold/done 不触发，
/// 已 done 天然只触发一次）。满足则置 `bangumiStatus="done"` +
/// `lastChangedBy="local"`（H_local 随之变化，写回引擎 PATCH type=2）。
/// 返回 `Some((subjectId, displayTitle))` 表示发生了完结转换（命令层据此发
/// `finale-completed` 事件）。
///
/// 条目定位：subjectId>0 直接按条目 id/bangumiId 匹配；anilist 键任务（无
/// subjectId）经条目 anilistId/id 反查。
#[cfg(feature = "standard")]
fn mark_entry_done_on_finale(state: &mut Value, task: &Value) -> Option<(i64, String)> {
    let subject_id = value_i64(task.get("subjectId"));
    let anime_id = value_i64(task.get("animeId"));
    let index = state["following"].as_array().and_then(|items| {
        items.iter().position(|item| {
            if subject_id > 0 {
                value_i64(item.get("id")) == subject_id
                    || value_i64(item.get("bangumiId")) == subject_id
            } else {
                anime_id > 0
                    && (value_i64(item.get("id")) == anime_id
                        || value_i64(item.get("anilistId")) == anime_id)
            }
        })
    })?;
    let entry = &state["following"][index];
    // 仅追踪中（doing / 空状态）触发；非空且非 doing（wish/on_hold/done）跳过。
    let current_status = value_string(entry.get("bangumiStatus"));
    if bangumi_status_blocks_tracking(&current_status) {
        return None;
    }
    let episodes = value_i64(entry.get("episodes"));
    let episode = value_i64(task.get("episode"));
    if episodes <= 0 || episode <= 0 || episode < episodes {
        return None;
    }
    let entry_subject_id = if subject_id > 0 {
        subject_id
    } else {
        value_i64(entry.get("id"))
    };
    let display_title = value_string(entry.get("displayTitle"));
    let entry = &mut state["following"][index];
    entry["bangumiStatus"] = json!("done");
    entry["lastChangedBy"] = json!("local");
    entry["syncUpdatedAt"] = json!(now_millis());
    Some((entry_subject_id, display_title))
}

/// standard 动作唤醒入口（问题 2b，跨平台）：toggle_follow / toggle_task /
/// bangumi_set_rating 的本地变更触发 `BANGUMI_SYNC_WAKEUP`，30 秒静默期合并
/// 后由 start_bangumi_sync_loop 执行全量同步。仅按 edition 门控（standard /
/// original），平台不限：Android 上循环只在进程存活期间运行、随进程死亡，
/// 60 分钟周期 + 动作唤醒，不违反"后台不常驻/不高频轮询"约束。original
/// 为空操作：零 Bangumi。
#[cfg(feature = "standard")]
fn notify_bangumi_sync_wakeup(wake: bool) {
    if wake {
        BANGUMI_SYNC_WAKEUP.notify_one();
    }
}

/// original 回退：零 Bangumi，唤醒即空操作。
#[cfg(not(feature = "standard"))]
fn notify_bangumi_sync_wakeup(_wake: bool) {}

#[tauri::command]
fn toggle_task(
    app: AppHandle,
    context: State<'_, AppContext>,
    task_id: String,
) -> Result<Value, String> {
    let mut state = context.state.lock().map_err(|_| "状态锁不可用")?;
    // 状态驱动追踪（任务 3）：完结集完成 → 条目自动转 done + finale-completed
    // 事件（app.emit，standard only；original 无 Bangumi 概念，行为不变）。
    #[cfg(feature = "standard")]
    let mut finale_completed: Option<(i64, String)> = None;
    #[cfg(not(feature = "standard"))]
    let finale_completed: Option<(i64, String)> = None;
    let bangumi_task_completed;
    if let Some(task) = state["tasks"].as_array_mut().and_then(|items| {
        items
            .iter_mut()
            .find(|task| value_string(task.get("id")) == task_id)
    }) {
        let newly_completed = toggle_task_status(task);
        bangumi_task_completed = newly_completed && value_i64(task.get("subjectId")) > 0;
        // 快照后任务借用即终结，才能再借 &mut state 做条目级完结转换。
        #[cfg(feature = "standard")]
        if newly_completed {
            let snapshot = task.clone();
            finale_completed = mark_entry_done_on_finale(&mut state, &snapshot);
        }
    } else {
        bangumi_task_completed = false;
    };
    drop(state);
    if let Some((subject_id, display_title)) = finale_completed {
        let _ = app.emit(
            "finale-completed",
            json!({"subjectId": subject_id, "displayTitle": display_title}),
        );
    }
    context.save_state().map_err(|error| error.to_string())?;
    context.webdav_wakeup.notify_one();
    // 问题 2b：bangumi 任务完成 → 动作唤醒桌面自动同步（写回单集进度）。
    notify_bangumi_sync_wakeup(bangumi_task_completed);
    refresh_mobile_configuration(&app, &context)?;
    emit_state(&app, &context);
    Ok(context.public_state())
}

#[tauri::command]
fn update_settings(
    app: AppHandle,
    context: State<'_, AppContext>,
    settings: Value,
) -> Result<Value, String> {
    let interval_changed = settings
        .as_object()
        .is_some_and(|patch| patch.contains_key("pollIntervalMinutes"));
    #[cfg(desktop)]
    let tray_language_changed = settings
        .as_object()
        .is_some_and(|patch| patch.contains_key("uiLanguage"));
    #[cfg(desktop)]
    let tray_visibility_changed = settings
        .as_object()
        .is_some_and(|patch| patch.contains_key("showTrayIcon"));
    let mut state = context.state.lock().map_err(|_| "状态锁不可用")?;
    if let Some(patch) = settings.as_object() {
        if !context.original && patch.contains_key("bangumiApiBaseUrl") {
            context
                .bangumi_unavailable_until
                .store(0, Ordering::Relaxed);
        }
        let target = state["settings"].as_object_mut().unwrap();
        for (key, value) in patch {
            if key == "showTrayIcon" && !value.is_boolean() {
                continue;
            }
            if key != "bangumiApiBaseUrl" || !context.original {
                target.insert(key.clone(), value.clone());
            }
        }
        if let Some(url) = target.get("bangumiApiBaseUrl").and_then(Value::as_str) {
            target.insert(
                "bangumiApiBaseUrl".into(),
                json!(normalize_url(url, Some("/v0")).unwrap_or_default()),
            );
        }
        if context.original {
            target.insert("bangumiApiBaseUrl".into(), json!(""));
        }
        if let Some(time) = target.get("dailyTaskReminderTime").and_then(Value::as_str) {
            if !is_valid_reminder_time(time) {
                target.insert("dailyTaskReminderTime".into(), json!("20:00"));
            }
        }
        // 决策 11：standard 版用户编辑 settings.bangumiApiBaseUrl 时，同步写入
        // 顶层 bangumi.apiBaseUrl，保持两处一致（original 不写 bangumi 块，
        // 行为不变：仍然拒绝 bangumiApiBaseUrl 写入并强制为空）。
        #[cfg(feature = "standard")]
        if !context.original && patch.contains_key("bangumiApiBaseUrl") {
            sync_bangumi_api_base_url_into_block(&mut state);
        }
        if context.original && patch.contains_key("titlePreference") {
            refresh_original_followed_titles(&mut state);
        }
    }
    #[cfg(desktop)]
    let launch_at_login = value_bool(state["settings"].get("launchAtLogin"));
    #[cfg(desktop)]
    let tray_visible = show_tray_icon(&state["settings"]);
    drop(state);
    #[cfg(desktop)]
    {
        reconcile_autostart(&app, launch_at_login).map_err(|error| error.to_string())?;
    }
    context.save_state().map_err(|error| error.to_string())?;
    if settings
        .as_object()
        .is_some_and(|patch| patch.contains_key("titlePreference"))
    {
        context.webdav_wakeup.notify_one();
    }
    if interval_changed {
        context.sync_wakeup.notify_one();
    }
    #[cfg(desktop)]
    if tray_language_changed {
        setup_tray(&app, &context).map_err(|error| error.to_string())?;
    } else if tray_visibility_changed {
        if let Some(tray) = app.tray_by_id("main") {
            tray.set_visible(tray_visible)
                .map_err(|error| error.to_string())?;
        } else {
            setup_tray(&app, &context).map_err(|error| error.to_string())?;
        }
    }
    refresh_mobile_configuration(&app, &context)?;
    emit_state(&app, &context);
    Ok(context.public_state())
}

fn is_valid_reminder_time(time: &str) -> bool {
    DAILY_TASK_REMINDER_TIME_RE.is_match(time)
}

#[tauri::command]
async fn sync_now(app: AppHandle, context: State<'_, AppContext>) -> Result<Value, String> {
    #[cfg(target_os = "android")]
    {
        let status = mobile::sync_native(&app, &context).map_err(|error| error.to_string())?;
        let created = value_i64(status.get("created"));
        return Ok(json!({"created": created, "syncedAt": value_i64(status.get("syncedAt"))}));
    }
    #[cfg(not(target_os = "android"))]
    sync_now_inner(&app, &context).await
}

#[cfg(not(target_os = "android"))]
#[derive(Debug, Default, PartialEq, Eq)]
struct AiringOutcome {
    aired: usize,
    created: usize,
}

/// 问题 3 内核（纯函数）：从任务列表收集"已完成集合"——status=completed
/// 任务的 (animeId, episode) 与 (subjectId, episode) 两种键身份 + 同集。
/// 旧版（AniList 主键时代）完成任务挂 anilistId 键，新版 bangumi 条目按
/// subjectId 生成任务，按任务 id 查重查不到，需按此集合按集拦截。供
/// apply_airing_schedules 与 Android mobile::merge_status 共用同一判定口径。
#[cfg(feature = "standard")]
fn completed_episode_history(tasks: &Value) -> HashSet<(i64, i64)> {
    let mut history: HashSet<(i64, i64)> = HashSet::new();
    for task in tasks.as_array().into_iter().flatten() {
        if value_string(task.get("status")) != "completed" {
            continue;
        }
        let episode = value_i64(task.get("episode"));
        if episode <= 0 {
            continue;
        }
        let anime_id = value_i64(task.get("animeId"));
        let subject_id = value_i64(task.get("subjectId"));
        if anime_id > 0 {
            history.insert((anime_id, episode));
        }
        if subject_id > 0 {
            history.insert((subject_id, episode));
        }
    }
    history
}

/// 问题 3 内核（纯函数，易测）：bangumi 条目（subjectId=S，anilistId=A）的
/// 新集事件命中已完成集合 (S, episode) 或 (A, episode) → 该集已有观看历史，
/// 应跳过创建 pending 任务。anilist 键条目不经过此守卫（其任务 id 查重本就
/// 覆盖 completed），由调用方负责。
#[cfg(feature = "standard")]
fn completed_history_blocks_event(
    history: &HashSet<(i64, i64)>,
    task_anime_id: i64,
    anilist_id: i64,
    episode: i64,
) -> bool {
    history.contains(&(task_anime_id, episode))
        || (anilist_id > 0 && history.contains(&(anilist_id, episode)))
}

/// 权威数据修复（共享 anilistId 认领分组）：following 中每个条目按其 AniList
/// 身份归组——bangumi 条目认领 `anilistId` 字段（≠自身 subjectId），anilist
/// （或缺省 source）条目认领自身 id。返回 anilistId → [(条目 id, followedAt)]。
/// 分季课程共占一个 AniList 条目时（如丧失篇 547888 与夺还篇 633836 共用
/// 189046），同一 anilistId 会出现多个认领条目。
#[cfg(feature = "standard")]
fn anilist_claimant_groups(state: &Value) -> HashMap<i64, Vec<(i64, i64)>> {
    let mut groups: HashMap<i64, Vec<(i64, i64)>> = HashMap::new();
    for item in state["following"].as_array().into_iter().flatten() {
        let id = value_i64(item.get("id"));
        if id <= 0 {
            continue;
        }
        let followed_at = value_i64(item.get("followedAt"));
        if value_string(item.get("source")) == "bangumi" {
            let anilist_id = value_i64(item.get("anilistId"));
            if anilist_id > 0 && anilist_id != id {
                groups
                    .entry(anilist_id)
                    .or_default()
                    .push((id, followed_at));
            }
        } else {
            groups.entry(id).or_default().push((id, followed_at));
        }
    }
    groups
}

/// 权威数据修复（共享 anilistId 主条目裁决）：同一 anilistId 被多个 following
/// 条目认领时，本轮 AniList 播出调度只分配给一个"主条目"——offline map
/// `anilistIndex[A]` 指向的条目优先（映射表的权威代表项），否则 followedAt
/// 最早的认领条目（并列取小 id 保证确定性）。仅含被共享的 anilistId。
#[cfg(feature = "standard")]
fn primary_anilist_claimants(state: &Value, map: &Value) -> HashMap<i64, i64> {
    anilist_claimant_groups(state)
        .into_iter()
        .filter(|(_, claimants)| claimants.len() > 1)
        .filter_map(|(anilist_id, claimants)| {
            Some((
                anilist_id,
                primary_anilist_claimant_for(&claimants, map, anilist_id)?,
            ))
        })
        .collect()
}

/// 主认领条目裁决的单 anilistId 版（与 [`primary_anilist_claimants`] 同规则，
/// 但单一认领也适用）：anilistIndex 指向者优先（需在认领集合内），否则
/// followedAt 最早（并列取小 id）。供 [`apply_anilist_authority_media`] 对每个
/// media 定位唯一接收条目——anilistId 非共享时同样要落到唯一条目上。
#[cfg(feature = "standard")]
fn primary_anilist_claimant_for(
    claimants: &[(i64, i64)],
    map: &Value,
    anilist_id: i64,
) -> Option<i64> {
    let indexed = value_i64(
        map.get("anilistIndex")
            .and_then(|index| index.get(anilist_id.to_string())),
    );
    if indexed > 0 && claimants.iter().any(|(id, _)| *id == indexed) {
        return Some(indexed);
    }
    claimants
        .iter()
        .copied()
        .min_by_key(|(id, followed_at)| (*followed_at, *id))
        .map(|(id, _)| id)
}

/// 权威数据修复（共享 anilistId 去重）：非主条目集合——这些条目跳过 AniList
/// 调度与离线调度（避免同集任务在两个条目下重复生成），其 pending 任务由
/// [`reconcile_anilist_authority_tasks`] 清理，completed 历史一律保留。
#[cfg(feature = "standard")]
fn secondary_anilist_claimant_ids(state: &Value, map: &Value) -> HashSet<i64> {
    let primary = primary_anilist_claimants(state, map);
    anilist_claimant_groups(state)
        .into_iter()
        .filter(|(anilist_id, _)| primary.contains_key(anilist_id))
        .flat_map(|(anilist_id, claimants)| {
            let primary_id = primary[&anilist_id];
            claimants
                .into_iter()
                .filter(move |(id, _)| *id != primary_id)
                .map(|(id, _)| id)
        })
        .collect()
}

// 同步主路径已改用 apply_airing_schedules_inner（共享 anilistId 去重需要传入
// skip_entries）；本便捷封装仅供单测调用，cfg(test) 避免正式构建 dead_code 告警。
#[cfg(all(test, not(target_os = "android")))]
fn apply_airing_schedules(state: &mut Value, schedules: &[Value], now: i64) -> AiringOutcome {
    apply_airing_schedules_inner(state, schedules, now, &HashSet::new())
}

/// `apply_airing_schedules` 的核心：`skip_entries` 为共享 anilistId 的非主条目
/// id 集合（权威数据修复），命中的条目整条跳过——不参与条目匹配、不写
/// nextAiringEpisode、不建任务，调度让给主条目。
#[cfg(not(target_os = "android"))]
fn apply_airing_schedules_inner(
    state: &mut Value,
    schedules: &[Value],
    now: i64,
    skip_entries: &HashSet<i64>,
) -> AiringOutcome {
    let create_tasks = value_bool(state["settings"].get("createWatchTasks"));
    let mut known: HashSet<String> = state["tasks"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|task| task.get("id").and_then(Value::as_str).map(str::to_string))
        .collect();
    let mut seen: HashSet<String> = state["seenAiringEvents"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect();
    // 问题 3（验收第 2 轮，旧版已完成任务被重新生成 pending）：旧版（AniList
    // 主键时代）完成任务挂在 anilistId 键（"21355-5"），新版 bangumi 条目按
    // subjectId 生成任务（"140001-5"），按任务 id 查重查不到 → 同一集重复建
    // pending。创建前先按"已完成集合"（status=completed 任务的 animeId 与
    // subjectId 两种键身份 + 同集）拦截：命中即视为该集已有观看历史。
    #[cfg(feature = "standard")]
    let completed_history = completed_episode_history(&state["tasks"]);
    #[cfg(not(feature = "standard"))]
    let _completed_history: HashSet<(i64, i64)> = HashSet::new();
    let mut outcome = AiringOutcome::default();
    for airing in schedules {
        let anime_id = value_i64(airing.get("mediaId"));
        let episode = value_i64(airing.get("episode"));
        let airing_at = value_i64(airing.get("airingAt"));
        if anime_id <= 0 || episode <= 0 || airing_at <= 0 {
            continue;
        }
        // 主键迁移后 bangumi 来源条目的 id 是 subjectId；AniList 播出调度
        // 仍按 AniList id 匹配（回退到 anilistId 兼容字段）。共享 anilistId 的
        // 非主条目（skip_entries）跳过：调度与 nextAiringEpisode 写回均让给
        // 主条目（anilistIndex 指向者优先，否则 followedAt 最早者）。
        let followed_index = state["following"].as_array().and_then(|items| {
            items.iter().position(|item| {
                !skip_entries.contains(&value_i64(item.get("id")))
                    && (value_i64(item.get("id")) == anime_id
                        || (value_string(item.get("source")) == "bangumi"
                            && value_i64(item.get("anilistId")) == anime_id))
            })
        });
        let Some(followed_index) = followed_index else {
            continue;
        };
        let followed_at = value_i64(state["following"][followed_index].get("followedAt"));
        if airing_at < followed_at {
            continue;
        }
        state["following"][followed_index]["nextAiringEpisode"] = airing
            .get("media")
            .and_then(|media| media.get("nextAiringEpisode"))
            .cloned()
            .unwrap_or(Value::Null);
        // 验收第 4 轮问题 1（防御）：AIRING_QUERY 窗口上沿是 `to: now+1`，
        // 未来集（airingAt > now）不进 seenAiringEvents、不建任务（产品契约
        // "新一集播出时创建任务"）；上方 nextAiringEpisode 展示更新保留。
        if airing_at > now {
            continue;
        }
        // 主键迁移后 bangumi 来源条目的任务以 subjectId 为键，避免被
        // merge_document_into_state 视为孤儿 pending 任务丢弃；AniList 播出
        // 调度的 mediaId 仍是 AniList id（上面已按 anilistId 匹配条目）。
        let followed = &state["following"][followed_index];
        // 主键迁移后 bangumi 来源条目的任务以 subjectId 为键，避免被
        // merge_document_into_state 视为孤儿 pending 任务丢弃；AniList 播出
        // 调度的 mediaId 仍是 AniList id（上面已按 anilistId 匹配条目），
        // 离线调度的 mediaId 则直接是 subjectId。
        let bangumi_sourced = value_string(followed.get("source")) == "bangumi";
        let task_anime_id = if bangumi_sourced {
            value_i64(followed.get("id"))
        } else {
            anime_id
        };
        let id = format!("{task_anime_id}-{episode}");
        if seen.insert(id.clone()) {
            state["seenAiringEvents"]
                .as_array_mut()
                .unwrap()
                .push(json!(id));
            outcome.aired += 1;
        }
        if !create_tasks {
            continue;
        }
        // 状态驱动追踪（任务 2 门控）：`bangumiStatus` 非空且非 doing（wish /
        // on_hold / done）→ 收录不追踪，不为新集创建观看任务。上方
        // nextAiringEpisode / seenAiringEvents 更新保留（供前端展示下一期）。
        // AniList 条目（bangumiStatus 为 null）不受影响。
        #[cfg(feature = "standard")]
        if bangumi_status_blocks_tracking(&value_string(
            state["following"][followed_index].get("bangumiStatus"),
        )) {
            continue;
        }
        if known.contains(&id) {
            continue;
        }
        // 问题 3 防重生成：仅拦 bangumi 条目（anilistId=A, subjectId=S）——
        // 已完成集合命中 (S, episode)（subjectId==S 或 animeId==S 的完成键）
        // 或 (A, episode)（animeId==A 的旧版完成键）→ 该集已有观看历史，跳过。
        // anilist 键条目不经过此守卫（其任务 id 查重本就覆盖 completed）。
        #[cfg(feature = "standard")]
        if bangumi_sourced {
            let anilist_id = value_i64(state["following"][followed_index].get("anilistId"));
            if completed_history_blocks_event(
                &completed_history,
                task_anime_id,
                anilist_id,
                episode,
            ) {
                continue;
            }
        }
        let title = value_string(state["following"][followed_index].get("displayTitle"));
        let cover = airing["media"]["coverImage"]["medium"]
            .as_str()
            .or(state["following"][followed_index]["coverImage"].as_str())
            .unwrap_or_default()
            .to_string();
        #[cfg_attr(not(feature = "standard"), expect(unused_mut))]
        let mut new_task = json!({"id": id, "animeId": task_anime_id, "animeTitle": title, "coverImage": cover, "episode": episode, "airingAt": airing_at, "status": "pending", "createdAt": now, "completedAt": null, "syncUpdatedAt": now_millis()});
        // standard 版任务补 additive 字段（original 不写，行为不变）。
        // subjectId 按 bangumi_sourced 判定：AniList 离线调度 mediaId=AniList id、
        // Bangumi 离线调度 mediaId=subjectId，两种情况任务键均为 subjectId。
        #[cfg(feature = "standard")]
        {
            new_task["subjectId"] = if bangumi_sourced {
                json!(task_anime_id)
            } else {
                Value::Null
            };
            new_task["episodeId"] = Value::Null;
            new_task["episodeSortKey"] = json!(episode.to_string());
            new_task["episodeType"] = json!("regular");
        }
        state["tasks"].as_array_mut().unwrap().push(new_task);
        known.insert(id);
        outcome.created += 1;
    }
    if let Some(events) = state["seenAiringEvents"].as_array_mut() {
        const MAX_SEEN_AIRING_EVENTS: usize = 2_000;
        if events.len() > MAX_SEEN_AIRING_EVENTS {
            events.drain(0..events.len() - MAX_SEEN_AIRING_EVENTS);
        }
    }
    outcome
}

/// 播出四级优先 ①（standard）：由离线映射 begin/broadcast 为 source=="bangumi"
/// 的追番条目本地生成逐期播出调度，形状与 AniList airingSchedule 一致
/// （`{mediaId: subjectId, episode, airingAt, media.nextAiringEpisode}`），
/// 供 apply_airing_schedules 灌入同一任务管道。
///
/// 规则：
/// - **anilistId 非空的条目整条跳过（权威数据修复）**：这些条目的播出数据完全
///   由 AniList AIRING_QUERY 提供（sync 按 anilistId 查询，覆盖所有此类条目）。
///   离线 bangumi-data 锚点（begin/broadcast 是流媒体上线时段，与正式播出存在
///   3-6 天周历错位）不再为其生成调度/任务/nextAiringEpisode——否则每轮同步
///   都会用离线值覆写 AniList 权威 nextAiringEpisode 并生成错期任务，形成自我
///   循环。仅无 anilistId 的条目走离线锚点（保留现逻辑 + 权威边界钳制）；
/// - episode 从 1 计（规则起点 = 选站后的 broadcast start / begin），超过条目
///   eps 截断（eps 未知时以防失控上限 1000 期兜底）；
/// - 只生成已播出的集（airingAt <= now；验收第 4 轮问题 1a：去掉 now+1 的
///   未来集，产品契约是"新一集播出时创建任务"）；下一未来期写入每个调度的
///   media.nextAiringEpisode（全部播完 → null）；
/// - 权威边界钳制（验收第 4 轮问题 1b）：following 条目带 AniList 权威
///   nextAiringEpisode（季度链已覆盖）时，episode >= nextAiringEpisode.episode
///   的集不生成（AniList 认为未播的不建）——离线 bangumi-data 锚点与 AniList
///   冲突时任务生成被钳制；无 AniList 数据维持 eps 钳制；
/// - 只有 begin 无 broadcast（电影/一次性）→ 单期，begin 已播则无下一期；
/// - 离线映射缺条目或全无播出数据 → 不生成任何调度（第四级：无数据→无任务）。
#[cfg(all(feature = "standard", not(target_os = "android"), test))]
fn bangumi_offline_schedules(state: &Value, map: &Value, now: i64) -> Vec<Value> {
    let preferred = preferred_broadcast_sites(state);
    let window_end = now;
    /// eps 未知的逐期推算上限（防异常数据导致无界循环）。
    const MAX_EPISODES_WITHOUT_EPS: i64 = 1_000;
    let mut schedules = Vec::new();
    for entry in state["following"].as_array().into_iter().flatten() {
        if value_string(entry.get("source")) != "bangumi" {
            continue;
        }
        // 权威数据修复：anilistId 非空的条目完全由 AniList AIRING_QUERY 提供
        // 播出数据；离线锚点不再参与（否则每轮同步覆写 AniList 权威
        // nextAiringEpisode——bangumi-data begin/broadcast 与正式播出存在周历
        // 错位，正是存量污染的根源）。仅无 anilistId 的条目走离线锚点。
        if value_i64(entry.get("anilistId")) > 0 {
            continue;
        }
        let subject_id = value_i64(entry.get("id"));
        if subject_id <= 0 {
            continue;
        }
        let Some(subject) = offline_bangumi_subject(map, subject_id) else {
            continue;
        };
        let eps = value_i64(entry.get("episodes"));
        // 验收第 4 轮问题 1b：AniList 权威边界（季度链已用 AniList 权威值覆盖
        // following 条目的 nextAiringEpisode）。存在时只生成 episode < 该值的集。
        let anilist_next_episode = entry
            .get("nextAiringEpisode")
            .and_then(|next| next.get("episode"))
            .and_then(Value::as_i64)
            .filter(|episode| *episode > 0);
        let begin = subject.get("begin").and_then(Value::as_str);
        let broadcast = subject.get("broadcast").and_then(Value::as_str);
        let sites = offline_broadcast_sites(&subject);
        if begin.is_none() && broadcast.is_none() && sites.is_empty() {
            continue; // 第四级：无播出数据 → 无任务。
        }
        let (selected_begin, selected_rule) =
            bangumi::select_broadcast_source(begin, broadcast, &sites, &preferred);
        let mut occurrences: Vec<(i64, i64)> = Vec::new();
        let mut next: Option<(i64, i64)> = None;
        if let Some((start, period)) = selected_rule.and_then(bangumi::parse_recurrence_rule) {
            if period.num_seconds() > 0 {
                let mut occurrence = start;
                let mut episode = 1i64;
                loop {
                    if eps > 0 && episode > eps {
                        break;
                    }
                    if episode > MAX_EPISODES_WITHOUT_EPS {
                        break;
                    }
                    if anilist_next_episode.is_some_and(|limit| episode >= limit) {
                        break;
                    }
                    if occurrence.timestamp() > window_end {
                        next = Some((episode, occurrence.timestamp()));
                        break;
                    }
                    occurrences.push((episode, occurrence.timestamp()));
                    let Some(next_occurrence) = occurrence.checked_add_signed(period) else {
                        break;
                    };
                    occurrence = next_occurrence;
                    episode += 1;
                }
            }
        } else if let Some(start) = selected_begin.and_then(bangumi::parse_instant) {
            // 一次性播出（电影/OVA/无周期数据）：只有第 1 期。
            if anilist_next_episode.is_none_or(|limit| limit > 1) {
                if start.timestamp() > window_end {
                    next = Some((1, start.timestamp()));
                } else {
                    occurrences.push((1, start.timestamp()));
                }
            }
        }
        if occurrences.is_empty() {
            // 窗口内无已播期（未开播/全部数据在窗口外/被 AniList 权威边界
            // 全部钳掉）→ 不生成调度，nextAiringEpisode 交由季度映射维护。
            continue;
        }
        // 权威钳制生效时 media.nextAiringEpisode 维持 AniList 值（离线推算的
        // "下一期"可能已被钳掉；apply_airing_schedules 会原样回写条目）。
        let next_json = match entry.get("nextAiringEpisode") {
            Some(authoritative) if authoritative.is_object() => authoritative.clone(),
            _ => next
                .map(|(episode, airing_at)| {
                    json!({
                        "episode": episode,
                        "airingAt": airing_at,
                        "timeUntilAiring": (airing_at - now).max(0)
                    })
                })
                .unwrap_or(Value::Null),
        };
        for (episode, airing_at) in occurrences {
            schedules.push(json!({
                "mediaId": subject_id,
                "episode": episode,
                "airingAt": airing_at,
                "media": {"nextAiringEpisode": next_json}
            }));
        }
    }
    schedules
}

/// Standard 播出真值：Bangumi episode 列表优先于 bangumi-data 的周期锚点。
/// `airdate` 只有日期时，只使用 Bangumi 给出的日期，不伪造分钟。
#[cfg(all(feature = "standard", not(target_os = "android")))]
fn bangumi_episode_timestamp(airdate: &str) -> Option<(i64, bool)> {
    let value = airdate.trim();
    if value.is_empty() {
        return None;
    }
    if let Ok(timestamp) = chrono::DateTime::parse_from_rfc3339(value) {
        return Some((timestamp.timestamp(), true));
    }
    let date = chrono::NaiveDate::parse_from_str(value.get(..10)?, "%Y-%m-%d").ok()?;
    let timestamp = date.and_hms_opt(0, 0, 0)?.and_utc().timestamp();
    Some((timestamp, false))
}

#[cfg(all(feature = "standard", not(target_os = "android")))]
fn bangumi_episode_airdate_is_aired(airdate: &str, now: i64) -> Option<bool> {
    let value = airdate.trim();
    if value.is_empty() {
        return None;
    }
    if let Ok(timestamp) = chrono::DateTime::parse_from_rfc3339(value) {
        return Some(timestamp.timestamp() <= now);
    }
    let date = chrono::NaiveDate::parse_from_str(value.get(..10)?, "%Y-%m-%d").ok()?;
    let now_date = chrono::DateTime::<chrono::Utc>::from_timestamp(now, 0)?.date_naive();
    Some(date < now_date)
}

#[cfg(all(feature = "standard", not(target_os = "android")))]
fn bangumi_episode_before_follow(airdate: &str, followed_at: i64) -> bool {
    if followed_at <= 0 {
        return false;
    }
    let value = airdate.trim();
    if value.is_empty() {
        return false;
    }
    if let Ok(timestamp) = chrono::DateTime::parse_from_rfc3339(value) {
        return timestamp.timestamp() < followed_at;
    }
    let Ok(air_date) = chrono::NaiveDate::parse_from_str(value.get(..10).unwrap_or_default(), "%Y-%m-%d") else {
        return false;
    };
    let Some(follow_date) = chrono::DateTime::<chrono::Utc>::from_timestamp(followed_at, 0)
        .map(|value| value.date_naive()) else {
        return false;
    };
    air_date < follow_date
}

#[cfg(all(feature = "standard", not(target_os = "android")))]
fn bangumi_episode_records_from_cache(
    cache_dir: &Path,
    subject_id: i64,
    now: i64,
    max_age: i64,
) -> Option<Vec<bangumi::BangumiEpisode>> {
    let path = cache_dir.join(format!("episodes-{subject_id}.json"));
    let raw = fs::read_to_string(path).ok()?;
    let value: Value = serde_json::from_str(&raw).ok()?;
    let fetched_at = value_i64(value.get("fetchedAt"));
    if fetched_at <= 0 || now.saturating_sub(fetched_at) > max_age {
        return None;
    }
    serde_json::from_value::<bangumi::Paged<bangumi::BangumiEpisode>>(
        value.get("paged").cloned().unwrap_or(Value::Null),
    )
    .ok()
    .map(|paged| paged.data)
}

#[cfg(all(feature = "standard", not(target_os = "android")))]
async fn load_bangumi_episode_records(
    context: &AppContext,
    client: &bangumi::HttpBangumiClient,
    subject_id: i64,
    now: i64,
) -> Option<Vec<bangumi::BangumiEpisode>> {
    if let Some(records) = bangumi_episode_records_from_cache(
        &bangumi_cache_dir(context),
        subject_id,
        now,
        BANGUMI_EPISODES_CACHE_TTL_SECONDS,
    ) {
        return Some(records);
    }
    match client
        .get_subject_episodes(subject_id, bangumi::SUBJECT_EPISODES_LIMIT_MAX, 0)
        .await
    {
        Ok(paged) => {
            let path = bangumi_cache_dir(context).join(format!("episodes-{subject_id}.json"));
            let _ = fs::create_dir_all(path.parent().unwrap_or_else(|| Path::new(".")));
            let _ = fs::write(path, json!({"fetchedAt": now, "paged": paged}).to_string());
            Some(paged.data)
        }
        Err(error) => {
            warn!("Bangumi episode 列表读取失败（subject {subject_id}）：{error}");
            bangumi_episode_records_from_cache(
                &bangumi_cache_dir(context),
                subject_id,
                now,
                i64::MAX,
            )
        }
    }
}

#[cfg(all(feature = "standard", not(target_os = "android")))]
fn bangumi_episode_schedule(
    records: &[bangumi::BangumiEpisode],
    now: i64,
) -> Vec<(i64, i64, i64, bool)> {
    let mut result = Vec::new();
    let mut seen = HashSet::new();
    for record in records {
        if record.id <= 0 || record.ep_type != 0 {
            continue;
        }
        let Some(episode) = bangumi_sync::episode_number(record) else {
            continue;
        };
        if !seen.insert(episode) {
            continue;
        }
        let Some((airing_at, _precise)) = record
            .airdate
            .as_deref()
            .and_then(bangumi_episode_timestamp)
        else {
            continue;
        };
        let aired = record
            .airdate
            .as_deref()
            .and_then(|date| bangumi_episode_airdate_is_aired(date, now))
            .unwrap_or(false);
        result.push((episode, record.id, airing_at, aired));
    }
    result.sort_by_key(|(episode, _, _, _)| *episode);
    result
}

#[cfg(all(feature = "standard", not(target_os = "android")))]
fn anilist_precise_airing_from_cache(
    cache_dir: &Path,
    anilist_id: i64,
    now: i64,
) -> HashMap<i64, i64> {
    if anilist_id <= 0 {
        return HashMap::new();
    }
    let Ok(raw) = fs::read_to_string(cache_dir.join(ANILIST_AUTHORITY_CACHE_FILE)) else {
        return HashMap::new();
    };
    let Ok(cache) = serde_json::from_str::<Value>(&raw) else {
        return HashMap::new();
    };
    let fetched_at = value_i64(cache.get("fetchedAt"));
    if fetched_at <= 0 || fetched_at < (now - ANILIST_PRECISE_AIRING_CACHE_MAX_AGE_SECS) * 1_000 {
        return HashMap::new();
    }
    cache["media"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|media| value_i64(media.get("id")) == anilist_id)
        .map(|media| {
            let mut precise: HashMap<i64, i64> = media["airingSchedule"]["nodes"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|node| {
                    let episode = value_i64(node.get("episode"));
                    let airing_at = value_i64(node.get("airingAt"));
                    (episode > 0 && airing_at > 0).then_some((episode, airing_at))
                })
                .collect();
            // Authority 查询的 schedule 只包含已播集；下一集仍在
            // nextAiringEpisode 中。它同样来自一次成功的 AniList 响应，必须
            // 写入可信缓存，否则上游短暂 403 时恰好会丢掉最需要的分钟提醒。
            let next_episode = value_i64(media["nextAiringEpisode"].get("episode"));
            let next_airing_at = value_i64(media["nextAiringEpisode"].get("airingAt"));
            if next_episode > 0 && next_airing_at > 0 {
                precise.insert(next_episode, next_airing_at);
            }
            precise
        })
        .unwrap_or_default()
}

#[cfg(all(feature = "standard", not(target_os = "android")))]
fn bangumi_episode_schedule_with_precision(
    records: &[bangumi::BangumiEpisode],
    now: i64,
    precise: &HashMap<i64, i64>,
) -> Vec<(i64, i64, i64, bool)> {
    let mut schedule = bangumi_episode_schedule(records, now);
    for (episode, _, airing_at, aired) in &mut schedule {
        // Usually AniList and Bangumi use the same episode number. For split
        // subjects they do not: AniList may expose the global continuation
        // number (e.g. 16) while Bangumi's subject uses local `ep` (e.g. 5).
        // In that case pair the precise timestamp by calendar date, which is
        // the stable identity shared by both sources, and retain Bangumi's
        // local episode number.
        let record = records.iter().find(|record| {
            record.ep_type == 0 && bangumi_sync::episode_number(record) == Some(*episode)
        });
        let record_date = record
            .and_then(|record| record.airdate.as_deref())
            .and_then(|airdate| airdate.get(..10));
        let local_and_global = record.and_then(|record| {
            record
                .ep
                .and_then(bangumi_sync::episode_sort_key)
                .zip(record.sort.and_then(bangumi_sync::episode_sort_key))
        });
        let same_calendar_date = |timestamp: i64| {
            let Some(date) = record_date else {
                return false;
            };
            chrono::DateTime::<chrono::Utc>::from_timestamp(timestamp, 0)
                .map(|value| value.date_naive().to_string() == date)
                .unwrap_or(false)
                || chrono::DateTime::<chrono::Utc>::from_timestamp(timestamp + 8 * 3600, 0)
                    .map(|value| value.date_naive().to_string() == date)
                    .unwrap_or(false)
        };
        let is_split_subject = local_and_global.is_some_and(|(local, global)| local != global);
        // Split subjects must never fall back to AniList's local episode number:
        // for Re:Zero 4th-season Dakkan, Bangumi ep=5 is not AniList ep=5.
        // Prefer the explicit Bangumi `sort` bridge when AniList exposes the
        // global number; otherwise pair by calendar date so a season-local
        // AniList numbering scheme (e.g. AniList ep=16) still maps to the
        // correct Bangumi ep=5 after a split.
        let candidate = if is_split_subject {
            local_and_global
                .and_then(|(_, global)| precise.get(&global).copied())
                .or_else(|| {
                    precise
                        .values()
                        .copied()
                        .find(|timestamp| same_calendar_date(*timestamp))
                })
        } else {
            precise.get(&episode).copied().or_else(|| {
                precise
                    .values()
                    .copied()
                    .find(|timestamp| same_calendar_date(*timestamp))
            })
        };
        let Some(candidate) = candidate else {
            continue;
        };
        let Some(_record) = records.iter().find(|record| {
            record.ep_type == 0 && bangumi_sync::episode_number(record) == Some(*episode)
        }) else {
            continue;
        };
        // 这里的 candidate 来自同一 AniList 条目，并且只有在 episode
        // 已经通过 Bangumi 逐集表匹配后才会进入 precise。因而 AniList
        // 可以纠正延期、停播和改档造成的“首播锚点 + P7D”错误；不能再用
        // Bangumi 的日期级值限制它，否则正确的分钟级时间会被静默丢弃。
        // 同时必须重新计算 aired——这是防止“Bangumi 误报已播 → 未来集
        // 被提前创建观看任务”的关键门禁。
        if candidate > 0 {
            *airing_at = candidate;
            *aired = candidate <= now;
        }
    }
    schedule
}

/// 将 Bangumi 逐集表应用到 following/tasks。该函数是一次性幂等愈合点：
/// - nextAiringEpisode 取第一条未播且有 airdate 的 episode；
/// - pending 只在逐集表明确为未来时删除，已完成任务永不删除；
/// - episodeId 缺失时绑定到对应 Bangumi episode；
/// - 只为明确已播集创建任务，不再用 begin + P7D 推算集数。
#[cfg(all(feature = "standard", not(target_os = "android")))]
fn apply_bangumi_episode_records_to_state(
    state: &mut Value,
    subject_id: i64,
    records: &[bangumi::BangumiEpisode],
    now: i64,
    create_tasks: bool,
    precise: Option<&HashMap<i64, i64>>,
) -> bool {
    let schedule = precise
        .map(|airing| bangumi_episode_schedule_with_precision(records, now, airing))
        .unwrap_or_else(|| bangumi_episode_schedule(records, now));
    if subject_id <= 0 || schedule.is_empty() {
        return false;
    }
    let Some(entry) = state["following"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|item| value_i64(item.get("id")) == subject_id)
        .cloned()
    else {
        return false;
    };
    let aliases = [subject_id, value_i64(entry.get("anilistId"))];
    let watched_episode = value_i64(entry.get("watchedEpisode"));
    let followed_at = value_i64(entry.get("followedAt"));
    let status = value_string(entry.get("bangumiStatus"));
    let tracking = status.is_empty() || status == "doing";
    let next = schedule.iter().find(|(_, _, _, aired)| !*aired).copied();
    let desired_next = next
        .map(|(episode, _, airing_at, _)| {
            json!({
                "episode": episode, "airingAt": airing_at,
                "timeUntilAiring": (airing_at - now).max(0), "source": "bangumi_episode"
            })
        })
        .unwrap_or(Value::Null);
    let mut changed = false;
    if let Some(item) = state["following"].as_array_mut().and_then(|items| {
        items
            .iter_mut()
            .find(|item| value_i64(item.get("id")) == subject_id)
    }) {
        let current_key = item
            .get("nextAiringEpisode")
            .map(|v| (value_i64(v.get("episode")), value_i64(v.get("airingAt"))))
            .unwrap_or((0, 0));
        let desired_key = (
            value_i64(desired_next.get("episode")),
            value_i64(desired_next.get("airingAt")),
        );
        if current_key != desired_key {
            item["nextAiringEpisode"] = desired_next;
            changed = true;
        }
    }
    let title = value_string(entry.get("displayTitle"));
    let cover = value_string(entry.get("coverImage"));
    let Some(tasks) = state["tasks"].as_array_mut() else {
        return changed;
    };
    let mut known: HashSet<i64> = tasks
        .iter()
        .filter(|task| {
            aliases.contains(&value_i64(task.get("animeId")))
                || value_i64(task.get("subjectId")) == subject_id
        })
        .map(|task| value_i64(task.get("episode")))
        .collect();
    tasks.retain_mut(|task| {
        if !aliases.contains(&value_i64(task.get("animeId")))
            && value_i64(task.get("subjectId")) != subject_id
        {
            return true;
        }
        let episode = value_i64(task.get("episode"));
        let completed = value_string(task.get("status")) == "completed";
        let Some((_, episode_id, airing_at, aired)) = schedule
            .iter()
            .find(|(number, _, _, _)| *number == episode)
            .copied()
        else {
            if !completed {
                let max_known = records
                    .iter()
                    .filter_map(bangumi_sync::episode_number)
                    .max()
                    .unwrap_or(0);
                if max_known > 0 && episode > max_known {
                    changed = true;
                    return false;
                }
                task["needsScheduleReview"] = json!(true);
                task["scheduleReviewReason"] = json!("Bangumi episode airdate unavailable");
                changed = true;
            }
            return true;
        };
        if !completed && !aired {
            changed = true;
            return false;
        }
        if value_i64(task.get("animeId")) != subject_id
            || value_i64(task.get("subjectId")) != subject_id
        {
            task["animeId"] = json!(subject_id);
            task["subjectId"] = json!(subject_id);
            task["id"] = json!(format!("{subject_id}-{episode}"));
            changed = true;
        }
        if value_i64(task.get("episodeId")) != episode_id {
            task["episodeId"] = json!(episode_id);
            changed = true;
        }
        if value_i64(task.get("airingAt")) != airing_at {
            task["airingAt"] = json!(airing_at);
            changed = true;
        }
        if task.get("airingSource").and_then(Value::as_str) != Some("bangumi_episode") {
            task["airingSource"] = json!("bangumi_episode");
            changed = true;
        }
        if let Some(object) = task.as_object_mut() {
            if object.remove("needsScheduleReview").is_some()
                || object.remove("scheduleReviewReason").is_some()
            {
                changed = true;
            }
        }
        true
    });
    if create_tasks && tracking {
        for (episode, episode_id, airing_at, aired) in &schedule {
            if !*aired
                || *episode <= watched_episode
                || known.contains(episode)
                || (followed_at > 0
                    && records
                        .iter()
                        .find(|record| {
                            record.ep_type == 0
                                && bangumi_sync::episode_number(record) == Some(*episode)
                        })
                        .and_then(|record| record.airdate.as_deref())
                        .is_some_and(|airdate| bangumi_episode_before_follow(airdate, followed_at)))
            {
                continue;
            }
            tasks.push(json!({
                "id": format!("{subject_id}-{episode}"), "animeId": subject_id,
                "subjectId": subject_id, "episode": episode, "episodeId": episode_id,
                "episodeSortKey": episode.to_string(), "episodeType": "regular",
                "animeTitle": title, "coverImage": cover, "airingAt": airing_at,
                "airingSource": "bangumi_episode", "status": "pending", "createdAt": now,
                "completedAt": Value::Null, "syncUpdatedAt": now_millis()
            }));
            known.insert(*episode);
            changed = true;
        }
    }
    changed
}

#[cfg(all(feature = "standard", not(target_os = "android")))]
async fn apply_bangumi_episode_authority(context: &AppContext, now: i64) -> bool {
    let base = {
        let Ok(state) = context.state.lock() else {
            return false;
        };
        bangumi_base_urls(&state)
    };
    let http = bangumi::HttpBangumiClient::new(context.client.clone(), base);
    let entries: Vec<Value> = {
        let Ok(state) = context.state.lock() else {
            return false;
        };
        state["following"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|entry| value_string(entry.get("source")) == "bangumi")
            .collect()
    };
    let create_tasks = context
        .state
        .lock()
        .ok()
        .map(|state| value_bool(state["settings"].get("createWatchTasks")))
        .unwrap_or(false);
    let mut changed = false;
    for entry in entries {
        let subject_id = value_i64(entry.get("id"));
        if subject_id <= 0 {
            continue;
        }
        let Some(records) = load_bangumi_episode_records(context, &http, subject_id, now).await
        else {
            continue;
        };
        #[cfg(all(feature = "standard", not(target_os = "android")))]
        let precise = anilist_precise_airing_from_cache(
            &bangumi_cache_dir(context),
            value_i64(entry.get("anilistId")),
            now,
        );
        #[cfg(target_os = "android")]
        let precise = HashMap::new();
        let schedule = bangumi_episode_schedule_with_precision(&records, now, &precise);
        if schedule.is_empty() {
            continue;
        }
        let next = schedule.iter().find(|(_, _, _, aired)| !*aired).cloned();
        let watched_episode = value_i64(entry.get("watchedEpisode"));
        let followed_at = value_i64(entry.get("followedAt"));
        let status = value_string(entry.get("bangumiStatus"));
        let tracking = status.is_empty() || status == "doing";
        let mut state = match context.state.lock() {
            Ok(state) => state,
            Err(_) => return changed,
        };
        let Some(following) = state["following"].as_array_mut() else {
            continue;
        };
        let Some(item) = following
            .iter_mut()
            .find(|item| value_i64(item.get("id")) == subject_id)
        else {
            continue;
        };
        let desired_next = next
            .map(|(episode, _, airing_at, _)| {
                json!({
                    "episode": episode,
                    "airingAt": airing_at,
                    "timeUntilAiring": (airing_at - now).max(0),
                    "source": "bangumi_episode"
                })
            })
            .unwrap_or(Value::Null);
        let current_key = item
            .get("nextAiringEpisode")
            .map(|v| (value_i64(v.get("episode")), value_i64(v.get("airingAt"))))
            .unwrap_or((0, 0));
        let desired_key = (
            value_i64(desired_next.get("episode")),
            value_i64(desired_next.get("airingAt")),
        );
        if current_key != desired_key {
            item["nextAiringEpisode"] = desired_next;
            changed = true;
        }
        let aliases = [subject_id, value_i64(item.get("anilistId"))];
        let entry_title = value_string(item.get("displayTitle"));
        let entry_cover = value_string(item.get("coverImage"));
        let _ = item;
        let _ = following;
        let Some(tasks) = state["tasks"].as_array_mut() else {
            continue;
        };
        let mut known: HashSet<i64> = tasks
            .iter()
            .filter(|task| {
                aliases.contains(&value_i64(task.get("animeId")))
                    || value_i64(task.get("subjectId")) == subject_id
            })
            .map(|task| value_i64(task.get("episode")))
            .collect();
        tasks.retain_mut(|task| {
            if !aliases.contains(&value_i64(task.get("animeId")))
                && value_i64(task.get("subjectId")) != subject_id
            {
                return true;
            }
            let episode = value_i64(task.get("episode"));
            let Some((_, episode_id, airing_at, aired)) = schedule
                .iter()
                .find(|(number, _, _, _)| *number == episode)
                .copied()
            else {
                if value_string(task.get("status")) == "pending" {
                    let max_known = records
                        .iter()
                        .filter_map(bangumi_sync::episode_number)
                        .max()
                        .unwrap_or(0);
                    if max_known > 0 && episode > max_known {
                        changed = true;
                        return false;
                    }
                    task["needsScheduleReview"] = json!(true);
                    task["scheduleReviewReason"] = json!("Bangumi episode airdate unavailable");
                    changed = true;
                }
                return true;
            };
            if value_string(task.get("status")) == "pending" && !aired {
                changed = true;
                return false;
            }
            if value_i64(task.get("animeId")) != subject_id
                || value_i64(task.get("subjectId")) != subject_id
            {
                task["animeId"] = json!(subject_id);
                task["subjectId"] = json!(subject_id);
                task["id"] = json!(format!("{subject_id}-{episode}"));
                changed = true;
            }
            if value_i64(task.get("episodeId")) != episode_id {
                task["episodeId"] = json!(episode_id);
                changed = true;
            }
            if value_i64(task.get("airingAt")) != airing_at {
                task["airingAt"] = json!(airing_at);
                changed = true;
            }
            if task.get("needsScheduleReview").is_some() {
                task.as_object_mut().map(|object| {
                    object.remove("needsScheduleReview");
                    object.remove("scheduleReviewReason");
                });
                changed = true;
            }
            task["airingSource"] = json!("bangumi_episode");
            true
        });
        if create_tasks && tracking {
            for (episode, episode_id, airing_at, aired) in &schedule {
                if !*aired
                    || *episode <= watched_episode
                    || known.contains(episode)
                    || (followed_at > 0
                        && records
                            .iter()
                            .find(|record| {
                                record.ep_type == 0
                                    && bangumi_sync::episode_number(record) == Some(*episode)
                            })
                            .and_then(|record| record.airdate.as_deref())
                            .is_some_and(|airdate| bangumi_episode_before_follow(airdate, followed_at)))
                {
                    continue;
                }
                let task = json!({
                    "id": format!("{subject_id}-{episode}"), "animeId": subject_id,
                    "subjectId": subject_id, "episode": episode, "episodeId": episode_id,
                    "episodeSortKey": episode.to_string(), "episodeType": "regular",
                    "animeTitle": entry_title, "coverImage": entry_cover,
                    "airingAt": airing_at, "airingSource": "bangumi_episode", "status": "pending",
                    "createdAt": now, "completedAt": Value::Null, "syncUpdatedAt": now_millis()
                });
                tasks.push(task);
                known.insert(*episode);
                changed = true;
            }
        }
    }
    changed
}

/// WebDAV 合并后的本地缓存愈合：不触网，只使用最近一次成功获取的逐集表。
/// 这一步必须发生在上传前，避免坚果云中的未来假票再次复活。
#[cfg(all(feature = "standard", not(target_os = "android")))]
fn apply_cached_bangumi_episode_authority(state: &mut Value, cache_dir: &Path, now: i64) -> bool {
    let entries = state["following"].as_array().cloned().unwrap_or_default();
    let create_tasks = value_bool(state["settings"].get("createWatchTasks"));
    let mut changed = false;
    for entry in entries
        .into_iter()
        .filter(|entry| value_string(entry.get("source")) == "bangumi")
    {
        let subject_id = value_i64(entry.get("id"));
        if subject_id <= 0 {
            continue;
        }
        let Some(records) =
            bangumi_episode_records_from_cache(cache_dir, subject_id, now, i64::MAX)
        else {
            continue;
        };
        #[cfg(all(feature = "standard", not(target_os = "android")))]
        let precise =
            anilist_precise_airing_from_cache(cache_dir, value_i64(entry.get("anilistId")), now);
        #[cfg(target_os = "android")]
        let precise = HashMap::new();
        changed |= apply_bangumi_episode_records_to_state(
            state,
            subject_id,
            &records,
            now,
            create_tasks,
            Some(&precise),
        );
    }
    changed
}

// -- 权威数据修复（缺口 1/2 网络自愈）：AniList 全量重写 + 任务纠偏 ----------
// 第 5 轮遗留实测：AIRING_QUERY 只返回窗口内播出的集——窗口内零播出时无
// media，nextAiringEpisode 污染（100女友 ep10@9/9、黄泉 ep24@9/13）永远等不到
// 权威写回；purge 只删 airingAt > now，过去时间假任务留存。本组函数按
// anilistId 无条件全量拉取 next + 已播 schedule，与播出窗口解耦。

/// 权威全量重写查询（与 AIRING_QUERY 的窗口语义解耦）：nextAiringEpisode +
/// 已播 airingSchedule（notYetAired:false，TIME_DESC 取最近 25 集）。分页
/// ≤3（perPage 50 → 单轮 ≤150 条目）。
#[cfg(all(feature = "standard", not(target_os = "android")))]
const ANILIST_AUTHORITY_QUERY: &str = r#"query AniListAuthority($ids: [Int], $page: Int) {
  Page(page: $page, perPage: 50) { pageInfo { lastPage }
    media(id_in: $ids, type: ANIME) {
      id
      nextAiringEpisode { episode airingAt }
      airingSchedule(notYetAired: false, perPage: 50) { nodes { episode airingAt } }
    }
  }
}"#;

/// 抓取 AniList 权威 media（anilistId → media）。任一页失败（网络/解析/GraphQL
/// errors）静默 None——调用方整体放弃本轮应用（不做半套改写）。
#[cfg(all(feature = "standard", not(target_os = "android")))]
async fn fetch_anilist_authority_media(
    client: &reqwest::Client,
    endpoint: &str,
    ids: &[i64],
) -> Option<HashMap<i64, Value>> {
    if ids.is_empty() {
        return None;
    }
    let mut media_by_id: HashMap<i64, Value> = HashMap::new();
    for page in 1..=3usize {
        let data = anilist_request_at(
            client,
            endpoint,
            ANILIST_AUTHORITY_QUERY,
            json!({"ids": ids, "page": page}),
        )
        .await
        .ok()?;
        let last_page = value_i64(data["Page"]["pageInfo"].get("lastPage")).clamp(1, 3) as usize;
        for media in data["Page"]["media"]
            .as_array()
            .cloned()
            .unwrap_or_default()
        {
            let id = value_i64(media.get("id"));
            if id > 0 {
                media_by_id.insert(id, media);
            }
        }
        if page >= last_page {
            break;
        }
    }
    Some(media_by_id)
}

/// 权威数据应用（纯同步，易测）：对每个 media 定位主认领条目
/// （[`primary_anilist_claimant_for`]，secondary 跳过），然后——
/// 1) **next 重写（治愈污染）**：media.nextAiringEpisode 存在 → 条目
///    nextAiringEpisode = {episode, airingAt, timeUntilAiring: airingAt-now}
///    无条件替换；为 null（完结）→ 条目 nextAiringEpisode = null。只比较
///    (episode, airingAt) 判定变更（timeUntilAiring 随 now 漂移，不参与幂等
///    比较）；
/// 2) **任务纠偏**：schedule + next 构造 episode→airingAt 权威映射，对该条目
///    pending 任务（animeId 命中条目 id 或旧键 anilistId）：a) episode >=
///    next.episode → 删除（未播出不该有票，播出后 AIRING_QUERY 按权威时间
///    重建；真实数据中已播 schedule 与 next 不相交，异常数据相交时也删除
///    优先，避免改写成未来 airingAt 后又被 purge 删除来回抖动）；b) episode
///    在映射中且 airingAt 不同 → 改写 airingAt + syncUpdatedAt=now 毫秒；
///    c) 无法判定 → 保留。completed 一律不动。
/// 3) **权威回填（第 6 轮误删事故修复）**：对追踪中的条目（bangumiStatus
///    为空或 "doing"；wish/on_hold/done 不回填；且 settings.createWatchTasks
///    开启——回填即自动建任务，须与调度管道同一契约）与映射内每个已播集 E
///    （仅 schedule 节点，perPage 50 已播；不处理未来集）：该条目名下不存在
///    任何任务（pending 或 completed；匹配 animeId==条目 id 或 subjectId==
///    条目 id，含旧键 animeId==anilistId——旧键存在时不动它，canonicalize
///    会归一）→ 创建 pending 任务（id="{S}-{E}"、episodeSortKey/episodeType
///    等 additive 字段与调度管道同形）。背景：桌面 next 被污染时 >= next
///    清理曾误删真实已播任务（100女友 ep10），而其 id 已在 seenAiringEvents、
///    AIRING_QUERY 窗口永不重建——权威 schedule 是唯一可信修复源。回填在
///    删除之后执行且跳过 >= next 的集（含 next 本集，其时间仅存在于纠偏
///    映射的 next 补充项），绝不复活假票。回填任务的 id 经
///    ensure_sync_metadata 自然进入 seenAiringEvents，无需额外处理。
///    第 8 轮问题 3 回填门槛：E 的 airingAt < followedAt（followedAt>0 才
///    判）→ 跳过（追番前的历史不是待办，曾把存量灌成约 60 条 pending 洪
///    流）；条目 watchedEpisode 已知（>0）且 E <= watchedEpisode → 跳过
///    （已看过的集不回填）。
/// 任一变更返回 true。本窗口刚建的已播任务 episode < next.episode 不受影响。
#[cfg(all(test, feature = "standard", not(target_os = "android")))]
fn apply_anilist_authority_media(
    state: &mut Value,
    map: &Value,
    media_by_id: &HashMap<i64, Value>,
    now: i64,
) -> bool {
    apply_anilist_authority_media_inner(state, map, media_by_id, now, false)
}

/// Apply the AniList snapshot while optionally leaving Bangumi-primary entries
/// to the per-subject Bangumi episode authority.  AniList can expose a shared
/// global episode number for split subjects (for example 82 while the current
/// Bangumi subject is local episode 5); writing that number directly into a
/// Bangumi following record recreates the exact next/task corruption this path
/// is meant to heal.
#[cfg(all(feature = "standard", not(target_os = "android")))]
fn apply_anilist_authority_media_inner(
    state: &mut Value,
    map: &Value,
    media_by_id: &HashMap<i64, Value>,
    now: i64,
    skip_bangumi_entries: bool,
) -> bool {
    if media_by_id.is_empty() {
        return false;
    }
    // 回填与调度管道同契约：createWatchTasks 关闭时不自动建任务（否则权威
    // 回填会为关闭该设置的设备灌满整季待看）。
    let create_tasks = value_bool(state["settings"].get("createWatchTasks"));
    let groups = anilist_claimant_groups(state);
    let mut changed = false;
    for (anilist_id, media) in media_by_id {
        let Some(claimants) = groups.get(anilist_id) else {
            continue; // 不再追番 / 无认领条目 → 不改写。
        };
        let Some(entry_id) = primary_anilist_claimant_for(claimants, map, *anilist_id) else {
            continue;
        };
        let Some(entry) = state["following"].as_array().and_then(|items| {
            items
                .iter()
                .find(|item| value_i64(item.get("id")) == entry_id)
        }) else {
            continue;
        };
        let entry_source = value_string(entry.get("source"));
        if skip_bangumi_entries && entry_source == "bangumi" {
            // Bangumi entries are reconciled from /v0/subjects/{id}/episodes
            // below, with AniList timestamps used only as a minute-precision
            // cache.  Never apply AniList's global episode number directly.
            continue;
        }
        let entry_anilist_id = value_i64(entry.get("anilistId"));
        // 回填所需条目字段在条目被改写前提取（后面有 following 可变借用）。
        let entry_bangumi_status = value_string(entry.get("bangumiStatus"));
        let entry_title = value_string(entry.get("displayTitle"));
        let entry_cover = value_string(entry.get("coverImage"));
        // 第 8 轮问题 3 回填门槛：追番时间与已看进度（followedAt 秒；
        // watchedEpisode null → 0 视为未知）。
        let entry_followed_at = value_i64(entry.get("followedAt"));
        let entry_watched_episode = value_i64(entry.get("watchedEpisode"));
        // 1) next 全量重写（无条件替换治愈污染；完结 → null）。
        let raw_next = media
            .get("nextAiringEpisode")
            .cloned()
            .unwrap_or(Value::Null);
        let desired_next = if raw_next.is_null() {
            Value::Null
        } else {
            let episode = value_i64(raw_next.get("episode"));
            let airing_at = value_i64(raw_next.get("airingAt"));
            if episode > 0 && airing_at > 0 {
                json!({
                    "episode": episode,
                    "airingAt": airing_at,
                    "timeUntilAiring": (airing_at - now).max(0)
                })
            } else {
                Value::Null
            }
        };
        let next_episode = value_i64(desired_next.get("episode"));
        let next_airing_at = value_i64(desired_next.get("airingAt"));
        let current_key = entry
            .get("nextAiringEpisode")
            .map(|current| {
                (
                    value_i64(current.get("episode")),
                    value_i64(current.get("airingAt")),
                )
            })
            .unwrap_or((0, 0));
        let desired_key = if desired_next.is_null() {
            (0, 0)
        } else {
            (next_episode, next_airing_at)
        };
        if current_key != desired_key {
            if let Some(slot) = state["following"].as_array_mut().and_then(|items| {
                items
                    .iter_mut()
                    .find(|item| value_i64(item.get("id")) == entry_id)
            }) {
                slot["nextAiringEpisode"] = desired_next;
            }
            changed = true;
        }
        // 2) 任务纠偏：episode→airingAt 权威映射（已播 schedule 为主，next
        //    补充其"下一未播集"的时间）。
        let mut authoritative_airing: HashMap<i64, i64> = HashMap::new();
        for node in media["airingSchedule"]["nodes"]
            .as_array()
            .into_iter()
            .flatten()
        {
            let episode = value_i64(node.get("episode"));
            let airing_at = value_i64(node.get("airingAt"));
            if episode > 0 && airing_at > 0 {
                authoritative_airing.insert(episode, airing_at);
            }
        }
        // 已播集回填源（此刻映射仅含 schedule 节点，即 AniList
        // notYetAired:false 的已播集；升序输出稳定）。next 补充项在快照之后
        // 才并入纠偏映射，只服务于时间改写，绝不进入回填。
        let mut aired_airing: Vec<(i64, i64)> = authoritative_airing
            .iter()
            .map(|(&episode, &airing_at)| (episode, airing_at))
            .collect();
        aired_airing.sort_unstable();
        if next_episode > 0 {
            authoritative_airing
                .entry(next_episode)
                .or_insert(next_airing_at);
        }
        let sync_updated_at = now_millis();
        let Some(tasks) = state
            .get_mut("tasks")
            .and_then(|tasks| tasks.as_array_mut())
        else {
            continue;
        };
        tasks.retain_mut(|task| {
            if value_string(task.get("status")) != "pending" {
                return true; // completed 观看历史一律不动。
            }
            let anime_id = value_i64(task.get("animeId"));
            let belongs = anime_id == entry_id
                || (entry_source == "bangumi"
                    && entry_anilist_id > 0
                    && anime_id == entry_anilist_id);
            if !belongs {
                return true;
            }
            let episode = value_i64(task.get("episode"));
            // a) 未播假票删除：AniList 认为该集未播（>= next.episode）。
            if next_episode > 0 && episode > 0 && episode >= next_episode {
                changed = true;
                return false;
            }
            // b) 已播集权威时间纠偏（如描绘 ep10@9/4 23:35 → 22:30）。
            if let Some(&airing_at) = authoritative_airing.get(&episode) {
                if airing_at != value_i64(task.get("airingAt")) {
                    task["airingAt"] = json!(airing_at);
                    task["syncUpdatedAt"] = json!(sync_updated_at);
                    changed = true;
                }
                return true;
            }
            // c) 无法判定 → 保留。
            true
        });
        // 3) 权威回填（删除/改写之后执行）：追踪中条目的已播集若名下无任何
        //    任务 → 补建 pending。>= next 的集（含 next 本集）一律跳过——
        //    假票刚被删除，回填绝不复活；同一集已有 pending/completed/旧键
        //    （animeId==anilistId）记录时不动（旧键由 canonicalize_cross_key_tasks
        //    归一）。回填只处理映射内已播集（perPage 50），不处理未来集、
        //    不触碰既有 completed。
        //    第 8 轮问题 3 回填门槛（回填洪流治理）：a) airingAt < followedAt
        //    （followedAt>0 才判）→ 追番前已播的历史不是待办（用户早已看过，
        //    本地只是无任务记录，曾把黄泉 1..16 等约 60 条存量灌成 pending）；
        //    b) 条目 watchedEpisode 已知（>0）且 episode <= watchedEpisode →
        //    已看过的集不回填。
        if create_tasks && !bangumi_status_blocks_tracking(&entry_bangumi_status) {
            let mut known_episodes: HashSet<i64> = tasks
                .iter()
                .filter(|task| {
                    let anime_id = value_i64(task.get("animeId"));
                    let subject_id = value_i64(task.get("subjectId"));
                    anime_id == entry_id
                        || subject_id == entry_id
                        || (entry_source == "bangumi"
                            && entry_anilist_id > 0
                            && (anime_id == entry_anilist_id || subject_id == entry_anilist_id))
                })
                .map(|task| value_i64(task.get("episode")))
                .collect();
            // 任务键与调度管道同形：bangumi 条目按 subjectId，anilist 条目
            // 按 AniList id（其条目 id 即 AniList id）。
            let task_anime_id = if entry_source == "bangumi" {
                entry_id
            } else {
                *anilist_id
            };
            for (episode, airing_at) in aired_airing {
                if (next_episode > 0 && episode >= next_episode)
                    || known_episodes.contains(&episode)
                    || (entry_followed_at > 0
                        && media["airingSchedule"]["nodes"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .find(|node| value_i64(node.get("episode")) == episode)
                            .and_then(|node| node.get("airingAt"))
                            .is_some_and(|value| value_i64(Some(value)) < entry_followed_at))
                    || (entry_watched_episode > 0 && episode <= entry_watched_episode)
                {
                    continue;
                }
                let mut new_task = json!({
                    "id": format!("{task_anime_id}-{episode}"),
                    "animeId": task_anime_id,
                    "animeTitle": entry_title,
                    "coverImage": entry_cover,
                    "episode": episode,
                    "airingAt": airing_at,
                    "status": "pending",
                    "createdAt": now,
                    "completedAt": Value::Null,
                    "syncUpdatedAt": sync_updated_at,
                });
                new_task["subjectId"] = if entry_source == "bangumi" {
                    json!(entry_id)
                } else {
                    Value::Null
                };
                new_task["episodeId"] = Value::Null;
                new_task["episodeSortKey"] = json!(episode.to_string());
                new_task["episodeType"] = json!("regular");
                tasks.push(new_task);
                known_episodes.insert(episode);
                changed = true;
            }
        }
    }
    changed
}

/// 权威数据修复本轮入口（desktop sync_now_inner 接线）：先**不持锁**全量抓取
/// （std MutexGuard 绝不能跨 await——Tauri 命令 future 必须 Send），抓取成功即
/// 落缓存（第 8 轮问题 1 止血：供 perform_webdav_sync 云端合并后免网络套用），
/// 再持锁应用（持锁段内无 await）。失败（网络/解析/锁）静默 false。state 为
/// 共享状态锁本体，await 只发生在抓取段。Android 无此路径（最小对齐走
/// reconcile_unaired_anilist_next_tasks，完整 schedule 纠偏留 Java Worker 后续）。
#[cfg(all(feature = "standard", not(target_os = "android")))]
async fn anilist_authority_refresh(
    state: &Mutex<Value>,
    map: &Value,
    endpoint: &str,
    client: &reqwest::Client,
    ids: &[i64],
    cache_dir: &Path,
    now: i64,
) -> bool {
    let Some(media_by_id) = fetch_anilist_authority_media(client, endpoint, ids).await else {
        return false;
    };
    // 缓存 best-effort：写失败只影响本轮 webdav 合并后的免网络纠偏，
    // 下一轮 sync_now 成功抓取会重写。
    write_anilist_authority_cache(cache_dir, &media_by_id);
    let Ok(mut state) = state.lock() else {
        return false;
    };
    apply_anilist_authority_media_inner(&mut state, map, &media_by_id, now, true)
}

/// 第 8 轮问题 1：AniList 权威 media 缓存文件（bangumi-cache/ 下，纳入
/// clear_cache 清理范围）。形状 {fetchedAt: 毫秒, media: [原始 JSON]}。
#[cfg(all(feature = "standard", not(target_os = "android")))]
const ANILIST_AUTHORITY_CACHE_FILE: &str = "anilist-authority.json";
/// 云端合并后的全量权威重放 TTL。这里只允许很新的快照直接覆盖状态，避免
/// 长时间离线的旧响应把用户刚刚得到的新状态又改回去。
#[cfg(all(feature = "standard", not(target_os = "android")))]
const ANILIST_AUTHORITY_HEAL_CACHE_TTL_SECS: i64 = 1_800;
/// 分钟级播出缓存的最大保留时间。它只来自成功的 AniList 响应、只在 Bangumi
/// 已确认同一集存在时使用；上游暂时不可用时保留一周，超过后回落日期级，绝不
/// 从同步状态或旧任务反推“精确”时间。
#[cfg(all(feature = "standard", not(target_os = "android")))]
const ANILIST_PRECISE_AIRING_CACHE_MAX_AGE_SECS: i64 = 7 * 86_400;

/// 第 8 轮问题 1：抓取成功的权威 media 序列化落盘（毫秒时间戳 + 原始 JSON
/// 数组，与 AniList 响应同形，重放无需任何转换）。
#[cfg(all(feature = "standard", not(target_os = "android")))]
fn write_anilist_authority_cache(cache_dir: &Path, media_by_id: &HashMap<i64, Value>) {
    let cache = json!({
        "fetchedAt": now_millis(),
        "media": media_by_id.values().collect::<Vec<&Value>>(),
    });
    let payload = serde_json::to_string(&cache).unwrap_or_default();
    if payload.is_empty() {
        return;
    }
    let _ = fs::write(cache_dir.join(ANILIST_AUTHORITY_CACHE_FILE), payload);
}

/// 第 8 轮问题 1 止血（桌面）：读取 fresh 缓存（TTL [`ANILIST_AUTHORITY_HEAL_CACHE_TTL_SECS`]
/// 内）并复用 [`apply_anilist_authority_media`] 同一应用逻辑（不重新抓取、
/// 不触网）。无缓存/过期/解析失败/媒体空 → false 不阻塞（webdav 合并照旧
/// 上传，权威纠正等下一轮 sync_now 重新抓取）。now 为秒，缓存 fetchedAt 为
/// 毫秒。
#[cfg(all(feature = "standard", not(target_os = "android")))]
fn apply_cached_anilist_authority_with_mode(
    state: &mut Value,
    map: &Value,
    cache_dir: &Path,
    now: i64,
    skip_bangumi_entries: bool,
) -> bool {
    let Ok(raw) = fs::read_to_string(cache_dir.join(ANILIST_AUTHORITY_CACHE_FILE)) else {
        return false;
    };
    let Ok(cache) = serde_json::from_str::<Value>(&raw) else {
        return false;
    };
    let fetched_at = value_i64(cache.get("fetchedAt"));
    let fresh_floor = (now - ANILIST_AUTHORITY_HEAL_CACHE_TTL_SECS) * 1_000;
    if fetched_at <= 0 || fetched_at < fresh_floor {
        return false;
    }
    let media_by_id: HashMap<i64, Value> = cache["media"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|media| {
            let id = value_i64(media.get("id"));
            (id > 0).then_some((id, media.clone()))
        })
        .collect();
    if media_by_id.is_empty() {
        return false;
    }
    apply_anilist_authority_media_inner(state, map, &media_by_id, now, skip_bangumi_entries)
}

#[cfg(all(test, feature = "standard", not(target_os = "android")))]
fn apply_cached_anilist_authority(
    state: &mut Value,
    map: &Value,
    cache_dir: &Path,
    now: i64,
) -> bool {
    // Unit fixtures exercise the standalone AniList authority kernel. The
    // production WebDAV path uses the split-aware mode below and then applies
    // cached Bangumi episode authority as the final source of truth.
    apply_cached_anilist_authority_with_mode(state, map, cache_dir, now, false)
}

#[cfg(not(target_os = "android"))]
async fn sync_now_inner(app: &AppHandle, context: &AppContext) -> Result<Value, String> {
    let (ids, from) = {
        let state = context.state.lock().map_err(|_| "状态锁不可用")?;
        (
            state["following"]
                .as_array()
                .unwrap_or(&vec![])
                .iter()
                .map(|item| {
                    // 主键迁移后 bangumi 来源条目按 anilistId 查询 AniList。
                    let id = value_i64(item.get("id"));
                    if value_string(item.get("source")) == "bangumi"
                        && value_i64(item.get("anilistId")) > 0
                    {
                        value_i64(item.get("anilistId"))
                    } else {
                        id
                    }
                })
                .filter(|id| *id > 0)
                .collect::<Vec<_>>(),
            value_i64(state.get("lastSyncAt")),
        )
    };
    // 权威数据修复：共享 anilistId（分季课程共占一个 AniList 条目）只查一次。
    let mut ids = ids;
    ids.sort_unstable();
    ids.dedup();
    let now = now_seconds();
    if ids.is_empty() {
        let mut state = context.state.lock().map_err(|_| "状态锁不可用")?;
        state["lastSyncAt"] = json!(now);
        drop(state);
        context.save_state().map_err(|error| error.to_string())?;
        emit_state(app, context);
        return Ok(json!({"created": 0, "syncedAt": now}));
    }
    // Standard 播出纠偏必须独立于 AniList：AniList 暂时不可用（例如 403）时，
    // 仍要依据本地/缓存的 Bangumi episode 表清理未来假票并修正 next。
    #[cfg(feature = "standard")]
    {
        if apply_bangumi_episode_authority(context, now).await {
            context.save_state().map_err(|error| error.to_string())?;
            emit_state(app, context);
            context.webdav_wakeup.notify_one();
        }
    }
    let mut schedules = Vec::new();
    #[cfg(feature = "standard")]
    let mut anilist_warning: Option<String> = None;
    for page in 1..=10 {
        let response = match anilist_request(
            &context,
            AIRING_QUERY,
            json!({"ids": ids, "from": from.min(now - 60), "to": now + 1, "page": page}),
        )
        .await
        {
            Ok(response) => response,
            Err(error) => {
                // Original 完全依赖 AniList，仍将错误如实交给调用方；Standard
                // 则已先跑过 Bangumi 逐集纠偏，AniList 暂停（2026-09 的 403）
                // 不能阻断安全降级和本地/坚果云自愈。
                #[cfg(feature = "standard")]
                {
                    let warning = format!("AniList 分钟级播出数据暂不可用：{error}");
                    warn!("{warning}");
                    anilist_warning = Some(warning);
                    break;
                }
                #[cfg(not(feature = "standard"))]
                return Err(error.to_string());
            }
        };
        schedules.extend(
            response["Page"]["airingSchedules"]
                .as_array()
                .cloned()
                .unwrap_or_default(),
        );
        if !value_bool(response["Page"]["pageInfo"].get("hasNextPage")) {
            break;
        }
    }
    // Standard 的 Bangumi 追番不再接入 bangumi-data begin/broadcast 生成的
    // 周播调度。它只能描述首播锚点，遇到停播/改档会把集数整体提前；逐集
    // /v0/episodes 已在本函数开始和结束时承担真正的任务/next 裁决。
    // 块作用域持锁（std MutexGuard 不能跨 await——Tauri 命令 future 必须 Send）。
    let outcome = {
        let mut state = context.state.lock().map_err(|_| "状态锁不可用")?;
        // 权威数据修复：同一 anilistId 被多个 following 条目认领时，本轮 AniList
        // 调度只分配给主条目（anilistIndex 指向者优先，否则 followedAt 最早者），
        // 其余条目跳过调度与 nextAiringEpisode 写回，避免同集任务在两个条目下
        // 重复生成。AIRING_QUERY 返回的 media.nextAiringEpisode 每轮随调度写回
        // 主条目（治愈被离线锚点污染的存量值；离线调度已跳过 anilistId 条目，
        // 不会再在其后覆写）。
        #[cfg(feature = "standard")]
        let mut secondary_claimants =
            secondary_anilist_claimant_ids(&state, &context.offline_bangumi);
        #[cfg(feature = "standard")]
        if let Some(items) = state["following"].as_array() {
            // Standard Bangumi entries are reconciled by their subject episode
            // table.  Do not let the shared AniList schedule transiently create
            // global-numbered tasks before that reconciliation runs.
            secondary_claimants.extend(items.iter().filter_map(|item| {
                (value_string(item.get("source")) == "bangumi")
                    .then(|| value_i64(item.get("id")))
                    .filter(|id| *id > 0)
            }));
        }
        #[cfg(not(feature = "standard"))]
        let secondary_claimants = HashSet::new();
        apply_airing_schedules_inner(&mut state, &schedules, now, &secondary_claimants)
    };
    // Bangumi 确认 episode 身份；已验证 AniList 缓存可补齐分钟与改档后的
    // 日期。最终再跑一次是为把本轮成功的 AniList 精确响应套回逐集表。
    #[cfg(feature = "standard")]
    if apply_bangumi_episode_authority(context, now).await {
        context.save_state().map_err(|error| error.to_string())?;
        emit_state(app, context);
        context.webdav_wakeup.notify_one();
    }
    // 权威数据修复（缺口 1/2 网络自愈）：AIRING_QUERY 只返回窗口内播出的集，
    // 窗口内零播出时无 media → next 污染无法纠正、过去时间假任务无法识别。
    // 这里在窗口调度应用之后，按 anilistId 全量抓取 AniList 权威 next + 已播
    // schedule：无条件重写 nextAiringEpisode、纠偏已播集 airingAt、删除未播
    // 假票、为追踪中条目回填缺失的已播集任务（第 6 轮误删事故修复：被污染
    // next 误删的已播任务 id 已在 seenAiringEvents、AIRING_QUERY 永不重建，
    // 权威 schedule 是唯一可信修复源）。顺序刻意为先 apply_airing_schedules
    // （处理已播窗口）后权威纠偏；删除规则以 next.episode 为界，本窗口刚建
    // 的已播任务 episode < next.episode 不会被误删。
    #[cfg(feature = "standard")]
    let authority_changed = anilist_authority_refresh(
        &context.state,
        &context.offline_bangumi,
        ANILIST_API,
        &context.client,
        &ids,
        &bangumi_cache_dir(context),
        now,
    )
    .await;
    // 最终逐集应用仅接受同一 AniList 条目、同一集号的可信精确时间；不允许
    // 分季或错误映射覆盖 Bangumi episode 身份。
    #[cfg(feature = "standard")]
    if apply_bangumi_episode_authority(context, now).await {
        context.save_state().map_err(|error| error.to_string())?;
        emit_state(app, context);
        context.webdav_wakeup.notify_one();
    }
    #[cfg(not(feature = "standard"))]
    let authority_changed = false;
    let mut state = context.state.lock().map_err(|_| "状态锁不可用")?;
    state["lastSyncAt"] = json!(now);
    state["tasks"]
        .as_array_mut()
        .unwrap()
        .sort_by(|left, right| {
            value_i64(right.get("airingAt")).cmp(&value_i64(left.get("airingAt")))
        });
    drop(state);
    context.save_state().map_err(|error| error.to_string())?;
    emit_state(app, context);
    if outcome.created > 0 || authority_changed {
        context.webdav_wakeup.notify_one();
    }
    #[cfg(desktop)]
    if outcome.aired > 0 {
        let (language, notifications_enabled) = {
            let state = context.state.lock().map_err(|_| "状态锁不可用")?;
            (
                value_string(state["settings"].get("uiLanguage")),
                value_bool(state["settings"].get("notifyWhenAired")),
            )
        };
        if notifications_enabled {
            let (title, body) = if language == "en-US" {
                (
                    "Anime updates are available".to_string(),
                    format!("{} followed episode(s) have aired.", outcome.aired),
                )
            } else {
                (
                    "追番已更新".to_string(),
                    format!("你追的番剧有 {} 集新内容已播出。", outcome.aired),
                )
            };
            show_desktop_notification(app, title, body);
        }
    }
    #[cfg_attr(not(feature = "standard"), expect(unused_mut))]
    let mut result = json!({"created": outcome.created, "syncedAt": now});
    #[cfg(feature = "standard")]
    if let Some(warning) = anilist_warning {
        result["warning"] = json!(warning);
    }
    Ok(result)
}

#[tauri::command]
fn get_cache_info(context: State<'_, AppContext>) -> Value {
    let bytes =
        directory_size(&context.cache_dir) + directory_size(&context.data_dir.join("webview-data"));
    json!({"bytes": bytes, "sessionBytes": 0, "legacyBytes": 0, "supported": true})
}

#[tauri::command]
fn clear_cache(app: AppHandle, context: State<'_, AppContext>) -> Value {
    #[cfg(desktop)]
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.clear_all_browsing_data();
    }
    #[cfg(not(desktop))]
    let _ = &app;
    if context.cache_dir.exists() {
        let _ = fs::remove_dir_all(&context.cache_dir);
        let _ = fs::create_dir_all(&context.cache_dir);
    }
    get_cache_info(context)
}

fn directory_size(path: &Path) -> u64 {
    fs::read_dir(path)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .map(|path| {
            if path.is_dir() {
                directory_size(&path)
            } else {
                fs::metadata(path).map(|meta| meta.len()).unwrap_or(0)
            }
        })
        .sum()
}

#[tauri::command]
fn open_external(app: AppHandle, url: String) -> Result<(), String> {
    if url.starts_with("https://") {
        app.opener()
            .open_url(url, None::<String>)
            .map_err(|error| error.to_string())
    } else {
        Err("只允许打开 HTTPS 地址".into())
    }
}

fn normalize_title_key(value: &str) -> String {
    value
        .chars()
        .flat_map(|character| character.to_lowercase())
        .filter(|character| character.is_alphanumeric())
        .collect()
}

fn anime_premiere_seconds(anime: &Value) -> i64 {
    let year = value_i64(anime["startDate"].get("year"));
    if year <= 0 {
        return 0;
    }
    let month = value_i64(anime["startDate"].get("month")).clamp(1, 12) as u32;
    let day = value_i64(anime["startDate"].get("day")).clamp(1, 28) as u32;
    chrono::NaiveDate::from_ymd_opt(year as i32, month, day)
        .and_then(|date| date.and_hms_opt(0, 0, 0))
        .map(|date| date.and_utc().timestamp())
        .unwrap_or(0)
}

fn cached_bangumi_title(state: &Value, anime: &Value, now: i64) -> Option<Value> {
    let anime_id = value_i64(anime.get("id")).to_string();
    let cached = state["bangumiTitles"].get(&anime_id)?;
    let status = value_string(cached.get("status"));
    if status != "matched" && value_i64(cached.get("resolverVersion")) != BANGUMI_RESOLVER_VERSION {
        return None;
    }
    let checked_at = value_i64(cached.get("checkedAt"));
    if checked_at <= 0 {
        return None;
    }
    let max_age = if status == "matched" {
        180 * 86_400
    } else if anime_premiere_seconds(anime) > now {
        86_400
    } else {
        7 * 86_400
    };
    (now - checked_at < max_age).then(|| cached.clone())
}

fn anime_start_date(anime: &Value) -> String {
    let year = value_i64(anime["startDate"].get("year"));
    let month = value_i64(anime["startDate"].get("month"));
    let day = value_i64(anime["startDate"].get("day"));
    if year > 0 && month > 0 && day > 0 {
        format!("{year:04}-{month:02}-{day:02}")
    } else {
        String::new()
    }
}

fn offline_format_matches(anime_format: &str, candidate_format: &str) -> bool {
    matches!(
        (anime_format, candidate_format),
        ("TV", "tv") | ("MOVIE", "movie") | ("OVA", "ova") | ("ONA", "web") | ("TV_SHORT", "web")
    )
}

// Look up a single subject entry directly by its Bangumi subject id in the
// offline map (v2 `bySubject`). Returns a copy of the stored entry, which
// carries `b/a/c/t/d/f/begin/broadcast/sites`. Consumed by the online-rebind
// path in Phase 2; exposed now so the mapping contract is unit tested.
#[allow(dead_code)]
pub(crate) fn offline_bangumi_subject(map: &Value, subject_id: i64) -> Option<Value> {
    map.get("bySubject")?.get(&subject_id.to_string()).cloned()
}

// Collect the bySubject candidates associated with an AniList id. The v2 map
// keys candidates by subject id and only records one representative per
// anilist id in `anilistIndex`, so recover every subject entry whose `a`
// (associated anilist id) matches to preserve the previous multi-candidate
// ranking behaviour. Falls back to the representative index entry when the
// scan finds no direct `a` match.
fn offline_anilist_candidates(map: &Value, anilist_id: i64) -> Vec<Value> {
    let Some(by_subject) = map.get("bySubject").and_then(Value::as_object) else {
        return Vec::new();
    };
    let mut candidates: Vec<Value> = by_subject
        .values()
        .filter(|entry| value_i64(entry.get("a")) == anilist_id)
        .cloned()
        .collect();
    if candidates.is_empty() {
        if let Some(subject_id) = map
            .get("anilistIndex")
            .and_then(|index| index.get(anilist_id.to_string()))
            .and_then(Value::as_i64)
        {
            if let Some(entry) = by_subject.get(&subject_id.to_string()) {
                candidates.push(entry.clone());
            }
        }
    }
    candidates
}

fn offline_bangumi_match(map: &Value, anime: &Value, checked_at: i64) -> Option<Value> {
    let candidates = offline_anilist_candidates(map, value_i64(anime.get("id")));
    if candidates.is_empty() {
        return None;
    }
    let anime_keys = ["native", "romaji", "english"]
        .iter()
        .filter_map(|key| anime["title"].get(*key).and_then(Value::as_str))
        .map(normalize_title_key)
        .filter(|key| !key.is_empty())
        .collect::<Vec<_>>();
    let start_date = anime_start_date(anime);
    let start_year = start_date.get(0..4).unwrap_or_default();
    let anime_format = value_string(anime.get("format"));
    let mut ranked = candidates
        .into_iter()
        .map(|candidate| {
            let candidate_key = normalize_title_key(&value_string(candidate.get("t")));
            let candidate_date = value_string(candidate.get("d"));
            let mut score = 0;
            if anime_keys.iter().any(|key| key == &candidate_key) {
                score += 100;
            } else if !candidate_key.is_empty()
                && anime_keys
                    .iter()
                    .any(|key| key.contains(&candidate_key) || candidate_key.contains(key))
            {
                score += 55;
            }
            if !start_date.is_empty() && candidate_date == start_date {
                score += 120;
            } else if !start_year.is_empty() && candidate_date.starts_with(start_year) {
                score += 30;
            }
            if offline_format_matches(&anime_format, &value_string(candidate.get("f"))) {
                score += 10;
            }
            (candidate, score)
        })
        .collect::<Vec<_>>();
    ranked.sort_by(|left, right| {
        right
            .1
            .cmp(&left.1)
            .then_with(|| value_i64(left.0.get("b")).cmp(&value_i64(right.0.get("b"))))
    });
    if ranked.len() > 1 && ranked[0].1 - ranked[1].1 < 8 {
        return None;
    }
    let (candidate, score) = &ranked[0];
    let chinese = value_string(candidate.get("c"));
    if chinese.is_empty() {
        return None;
    }
    let original_title = value_string(candidate.get("t"));
    let original_title = if original_title.is_empty() {
        value_string(anime["title"].get("native"))
    } else {
        original_title
    };
    Some(json!({
        "animeId": anime["id"],
        "status": "matched",
        "subjectId": candidate["b"],
        "name": original_title,
        "nameCn": chinese,
        "confidence": if ranked.len() == 1 { 100 } else { (*score).clamp(68, 100) },
        "source": "bangumi-data-anilist-id",
        "checkedAt": checked_at,
        "resolverVersion": BANGUMI_RESOLVER_VERSION
    }))
}

fn persist_bangumi_result(
    app: &AppHandle,
    context: &AppContext,
    anime_id: i64,
    result: &Value,
) -> Result<(), String> {
    let mut followed_changed = false;
    let mut state = context.state.lock().map_err(|_| "状态锁不可用")?;
    state["bangumiTitles"][anime_id.to_string()] = result.clone();
    if result["status"] == "matched" {
        let chinese = value_string(result.get("nameCn"));
        if !chinese.is_empty() {
            if let Some(followed) = state["following"].as_array_mut().and_then(|items| {
                items
                    .iter_mut()
                    .find(|item| value_i64(item.get("id")) == anime_id)
            }) {
                if value_string(followed.get("titleSource")) != "custom" {
                    followed["displayTitle"] = json!(chinese);
                    followed["titleSource"] = json!("bangumi");
                    followed["bangumiId"] = result["subjectId"].clone();
                    followed_changed = true;
                }
            }
            if let Some(tasks) = state["tasks"].as_array_mut() {
                for task in tasks
                    .iter_mut()
                    .filter(|task| value_i64(task.get("animeId")) == anime_id)
                {
                    task["animeTitle"] = json!(chinese);
                    task["syncUpdatedAt"] = json!(now_millis());
                }
            }
        }
    }
    if followed_changed {
        mark_following_changed(&mut state, anime_id);
    }
    drop(state);
    context.save_state().map_err(|error| error.to_string())?;
    emit_state(app, context);
    if followed_changed {
        context.webdav_wakeup.notify_one();
    }
    Ok(())
}

fn can_use_bangumi(original: bool, _base_url: &str) -> bool {
    !original
}

async fn bangumi_search(
    context: &AppContext,
    base_url: &str,
    keyword: &str,
) -> anyhow::Result<Vec<Value>> {
    let endpoint = format!(
        "{}/search/subjects?limit=12&offset=0",
        base_url.trim_end_matches('/')
    );
    let response = context
        .client
        .post(endpoint)
        .header(CONTENT_TYPE, "application/json")
        .header(ACCEPT, "application/json")
        .json(&json!({"keyword": keyword, "sort": "match", "filter": {"type": [2]}}))
        .send()
        .await?;
    if !response.status().is_success() {
        return Err(anyhow!("Bangumi 请求失败（HTTP {}）", response.status()));
    }
    Ok(response.json::<Value>().await?["data"]
        .as_array()
        .cloned()
        .unwrap_or_default())
}

#[tauri::command]
async fn resolve_bangumi_title(
    app: AppHandle,
    context: State<'_, AppContext>,
    anime: Value,
) -> Result<Value, String> {
    let base = {
        let state = context.state.lock().map_err(|_| "状态锁不可用")?;
        value_string(state["settings"].get("bangumiApiBaseUrl"))
    };
    if !can_use_bangumi(context.original, &base) {
        return Ok(
            json!({"animeId": anime["id"], "status": "unavailable", "checkedAt": now_seconds(), "resolverVersion": BANGUMI_RESOLVER_VERSION}),
        );
    }
    let checked = now_seconds();
    let cached = {
        let state = context.state.lock().map_err(|_| "状态锁不可用")?;
        cached_bangumi_title(&state, &anime, checked)
    };
    if let Some(cached) = cached {
        return Ok(cached);
    }
    if let Some(offline) = offline_bangumi_match(&context.offline_bangumi, &anime, checked) {
        persist_bangumi_result(&app, &context, value_i64(anime.get("id")), &offline)?;
        return Ok(offline);
    }
    if context.bangumi_unavailable_until.load(Ordering::Relaxed) > checked {
        return Ok(
            json!({"animeId": anime["id"], "status": "unavailable", "checkedAt": checked, "resolverVersion": BANGUMI_RESOLVER_VERSION}),
        );
    }
    let _lookup_guard = context.bangumi_lookup_lock.lock().await;
    let cached = {
        let state = context.state.lock().map_err(|_| "状态锁不可用")?;
        cached_bangumi_title(&state, &anime, checked)
    };
    if let Some(cached) = cached {
        return Ok(cached);
    }
    let title = anime.get("title").cloned().unwrap_or_default();
    let keywords: Vec<String> = ["native", "romaji", "english"]
        .iter()
        .filter_map(|key| title.get(*key).and_then(Value::as_str).map(str::to_string))
        .filter(|value| !value.is_empty())
        .collect();
    let mut candidates = BTreeMap::new();
    let mut received = false;
    for (index, keyword) in keywords.iter().take(3).enumerate() {
        if index > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(450)).await;
        }
        let endpoints = if base.trim().is_empty() {
            vec![OFFICIAL_BANGUMI_API]
        } else {
            vec![base.as_str(), OFFICIAL_BANGUMI_API]
        };
        for endpoint in endpoints {
            match bangumi_search(&context, endpoint, keyword).await {
                Ok(items) => {
                    received = true;
                    for item in items {
                        if let Some(id) = item.get("id").and_then(Value::as_i64) {
                            candidates.insert(id, item);
                        }
                    }
                    break;
                }
                Err(error) => {
                    warn!("Bangumi endpoint failed: {error}");
                }
            }
        }
    }
    let anime_keys: Vec<String> = keywords
        .iter()
        .map(|value| normalize_title_key(value))
        .collect();
    let mut ranked: Vec<(i64, Value, i64)> = candidates
        .into_iter()
        .filter_map(|(id, candidate)| {
            if value_string(candidate.get("name_cn")).trim().is_empty() {
                return None;
            }
            let names = [candidate.get("name"), candidate.get("name_cn")]
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>();
            let keys = names
                .iter()
                .map(|name| normalize_title_key(name))
                .collect::<Vec<_>>();
            let score = keys
                .iter()
                .map(|key| {
                    anime_keys
                        .iter()
                        .map(|anime_key| {
                            if key == anime_key {
                                100
                            } else if key.contains(anime_key) || anime_key.contains(key) {
                                72
                            } else {
                                0
                            }
                        })
                        .max()
                        .unwrap_or(0)
                })
                .max()
                .unwrap_or(0);
            Some((id, candidate, score))
        })
        .collect();
    ranked.sort_by(|left, right| right.2.cmp(&left.2));
    let result = if let Some((id, candidate, score)) = ranked.first() {
        if *score >= 68 {
            json!({"animeId": anime["id"], "status": "matched", "subjectId": id, "name": candidate["name"], "nameCn": candidate["name_cn"], "confidence": score, "source": "tauri-title", "checkedAt": checked, "resolverVersion": BANGUMI_RESOLVER_VERSION})
        } else {
            json!({"animeId": anime["id"], "status": "unmatched", "confidence": score, "checkedAt": checked, "resolverVersion": BANGUMI_RESOLVER_VERSION})
        }
    } else {
        json!({"animeId": anime["id"], "status": if received { "unmatched" } else { "unavailable" }, "checkedAt": checked, "resolverVersion": BANGUMI_RESOLVER_VERSION})
    };
    if !received {
        context
            .bangumi_unavailable_until
            .store(checked + 10 * 60, Ordering::Relaxed);
    } else {
        context
            .bangumi_unavailable_until
            .store(0, Ordering::Relaxed);
        persist_bangumi_result(&app, &context, value_i64(anime.get("id")), &result)?;
    }
    tokio::time::sleep(std::time::Duration::from_millis(450)).await;
    Ok(result)
}

#[tauri::command]
async fn test_bangumi_connection(
    context: State<'_, AppContext>,
    base_url: String,
) -> Result<Value, String> {
    if context.original {
        return Ok(
            json!({"ok": false, "message": "AniLog Original 不使用 Bangumi API", "baseUrl": ""}),
        );
    }
    let normalized = normalize_url(&base_url, Some("/v0")).map_err(|error| error.to_string())?;
    let endpoint = if normalized.is_empty() {
        OFFICIAL_BANGUMI_API.to_string()
    } else {
        normalized
    };
    match bangumi_search(&context, &endpoint, "CLANNAD").await {
        Ok(_) => Ok(json!({"ok": true, "message": "Bangumi 连接成功", "baseUrl": endpoint})),
        Err(error) => Ok(json!({"ok": false, "message": error.to_string(), "baseUrl": endpoint})),
    }
}

// ---------------------------------------------------------------------------
// Phase 1 Bangumi 命令（Token + 连接 + 只读）。命令契约（前端按此对接）：
// - bangumi_auth_status() -> { supported, hasToken, apiBaseUrl }（永不回传 Token 本体）
// - bangumi_save_token({ token }) -> { ok, message }
// - bangumi_disconnect() -> { ok, message }
// - bangumi_test_connection({ baseUrl? }) -> { ok, message, username, nickname }
// - bangumi_get_user_profile() -> 用户 camelCase JSON | null
// - bangumi_get_user_collections({ offset?, limit? }) -> { total, items }
// - bangumi_set_api_base_url({ baseUrl }) -> public_state（决策 11 双向同步的另一入口）
// Original edition 下全部编译且运行即拒绝（固定文案 "Original 版不支持 Bangumi"）。
// 命令逻辑抽在 bangumi_commands 模块的内部函数中，测试以 MemoryTokenStore +
// mock server 直接调用，不依赖真实 keyring。
// ---------------------------------------------------------------------------

#[cfg(feature = "standard")]
mod bangumi_commands {
    use super::{AppContext, value_string};
    use crate::bangumi::{
        BangumiApiError, BangumiSubjectImages, BangumiTokenStore, HttpBangumiClient,
        SUBJECT_TYPE_ANIME, TokenStoreError, bangumi_collection_json, bangumi_profile_json,
    };
    use serde_json::{Value, json};
    use std::fs;
    use std::path::Path;
    use std::sync::Mutex;

    /// 当前平台是否接入了安全凭据存储（Windows keyring / Android 桥）。
    pub(super) fn token_store_supported() -> bool {
        cfg!(any(target_os = "windows", target_os = "android"))
    }

    /// TokenStore 错误 → 用户文案。Android 桥失败与不支持平台使用固定文案
    /// （前端 i18n 按此映射）；Windows Credential Manager 的平台错误不含
    /// Token，可透传排障。
    pub(super) fn store_error_message(error: &TokenStoreError) -> String {
        if cfg!(target_os = "android") {
            "Bangumi 安全存储不可用".into()
        } else if !token_store_supported() {
            "当前平台不支持 Bangumi Token 存储".into()
        } else {
            format!("Bangumi Token 存储失败：{error}")
        }
    }

    /// BangumiApiError → 用户文案：401 与网络层失败使用固定文案，
    /// 其余错误直接使用 Display（不含任何 Token 材料，由 bangumi.rs 测试锁定）。
    pub(super) fn request_error_message(error: BangumiApiError) -> String {
        match error {
            BangumiApiError::Unauthorized { .. } => "Bangumi 授权失败，Token 可能已失效".into(),
            BangumiApiError::Network(_) | BangumiApiError::Timeout => {
                "无法连接 Bangumi 服务".into()
            }
            other => other.to_string(),
        }
    }

    fn profile_error_payload(message: &str) -> Value {
        json!({"ok": false, "message": message, "username": Value::Null, "nickname": Value::Null})
    }

    fn collections_error_payload(message: &str) -> Value {
        json!({"ok": false, "message": message, "total": 0, "items": []})
    }

    /// `bangumi_auth_status`：supported / hasToken / apiBaseUrl。
    pub(super) fn auth_status(state: &Value, tokens: &dyn BangumiTokenStore) -> Value {
        let supported = token_store_supported();
        let has_token = supported
            && tokens
                .load()
                .ok()
                .flatten()
                .is_some_and(|token| !token.trim().is_empty());
        // apiBaseUrl 展示值与 bangumi_base_urls 的读取顺序一致（决策 11）。
        let from_block = value_string(
            state
                .get("bangumi")
                .and_then(|block| block.get("apiBaseUrl")),
        );
        let api_base_url = if from_block.trim().is_empty() {
            value_string(state["settings"].get("bangumiApiBaseUrl"))
        } else {
            from_block
        };
        json!({"supported": supported, "hasToken": has_token, "apiBaseUrl": api_base_url})
    }

    /// `bangumi_save_token`：trim 后为空 → "Token 不能为空"；成功不回显 Token。
    pub(super) fn save_token(tokens: &dyn BangumiTokenStore, token: &str) -> Value {
        if !token_store_supported() {
            return json!({"ok": false, "message": "当前平台不支持 Bangumi Token 存储"});
        }
        let trimmed = token.trim();
        if trimmed.is_empty() {
            return json!({"ok": false, "message": "Token 不能为空"});
        }
        match tokens.store(trimmed) {
            Ok(()) => json!({"ok": true, "message": "Bangumi Token 已保存"}),
            Err(error) => json!({"ok": false, "message": store_error_message(&error)}),
        }
    }

    /// `bangumi_disconnect`：清除 Token 并清空 username 缓存。
    pub(super) fn disconnect(
        tokens: &dyn BangumiTokenStore,
        username_cache: &Mutex<Option<String>>,
    ) -> Value {
        if !token_store_supported() {
            return json!({"ok": false, "message": "当前平台不支持 Bangumi Token 存储"});
        }
        if let Ok(mut cache) = username_cache.lock() {
            *cache = None;
        }
        match tokens.clear() {
            Ok(()) => json!({"ok": true, "message": "已断开 Bangumi 连接"}),
            Err(error) => json!({"ok": false, "message": store_error_message(&error)}),
        }
    }

    /// 读取 Token；错误时返回固定文案载荷。
    fn load_token(tokens: &dyn BangumiTokenStore) -> Result<String, Value> {
        match tokens.load() {
            Ok(Some(token)) if !token.trim().is_empty() => Ok(token),
            Ok(_) => Err(json!({"ok": false, "message": "尚未保存 Bangumi Token"})),
            Err(error) => Err(json!({"ok": false, "message": store_error_message(&error)})),
        }
    }

    /// `bangumi_test_connection`：有 Token 时 `GET /v0/me`。
    pub(super) async fn test_connection(
        tokens: &dyn BangumiTokenStore,
        client: &HttpBangumiClient,
        username_cache: &Mutex<Option<String>>,
    ) -> Value {
        let token = match load_token(tokens) {
            Ok(token) => token,
            Err(payload) => {
                return profile_error_payload(payload["message"].as_str().unwrap_or_default());
            }
        };
        match client.test_connection(&token).await {
            Ok(profile) => {
                if let Ok(mut cache) = username_cache.lock() {
                    if !profile.username.trim().is_empty() {
                        *cache = Some(profile.username.clone());
                    }
                }
                json!({
                    "ok": true,
                    "message": "Bangumi 连接成功",
                    "username": profile.username,
                    "nickname": profile.nickname,
                })
            }
            Err(error) => profile_error_payload(&request_error_message(error)),
        }
    }

    /// `bangumi_get_user_profile`：无 Token / 失败 → `Value::Null`（Option 语义）。
    pub(super) async fn user_profile(
        tokens: &dyn BangumiTokenStore,
        client: &HttpBangumiClient,
    ) -> Value {
        let token = match load_token(tokens) {
            Ok(token) => token,
            Err(_) => return Value::Null,
        };
        match client.get_user_profile(&token).await {
            Ok(profile) => bangumi_profile_json(&profile),
            Err(_) => Value::Null,
        }
    }

    /// /v0/me → username 的进程内缓存读取（bangumi_get_user_collections 与
    /// Phase 3 拉取引擎共用）。
    pub(super) async fn ensure_username(
        tokens: &dyn BangumiTokenStore,
        client: &HttpBangumiClient,
        username_cache: &Mutex<Option<String>>,
    ) -> Result<String, String> {
        if let Ok(cache) = username_cache.lock() {
            if let Some(username) = cache.as_ref().filter(|name| !name.trim().is_empty()) {
                return Ok(username.clone());
            }
        }
        let token = load_token(tokens)
            .map_err(|payload| payload["message"].as_str().unwrap_or_default().to_string())?;
        let profile = client
            .get_user_profile(&token)
            .await
            .map_err(request_error_message)?;
        let username = profile.username.trim().to_string();
        if username.is_empty() {
            return Err("Bangumi 授权失败，Token 可能已失效".into());
        }
        if let Ok(mut cache) = username_cache.lock() {
            *cache = Some(username.clone());
        }
        Ok(username)
    }

    /// `bangumi_get_user_collections`：固定 subject_type=2（动画），
    /// 读端点 `/v0/users/{username}/collections`（username 先经 /v0/me 取得）。
    pub(super) async fn user_collections(
        tokens: &dyn BangumiTokenStore,
        client: &HttpBangumiClient,
        username_cache: &Mutex<Option<String>>,
        offset: Option<u32>,
        limit: Option<u32>,
    ) -> Value {
        if let Err(payload) = load_token(tokens) {
            return collections_error_payload(payload["message"].as_str().unwrap_or_default());
        }
        let username = match ensure_username(tokens, client, username_cache).await {
            Ok(username) => username,
            Err(message) => return collections_error_payload(&message),
        };
        let token = match load_token(tokens) {
            Ok(token) => token,
            Err(payload) => {
                return collections_error_payload(payload["message"].as_str().unwrap_or_default());
            }
        };
        match client
            .get_user_collections(
                &token,
                &username,
                SUBJECT_TYPE_ANIME,
                limit.unwrap_or(30),
                offset.unwrap_or(0),
            )
            .await
        {
            Ok(page) => json!({
                "total": page.total,
                "items": page
                    .data
                    .iter()
                    .map(bangumi_collection_json)
                    .collect::<Vec<_>>(),
            }),
            Err(error) => collections_error_payload(&request_error_message(error)),
        }
    }

    /// 条目 extras 缓存 TTL（schema §7：24h SWR）。
    pub(super) const SUBJECT_EXTRAS_TTL_SECONDS: i64 = 24 * 3_600;

    /// 读取 subject extras 缓存。`max_age_seconds` 传 TTL 内为新鲜读取，
    /// 传 `i64::MAX` 为 stale 兜底读取（任何年龄都接受）。
    fn read_subject_extras_cache(
        cache_path: &Path,
        max_age_seconds: i64,
        now: i64,
    ) -> Option<Value> {
        let body = fs::read_to_string(cache_path).ok()?;
        let extras: Value = serde_json::from_str(&body).ok()?;
        let fetched_at = extras.get("fetchedAt").and_then(Value::as_i64)?;
        if fetched_at <= 0 {
            return None;
        }
        (now - fetched_at < max_age_seconds).then_some(extras)
    }

    /// infobox value（字符串或 `{v}` 数组）→ 展示字符串（数组用「、」连接）。
    fn infobox_value_string(value: &Value) -> String {
        match value {
            Value::String(text) => text.clone(),
            Value::Array(items) => items
                .iter()
                .filter_map(|item| {
                    item.as_str()
                        .map(str::to_string)
                        .or_else(|| item.get("v").and_then(Value::as_str).map(str::to_string))
                })
                .filter(|text| !text.is_empty())
                .collect::<Vec<_>>()
                .join("、"),
            Value::Null => String::new(),
            other => other.to_string(),
        }
    }

    /// 角色/关联条目图片（large || common || medium || small；全缺 → null）。
    /// 问题 4（验收第 2 轮，角色图下半身）：Bangumi 角色 `medium`/`small` 是
    /// 全身立绘的中心方形裁剪缩略图，`large` 是未裁剪全身图——链首保持
    /// `large`；角色 images 无 `common`，缺 large 时按 medium → small 回落，
    /// 不再可能落到裁剪图之外的其他键。前端配合 `object-position: top` 展示。
    pub(super) fn subject_image_url(images: Option<&BangumiSubjectImages>) -> Value {
        images
            .and_then(|images| {
                images
                    .large
                    .clone()
                    .or_else(|| images.common.clone())
                    .or_else(|| images.medium.clone())
                    .or_else(|| images.small.clone())
                    .or_else(|| images.grid.clone())
            })
            .filter(|url| !url.is_empty())
            .map(Value::String)
            .unwrap_or(Value::Null)
    }

    /// 三次请求（详情/角色/关联）组装 extras（camelCase，前端契约冻结形状）。
    /// 三请求经 HttpBangumiClient 的 Semaphore(2) 串行；任一失败 → 整体失败
    /// （避免把半截数据缓存 24h）。
    async fn fetch_subject_extras(
        client: &HttpBangumiClient,
        subject_id: i64,
        now: i64,
    ) -> Result<Value, BangumiApiError> {
        let detail = client.get_subject_detail(subject_id).await?;
        let characters = client.get_subject_characters(subject_id).await?;
        let related = client.get_subject_related(subject_id).await?;
        Ok(json!({
            "fetchedAt": now,
            "rating": detail.rating.map(|rating| json!({
                "score": rating.score, "total": rating.total, "rank": rating.rank
            })),
            "tags": detail
                .tags
                .iter()
                .map(|tag| json!({"name": tag.name, "count": tag.count}))
                .collect::<Vec<_>>(),
            "characters": characters
                .iter()
                .map(|character| json!({
                    "id": character.id, "name": character.name, "nameCn": character.name_cn,
                    "imageUrl": subject_image_url(character.images.as_ref()),
                    "relation": character.relation
                }))
                .collect::<Vec<_>>(),
            "related": related
                .iter()
                .map(|related| json!({
                    "id": related.id, "name": related.name, "nameCn": related.name_cn,
                    "relation": related.relation,
                    "imageUrl": subject_image_url(related.images.as_ref())
                }))
                .collect::<Vec<_>>(),
            "staff": detail
                .infobox
                .iter()
                .take(8)
                .map(|item| json!({"key": item.key, "value": infobox_value_string(&item.value)}))
                .collect::<Vec<_>>(),
            "siteUrl": format!("https://bgm.tv/subject/{subject_id}")
        }))
    }

    /// `bangumi_get_subject_extras` 核心（测试经 MockBangumiServer 直调）。
    ///
    /// SWR 取舍（schema §7 允许简化）：缓存 24h 内直接返回；过期时**阻塞刷新**
    /// （本批不实现后台刷新），刷新失败回落旧缓存（stale）；连旧缓存都没有 →
    /// `Value::Null`（前端按 null 隐藏区块）。
    pub(super) async fn subject_extras(
        client: &HttpBangumiClient,
        cache_path: &Path,
        subject_id: i64,
        now: i64,
    ) -> Value {
        if let Some(extras) = read_subject_extras_cache(cache_path, SUBJECT_EXTRAS_TTL_SECONDS, now)
        {
            return extras;
        }
        match fetch_subject_extras(client, subject_id, now).await {
            Ok(extras) => {
                if let Some(directory) = cache_path.parent() {
                    let _ = fs::create_dir_all(directory);
                }
                if let Ok(body) = serde_json::to_vec(&extras) {
                    let _ = fs::write(cache_path, body);
                }
                extras
            }
            Err(_) => read_subject_extras_cache(cache_path, i64::MAX, now).unwrap_or(Value::Null),
        }
    }

    /// `bangumi_set_api_base_url`（决策 11 的反向入口）：规范化后同时写入
    /// settings.bangumiApiBaseUrl 与顶层 bangumi.apiBaseUrl。
    pub(super) fn set_api_base_url(context: &AppContext, base_url: &str) -> Result<(), String> {
        let normalized = if base_url.trim().is_empty() {
            String::new()
        } else {
            super::normalize_url(base_url, Some("/v0")).map_err(|error| error.to_string())?
        };
        {
            let mut state = context.state.lock().map_err(|_| "状态锁不可用")?;
            state["settings"]["bangumiApiBaseUrl"] = json!(normalized);
            if let Some(block) = state.get_mut("bangumi").and_then(Value::as_object_mut) {
                block.insert("apiBaseUrl".into(), json!(normalized));
            }
        }
        context.save_state().map_err(|error| error.to_string())?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Phase 3（standard，LOCAL 方案 §7）：Bangumi 收藏/评分/进度拉取合并 + hash
// 冲突解决 + 写回引擎。
//
// 冲突解决禁用 updated_at LWW（官方注明 updated_at 不可靠，schema §3.2）：
// 每条 following 记录维护 lastPulledPayloadHash / lastPushedPayloadHash
//（对 collection 关心字段 {type,rate,ep_status,comment,tags,private} 规范化
// JSON 的 sha256，见 bangumi::collection_payload_hash_parts）与 lastChangedBy。
// 拉取时判定：
// - H_remote == lastPulledPayloadHash → 远端无变化，跳过；
// - H_local == H_remote → 方向不明，按 conflictPolicy（latest=不动+记冲突 /
//   local-first=推远端 / bangumi-first=改本地）；若 lastPushedPayloadHash ==
//   H_local（本地自上次推送无变更）则视为已收敛，仅更新拉取基线；
// - 否则 → 外部变化 → 合并（写 lastPulledPayloadHash=H_remote、
//   lastChangedBy="bangumi"）。
// 防循环：lastChangedBy=="bangumi" 的记录不自动推送；远端驱动的取消追番即时
// 清出 pendingBangumiUnfollows；写回仅处理 lastChangedBy 为 local/webdav 且
// H_local != lastPushedPayloadHash 的记录。
// ---------------------------------------------------------------------------
#[cfg(feature = "standard")]
mod bangumi_sync {
    use super::{
        bangumi_following_entry, bangumi_platform_to_format, mark_following_changed, now_millis,
        now_seconds, offline_bangumi_subject, remove_following, value_i64, value_string,
    };
    use crate::bangumi::{
        self, BangumiCollection, BangumiSubject, BangumiSyncReport, BangumiSyncSettings,
        BangumiTokenStore, HttpBangumiClient,
    };
    use serde_json::{Value, json};
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    /// 收藏列表分页上限（limit=50，≤20 页）。
    const COLLECTION_MAX_PAGES: usize = 20;
    /// 收藏列表分页大小。
    const COLLECTION_PAGE_LIMIT: u32 = 50;
    /// 集数列表缓存 TTL（schema §7：12-24h，取 24h）。
    const EPISODES_CACHE_TTL_SECONDS: i64 = 24 * 3_600;
    /// 集数列表单页上限（官方上限 200；≤1 页足够覆盖观看任务场景）。
    const EPISODES_PAGE_LIMIT: u32 = 200;

    /// 读取顶层 `bangumi` 设置块（缺失/损坏时回落默认值）。
    pub(super) fn sync_settings(state: &Value) -> BangumiSyncSettings {
        state
            .get("bangumi")
            .cloned()
            .and_then(|value| serde_json::from_value(value).ok())
            .unwrap_or_default()
    }

    /// 本地评分（0-10；null/负数/越界 → None）。
    fn local_rating(entry: &Value) -> Option<u8> {
        entry
            .get("rating")
            .and_then(Value::as_i64)
            .filter(|rate| (0..=10).contains(rate))
            .and_then(|rate| u8::try_from(rate).ok())
    }

    /// 本地观看进度（>=0；null → None）。
    fn local_watched_episode(entry: &Value) -> Option<u32> {
        entry
            .get("watchedEpisode")
            .and_then(Value::as_i64)
            .filter(|episode| *episode >= 0)
            .and_then(|episode| u32::try_from(episode).ok())
    }

    /// 写回 type（状态驱动追踪，任务 4）：由条目 `bangumiStatus` 映射
    /// （wish=1 / done=2 / doing=3 / on_hold=4），空/null 视为 doing=3
    /// （anilist 来源迁移条目维持旧行为）。
    fn local_collection_type(entry: &Value) -> u32 {
        bangumi::collection_status_value(&value_string(entry.get("bangumiStatus")))
    }

    /// 本地收藏写回 payload（type 由 [`local_collection_type`] 映射；rate 仅在
    /// 有评分时携带——ModifyPayload 全可选，不传会被忽略）。
    pub(super) fn local_collection_payload(entry: &Value) -> Value {
        let mut payload = serde_json::Map::new();
        payload.insert("type".into(), json!(local_collection_type(entry)));
        if let Some(rate) = local_rating(entry) {
            payload.insert("rate".into(), json!(rate));
        }
        Value::Object(payload)
    }

    /// 本地记录的收藏 payload 哈希（H_local；与远端同一规范化函数，保证可比）。
    pub(super) fn local_collection_hash(entry: &Value) -> String {
        bangumi::collection_payload_hash_parts(
            local_collection_type(entry),
            local_rating(entry),
            local_watched_episode(entry),
            None,
            &[],
            None,
        )
    }

    fn find_entry_index(state: &Value, subject_id: i64) -> Option<usize> {
        state["following"].as_array().and_then(|items| {
            items.iter().position(|item| {
                value_i64(item.get("id")) == subject_id
                    || value_i64(item.get("bangumiId")) == subject_id
            })
        })
    }

    /// 进度推送候选任务（验收第 4 轮问题 2 抽取）：completed、非拉取来源
    /// （lastChangedBy != bangumi，防循环）、未推送过（无 lastPushedToBangumiAt）。
    fn push_candidate_task(task: &Value) -> bool {
        value_string(task.get("status")) == "completed"
            && task.get("lastChangedBy").and_then(Value::as_str) != Some("bangumi")
            && !task
                .get("lastPushedToBangumiAt")
                .is_some_and(Value::is_number)
    }

    /// 任务是否已绑定有效 Bangumi episodeId。
    fn task_has_episode_id(task: &Value) -> bool {
        task.get("episodeId")
            .and_then(Value::as_i64)
            .is_some_and(|id| id > 0)
    }

    /// 任务 2：拉取合并引擎。前置：Token 且 `bangumi.syncEnabled` 且
    /// `pullCollections`，否则返回零值报告（skipped，调用方按语义给 message）。
    pub(super) async fn run_bangumi_collection_sync(
        http: &HttpBangumiClient,
        tokens: &dyn BangumiTokenStore,
        username_cache: &Mutex<Option<String>>,
        state: &Mutex<Value>,
        offline_map: &Value,
    ) -> BangumiSyncReport {
        let mut report = BangumiSyncReport::default();
        let settings = {
            let guard = state.lock().expect("state lock");
            sync_settings(&guard)
        };
        if !settings.sync_enabled || !settings.pull_collections {
            return report; // skipped：同步未启用
        }
        let token = match tokens.load() {
            Ok(Some(token)) if !token.trim().is_empty() => token,
            _ => return report, // skipped：无 Token
        };
        let username =
            match super::bangumi_commands::ensure_username(tokens, http, username_cache).await {
                Ok(username) => username,
                Err(message) => {
                    report.errors.push(message);
                    return report;
                }
            };
        // 分页拉全（GET /v0/users/{username}/collections?subject_type=2）。
        let mut collections: Vec<BangumiCollection> = Vec::new();
        let mut offset = 0u32;
        for _ in 0..COLLECTION_MAX_PAGES {
            let page = match http
                .get_user_collections(
                    &token,
                    &username,
                    bangumi::SUBJECT_TYPE_ANIME,
                    COLLECTION_PAGE_LIMIT,
                    offset,
                )
                .await
            {
                Ok(page) => page,
                Err(error) => {
                    report
                        .errors
                        .push(super::bangumi_commands::request_error_message(error));
                    return report;
                }
            };
            let count = page.data.len() as u32;
            collections.extend(page.data);
            offset += count;
            if count == 0
                || (count as usize) < COLLECTION_PAGE_LIMIT as usize
                || (page.total > 0 && offset >= page.total)
            {
                break;
            }
        }
        report.pulled = collections.len() as u32;

        // 第 1 步（无网络）：快照分类 → 创建 / 合并 / 冲突推送 / 收敛。
        struct CreatePlan {
            collection: BangumiCollection,
            h_remote: String,
            subject: Option<BangumiSubject>,
        }
        struct ConflictPushPlan {
            subject_id: i64,
            payload: Value,
            create: bool,
            h_local: String,
        }
        let (following_snapshot, deleted_snapshot) = {
            let guard = state.lock().expect("state lock");
            (
                guard["following"].as_array().cloned().unwrap_or_default(),
                guard["syncMetadata"]["followingDeletedAt"].clone(),
            )
        };
        let snapshot_tombstone = |subject_id: i64| -> bool {
            value_i64(deleted_snapshot.get(&subject_id.to_string())) > 0
        };
        let mut creates: Vec<CreatePlan> = Vec::new();
        let mut merges: Vec<(BangumiCollection, String)> = Vec::new();
        let mut converged: Vec<(i64, String)> = Vec::new();
        let mut conflict_pushes: Vec<ConflictPushPlan> = Vec::new();
        for collection in &collections {
            let subject_id = collection.subject_id;
            if subject_id <= 0 {
                continue;
            }
            let h_remote = bangumi::collection_payload_hash(collection);
            let entry = following_snapshot.iter().find(|item| {
                value_i64(item.get("id")) == subject_id
                    || value_i64(item.get("bangumiId")) == subject_id
            });
            let Some(entry) = entry else {
                match collection.collection_type {
                    // 本地无条目 + doing + 无墓碑 → 创建 following。
                    3 if !snapshot_tombstone(subject_id) => {
                        creates.push(CreatePlan {
                            collection: collection.clone(),
                            h_remote: h_remote.clone(),
                            subject: None,
                        });
                    }
                    // wish/on_hold 仅建议。doing 即使存在旧墓碑也必须恢复：
                    // 墓碑代表过去的本地取消，不应覆盖当前 Bangumi 的在看状态。
                    1 | 4 => report.suggestions.push(bangumi::BangumiSyncSuggestion {
                        subject_id,
                        name_cn: collection.subject.as_ref().and_then(|s| s.name_cn.clone()),
                        collection_type: collection.collection_type,
                    }),
                    3 => creates.push(CreatePlan {
                        collection: collection.clone(),
                        h_remote: h_remote.clone(),
                        subject: None,
                    }),
                    // dropped（弃番）/ done（看过）且本地无条目：跳过（看过≠追番）。
                    _ => {}
                }
                continue;
            };
            // 远端无变化（payload hash 相同）→ 跳过。
            if entry.get("lastPulledPayloadHash").and_then(Value::as_str) == Some(h_remote.as_str())
            {
                continue;
            }
            let h_local = local_collection_hash(entry);
            if h_local == h_remote {
                // H_local==H_remote：方向不明。
                if entry.get("lastPushedPayloadHash").and_then(Value::as_str)
                    == Some(h_local.as_str())
                {
                    // 本地自上次推送无变更 → 已收敛，仅更新拉取基线。
                    converged.push((subject_id, h_remote.clone()));
                } else {
                    match settings.conflict_policy {
                        // latest：不动本地，仅记录冲突。
                        bangumi::ConflictPolicy::Latest => report.conflicts += 1,
                        // local-first：推远端。
                        bangumi::ConflictPolicy::LocalFirst => {
                            conflict_pushes.push(ConflictPushPlan {
                                subject_id,
                                payload: local_collection_payload(entry),
                                // H_local==H_remote 意味着远端已有等值记录 → PATCH。
                                create: false,
                                h_local: h_local.clone(),
                            });
                        }
                        // bangumi-first：改本地（走外部变化合并）。
                        bangumi::ConflictPolicy::BangumiFirst => {
                            merges.push((collection.clone(), h_remote.clone()));
                        }
                    }
                }
                continue;
            }
            // 外部变化 → 合并。
            merges.push((collection.clone(), h_remote.clone()));
        }

        // 第 2 步（网络）：为缺少内嵌 SlimSubject 的创建计划补拉条目详情。
        for plan in creates.iter_mut() {
            if plan.collection.subject.is_none() {
                match http.get_subject_detail(plan.collection.subject_id).await {
                    Ok(detail) => plan.subject = Some(detail),
                    Err(error) => {
                        report
                            .errors
                            .push(super::bangumi_commands::request_error_message(error));
                    }
                }
            }
        }

        // 第 3 步（锁内）：应用创建 / 合并 / 收敛（纯本地写，无网络）。
        {
            let mut guard = state.lock().expect("state lock");
            for plan in &creates {
                if plan.collection.subject.is_none() && plan.subject.is_none() {
                    continue; // 详情补拉失败且无内嵌概要：无法构造条目
                }
                if find_entry_index(&guard, plan.collection.subject_id).is_some() {
                    continue; // 复核：应用前条目已被创建
                }
                let anime =
                    collection_subject_anime(&plan.collection, plan.subject.as_ref(), offline_map);
                let mut entry = bangumi_following_entry(
                    &anime,
                    &value_string(guard["settings"].get("titlePreference")),
                    &value_string(guard["settings"].get("uiLanguage")),
                );
                entry["bangumiStatus"] = json!("doing");
                entry["rating"] = match plan.collection.rate {
                    Some(rate) => json!(rate),
                    None => Value::Null,
                };
                entry["watchedEpisode"] = plan
                    .collection
                    .ep_status
                    .map(|value| json!(i64::from(value)))
                    .unwrap_or(Value::Null);
                entry["lastPulledPayloadHash"] = json!(plan.h_remote);
                entry["lastPushedPayloadHash"] = Value::Null;
                entry["lastChangedBy"] = json!("bangumi");
                entry["lastPulledFromBangumiAt"] = json!(now_seconds());
                if plan.collection.collection_type == bangumi::SubjectCollectionType::Doing.as_u32() {
                    if let Some(tombstones) = guard["syncMetadata"]["followingDeletedAt"].as_object_mut() {
                        tombstones.remove(&plan.collection.subject_id.to_string());
                    }
                }
                guard["following"]
                    .as_array_mut()
                    .expect("following array")
                    .push(entry);
                mark_following_changed(&mut guard, plan.collection.subject_id);
                report.followed += 1;
            }
            for (collection, h_remote) in &merges {
                if let Some(index) = find_entry_index(&guard, collection.subject_id) {
                    apply_remote_merge(&mut guard, index, collection, h_remote, &mut report);
                }
            }
            for (subject_id, hash) in &converged {
                if let Some(index) = find_entry_index(&guard, *subject_id) {
                    guard["following"][index]["lastPulledPayloadHash"] = json!(hash);
                }
            }
        }

        // 第 4 步（网络）：conflictPolicy=local-first 的方向不明冲突推远端。
        let mut pushed_hashes: Vec<(i64, String)> = Vec::new();
        for push in conflict_pushes {
            match http
                .update_collection(&token, push.subject_id, &push.payload, push.create)
                .await
            {
                Ok(()) => {
                    report.pushed += 1;
                    pushed_hashes.push((push.subject_id, push.h_local));
                }
                Err(error) => report
                    .errors
                    .push(super::bangumi_commands::request_error_message(error)),
            }
        }

        // 第 5 步（锁内）：记录推送基线。
        if !pushed_hashes.is_empty() {
            let mut guard = state.lock().expect("state lock");
            for (subject_id, hash) in pushed_hashes {
                if let Some(index) = find_entry_index(&guard, subject_id) {
                    guard["following"][index]["lastPushedPayloadHash"] = json!(hash);
                    guard["following"][index]["lastPushedToBangumiAt"] = json!(now_seconds());
                }
            }
        }
        report
    }

    /// 远端收藏合并到本地条目（外部变化 / bangumi-first）。
    /// 枚举映射（schema §5）：3=Doing→追番中（已追番则同步状态字段）；
    /// 2=Done→补完成本地 pending 中 episode<=ep_status 的任务（不新建、不删）；
    /// 5=Dropped→取消追番（复用 remove_following：只删未完成任务，已完成任务
    /// 永保留；远端驱动 → 即时清出取消队列防写回循环）；1=Wish/4=OnHold→本地
    /// 追番状态不动，仅同步 bangumiStatus 字段。rating：rate>=0 → 本地 rating=rate。
    fn apply_remote_merge(
        state: &mut Value,
        entry_index: usize,
        collection: &BangumiCollection,
        h_remote: &str,
        report: &mut BangumiSyncReport,
    ) {
        let entry_id = value_i64(state["following"][entry_index].get("id"));
        if collection.collection_type == bangumi::SubjectCollectionType::Dropped.as_u32() {
            // A locally-created/re-followed record must not be deleted by a stale
            // Bangumi `dropped` snapshot.  Keep the local intent and let the write
            // phase converge the remote collection back to Doing.  Without this
            // guard a just-added subject disappears seconds after it is matched.
            let local_intent = {
                let intent_at = value_i64(
                    state["following"][entry_index].get("localFollowIntentAt"),
                );
                let last_remote_at = value_i64(
                    state["following"][entry_index].get("lastPulledFromBangumiAt"),
                );
                intent_at > 0
                    && (intent_at / 1_000 > last_remote_at
                        || intent_at > value_i64(
                            state["syncMetadata"]["followingDeletedAt"]
                                .get(&entry_id.to_string()),
                        ))
            };
            if local_intent {
                return;
            }
            remove_following(state, entry_id);
            super::remove_pending_bangumi_unfollow(state, collection.subject_id);
            report.unfollowed += 1;
            return; // 条目已删除，剩余 hash 记在墓碑侧
        }
        if collection.collection_type == bangumi::SubjectCollectionType::Done.as_u32() {
            let ep_status = i64::from(collection.ep_status.unwrap_or(0));
            let now = now_seconds();
            let mut completed = 0u32;
            if let Some(tasks) = state["tasks"].as_array_mut() {
                for task in tasks.iter_mut() {
                    if value_i64(task.get("animeId")) == entry_id
                        && value_string(task.get("status")) == "pending"
                        && value_i64(task.get("episode")) > 0
                        && value_i64(task.get("episode")) <= ep_status
                    {
                        task["status"] = json!("completed");
                        task["completedAt"] = json!(now);
                        task["syncUpdatedAt"] = json!(now_millis());
                        task["lastChangedBy"] = json!("bangumi");
                        completed += 1;
                    }
                }
            }
            report.completed_tasks += completed;
        }
        // wish/on_hold：仅建议（本地追番状态不动），与无条目分支同语义。
        // fallback 标题先于 entry 可变借用读取。
        let suggestion_title = if collection.collection_type
            == bangumi::SubjectCollectionType::Wish.as_u32()
            || collection.collection_type == bangumi::SubjectCollectionType::OnHold.as_u32()
        {
            Some(
                collection
                    .subject
                    .as_ref()
                    .and_then(|subject| subject.name_cn.clone())
                    .or_else(|| {
                        Some(value_string(
                            state["following"][entry_index].get("displayTitle"),
                        ))
                    })
                    .filter(|name| !name.is_empty()),
            )
        } else {
            None
        };
        let Some(entry) = state["following"]
            .as_array_mut()
            .and_then(|items| items.get_mut(entry_index))
        else {
            return;
        };
        if let Some(status) = bangumi::collection_status_name(collection.collection_type) {
            entry["bangumiStatus"] = json!(status);
        }
        if collection.collection_type == bangumi::SubjectCollectionType::Done.as_u32() {
            entry["watchedEpisode"] = collection
                .ep_status
                .map(|value| json!(i64::from(value)))
                .unwrap_or(Value::Null);
        }
        if let Some(rate) = collection.rate {
            entry["rating"] = json!(rate);
        }
        if let Some(name_cn) = suggestion_title {
            report.suggestions.push(bangumi::BangumiSyncSuggestion {
                subject_id: collection.subject_id,
                name_cn,
                collection_type: collection.collection_type,
            });
        }
        entry["lastPulledPayloadHash"] = json!(h_remote);
        entry["lastChangedBy"] = json!("bangumi");
        entry["lastPulledFromBangumiAt"] = json!(now_seconds());
        entry["syncUpdatedAt"] = json!(now_millis());
        if collection.collection_type == bangumi::SubjectCollectionType::Doing.as_u32() {
            entry["localFollowIntentAt"] = Value::Null;
        }
    }

    /// 收藏（内嵌 SlimSubject 优先，缺信息用补拉的 Subject 详情）→
    /// `bangumi_following_entry` 所需的 Anime 形状（Phase 2 契约）。
    /// anilistId 由离线映射反查（使 AIRING_QUERY 迁移期补充继续生效）。
    fn collection_subject_anime(
        collection: &BangumiCollection,
        detail: Option<&BangumiSubject>,
        offline_map: &Value,
    ) -> Value {
        let subject_id = collection.subject_id;
        let slim = collection.subject.as_ref();
        let name = detail
            .map(|subject| subject.name.clone())
            .or_else(|| slim.map(|subject| subject.name.clone()))
            .unwrap_or_default();
        let name_cn = detail
            .and_then(|subject| subject.name_cn.clone())
            .or_else(|| slim.and_then(|subject| subject.name_cn.clone()));
        let date = detail
            .and_then(|subject| subject.date.clone())
            .or_else(|| slim.and_then(|subject| subject.date.clone()));
        let eps = detail
            .and_then(|subject| subject.eps)
            .or_else(|| slim.and_then(|subject| subject.eps));
        let images = detail
            .and_then(|subject| subject.images.clone())
            .or_else(|| slim.and_then(|subject| subject.images.clone()));
        let platform = detail.and_then(|subject| subject.platform.clone());
        let anilist_id = offline_bangumi_subject(offline_map, subject_id)
            .and_then(|entry| entry.get("a").and_then(Value::as_i64))
            .filter(|anilist_id| *anilist_id > 0);
        let pick_image = |pick: fn(&bangumi::BangumiSubjectImages) -> &Option<String>| -> String {
            images
                .as_ref()
                .and_then(|images| pick(images).clone())
                .filter(|url| !url.is_empty())
                .unwrap_or_default()
        };
        let extra_large = {
            let large = pick_image(|images| &images.large);
            if large.is_empty() {
                pick_image(|images| &images.common)
            } else {
                large
            }
        };
        let medium = {
            let medium = pick_image(|images| &images.medium);
            if medium.is_empty() {
                pick_image(|images| &images.common)
            } else {
                medium
            }
        };
        let (season_year, start_date) = match date.as_deref().and_then(|date| {
            let mut parts = date.split('-');
            let year: i64 = parts.next()?.parse().ok()?;
            let month: i64 = parts.next()?.parse().ok()?;
            let day: i64 = parts.next()?.parse().ok()?;
            Some((year, month, day))
        }) {
            Some((year, month, day)) => (
                json!(year),
                json!({"year": year, "month": month, "day": day}),
            ),
            None => (Value::Null, Value::Null),
        };
        json!({
            "id": subject_id,
            "source": "bangumi",
            "bangumiSubjectId": subject_id,
            "anilistId": anilist_id.map(|id| json!(id)).unwrap_or(Value::Null),
            "nameCn": name_cn,
            "title": {
                "native": name_cn.clone().unwrap_or_else(|| name.clone()),
                "english": Value::Null,
                "romaji": name
            },
            "coverImage": {"extraLarge": extra_large, "medium": medium, "color": Value::Null},
            "format": bangumi_platform_to_format(platform.as_deref()),
            "episodes": eps,
            "seasonYear": season_year,
            "startDate": start_date,
            "nextAiringEpisode": Value::Null
        })
    }

    /// sort/ep 数值 → 任务集数键（整数）：round 相等且 |value - ep| < 0.25 才映射
    /// （验收第 4 轮问题 2：SP 等特殊集的 4.5 不得错配第 4 或第 5 集）。
    pub(super) fn episode_sort_key(sort: f64) -> Option<i64> {
        let rounded = sort.round();
        if rounded > 0.0 && (sort - rounded).abs() < 0.25 {
            Some(rounded as i64)
        } else {
            None
        }
    }

    /// Bangumi 的 `sort` 是作品全局顺序，而 `ep` 是当前 subject/分季的本地
    /// 集号。标准版任务和 AniList 的 `nextAiringEpisode` 都使用后者；若优先
    /// 使用 sort，分季条目（例如 Re: 从零第四季夺还篇 sort=78..）会被误当成
    /// 第 78 集，导致 next 为空、任务被标记为无日程或随后清掉。
    pub(super) fn episode_number(episode: &bangumi::BangumiEpisode) -> Option<i64> {
        episode
            .ep
            .and_then(episode_sort_key)
            .or_else(|| episode.sort.and_then(episode_sort_key))
    }

    /// 集数记录 → {任务集数: episode_id}；同键冲突取更贴近整数的记录。
    pub(super) fn episode_id_map(episodes: &[bangumi::BangumiEpisode]) -> BTreeMap<i64, i64> {
        let mut best: BTreeMap<i64, (i64, f64)> = BTreeMap::new();
        for episode in episodes {
            if episode.id <= 0 {
                continue;
            }
            let Some(key) = episode_number(episode) else {
                continue;
            };
            let value = episode
                .ep
                .filter(|ep| episode_sort_key(*ep) == Some(key))
                .or(episode.sort)
                .unwrap_or(key as f64);
            let distance = (value - key as f64).abs();
            match best.get(&key) {
                Some((_, existing)) if *existing <= distance => {}
                _ => {
                    best.insert(key, (episode.id, distance));
                }
            }
        }
        best.into_iter().map(|(key, (id, _))| (key, id)).collect()
    }

    /// 验收第 4 轮问题 2：解析条目的 Bangumi episodeId 映射（任务集数 →
    /// episode_id）。`GET /v0/episodes?subject_id=S&limit=200`（公开端点，
    /// 经 HttpBangumiClient 全局 Semaphore 限流）；缓存
    /// `bangumi-cache/episodes-{id}.json` TTL 24h，网络失败回落旧缓存
    /// （stale），连缓存都没有 → Err（调用方跳过该条目本轮进度写回）。
    pub(super) async fn resolve_subject_episode_ids(
        http: &HttpBangumiClient,
        cache_dir: &std::path::Path,
        subject_id: i64,
        now: i64,
    ) -> Result<BTreeMap<i64, i64>, String> {
        if subject_id <= 0 {
            return Err("无效的 subjectId".to_string());
        }
        let cache_path = cache_dir.join(format!("episodes-{subject_id}.json"));
        let read_cache = |max_age: i64| -> Option<bangumi::Paged<bangumi::BangumiEpisode>> {
            let raw = std::fs::read_to_string(&cache_path).ok()?;
            let value: Value = serde_json::from_str(&raw).ok()?;
            let fetched_at = value.get("fetchedAt").and_then(Value::as_i64)?;
            if now.saturating_sub(fetched_at) > max_age {
                return None;
            }
            serde_json::from_value(value.get("paged").cloned().unwrap_or(Value::Null)).ok()
        };
        if let Some(paged) = read_cache(EPISODES_CACHE_TTL_SECONDS) {
            return Ok(episode_id_map(&paged.data));
        }
        match http
            .get_subject_episodes(subject_id, EPISODES_PAGE_LIMIT, 0)
            .await
        {
            Ok(paged) => {
                if let Some(directory) = cache_path.parent() {
                    let _ = std::fs::create_dir_all(directory);
                }
                let payload = json!({
                    "fetchedAt": now,
                    "paged": serde_json::to_value(&paged).unwrap_or(Value::Null),
                });
                let _ = std::fs::write(&cache_path, payload.to_string());
                Ok(episode_id_map(&paged.data))
            }
            Err(error) => match read_cache(i64::MAX) {
                Some(paged) => Ok(episode_id_map(&paged.data)),
                None => Err(super::bangumi_commands::request_error_message(error)),
            },
        }
    }

    /// 任务 3：写回引擎。前置：Token 且 pushLocalChanges（进度另需
    /// pushCompletedEpisodes）。写前 hash 幂等：H_local == lastPushedPayloadHash
    /// 跳过；lastChangedBy=="bangumi" 跳过（防循环）。成功后更新
    /// lastPushedPayloadHash / lastPushedToBangumiAt，并清除取消队列对应项。
    /// 无拉取基线的条目先经 `GET /v0/users/{username}/collections/{subject_id}`
    /// 探测远端记录（404 → POST 创建，200 → PATCH 更新，官方写端点用 `-` 占位）。
    pub(super) async fn push_local_changes(
        http: &HttpBangumiClient,
        tokens: &dyn BangumiTokenStore,
        username_cache: &Mutex<Option<String>>,
        state: &Mutex<Value>,
        cache_dir: &std::path::Path,
    ) -> BangumiSyncReport {
        let mut report = BangumiSyncReport::default();
        let settings = {
            let guard = state.lock().expect("state lock");
            sync_settings(&guard)
        };
        if !settings.push_local_changes && !settings.push_completed_episodes {
            return report; // skipped：写回未启用
        }
        let token = match tokens.load() {
            Ok(Some(token)) if !token.trim().is_empty() => token,
            _ => return report, // skipped：无 Token
        };

        // 第 0 步（验收第 4 轮问题 2）：episodeId 解析与绑定。本地任务从不绑
        // Bangumi episodeId（集数列表从未拉取），进度推送此前永远空转；此处
        // 对涉事条目拉取集数列表（24h 缓存），把待推送完成任务的 episodeId
        // 补上（任务级 lastPushedToBangumiAt 语义不变）。解析失败 → 该条目
        // 本轮跳过进度写回（记 errors，不阻断其他条目）。
        if settings.push_completed_episodes {
            let targets: Vec<i64> = {
                let guard = state.lock().expect("state lock");
                let mut subjects = std::collections::BTreeSet::new();
                for task in guard["tasks"].as_array().into_iter().flatten() {
                    if push_candidate_task(task) && !task_has_episode_id(task) {
                        let subject_id = value_i64(task.get("subjectId"));
                        if subject_id > 0 {
                            subjects.insert(subject_id);
                        }
                    }
                }
                subjects.into_iter().collect()
            };
            let mut resolved: BTreeMap<i64, BTreeMap<i64, i64>> = BTreeMap::new();
            for subject_id in targets {
                match resolve_subject_episode_ids(http, cache_dir, subject_id, now_seconds()).await
                {
                    Ok(mapping) => {
                        resolved.insert(subject_id, mapping);
                    }
                    Err(message) => report
                        .errors
                        .push(format!("解析集数列表失败（{subject_id}）：{message}")),
                }
            }
            if !resolved.is_empty() {
                let mut guard = state.lock().expect("state lock");
                let bound_at = now_millis();
                for task in guard["tasks"].as_array_mut().into_iter().flatten() {
                    if !(push_candidate_task(task) && !task_has_episode_id(task)) {
                        continue;
                    }
                    let subject_id = value_i64(task.get("subjectId"));
                    let episode = value_i64(task.get("episode"));
                    if let Some(episode_id) = resolved
                        .get(&subject_id)
                        .and_then(|mapping| mapping.get(&episode))
                        .copied()
                    {
                        task["episodeId"] = json!(episode_id);
                        task["syncUpdatedAt"] = json!(bound_at);
                    }
                }
            }
        }

        // 第 1 步（锁内）：快照写回计划。
        struct CollectionPush {
            subject_id: i64,
            payload: Value,
            create: bool,
            hash: String,
        }
        let (follows, unfollows, refollowed, episode_batches) = {
            let guard = state.lock().expect("state lock");
            let mut follows: Vec<CollectionPush> = Vec::new();
            let mut unfollows: Vec<i64> = Vec::new();
            let mut refollowed: Vec<i64> = Vec::new();
            if settings.push_local_changes {
                // 最近取消队列：本地取消追番刚发生 → PATCH type=5。
                if let Some(queue) = guard
                    .get("pendingBangumiUnfollows")
                    .and_then(Value::as_array)
                {
                    for item in queue {
                        let subject_id = value_i64(item.get("subjectId"));
                        if subject_id <= 0 {
                            continue;
                        }
                        if find_entry_index(&guard, subject_id).is_some() {
                            // 已重新追番：丢弃队列项，不推 type=5。
                            refollowed.push(subject_id);
                        } else {
                            unfollows.push(subject_id);
                        }
                    }
                }
                for entry in guard["following"].as_array().into_iter().flatten() {
                    if value_string(entry.get("source")) != "bangumi" {
                        continue;
                    }
                    // 防循环：拉取来的变更（lastChangedBy=bangumi）不自动推送。
                    if entry.get("lastChangedBy").and_then(Value::as_str) == Some("bangumi") {
                        continue;
                    }
                    let subject_id = value_i64(entry.get("id"));
                    if subject_id <= 0 {
                        continue;
                    }
                    let hash = local_collection_hash(entry);
                    // hash 幂等：本地无未推送变更 → 跳过。
                    if entry.get("lastPushedPayloadHash").and_then(Value::as_str)
                        == Some(hash.as_str())
                    {
                        continue;
                    }
                    // 远端有记录（拉取过）→ PATCH；无记录 → POST 创建。
                    let create = entry
                        .get("lastPulledPayloadHash")
                        .is_none_or(Value::is_null);
                    follows.push(CollectionPush {
                        subject_id,
                        payload: local_collection_payload(entry),
                        create,
                        hash,
                    });
                }
            }
            let mut episode_batches: Vec<(i64, Vec<i64>)> = Vec::new();
            if settings.push_completed_episodes {
                let mut grouped: BTreeMap<i64, Vec<i64>> = BTreeMap::new();
                for task in guard["tasks"].as_array().into_iter().flatten() {
                    if !push_candidate_task(task) {
                        continue;
                    }
                    let subject_id = task
                        .get("subjectId")
                        .and_then(Value::as_i64)
                        .filter(|value| *value > 0);
                    let episode_id = task
                        .get("episodeId")
                        .and_then(Value::as_i64)
                        .filter(|value| *value > 0);
                    let (Some(subject_id), Some(episode_id)) = (subject_id, episode_id) else {
                        continue;
                    };
                    grouped.entry(subject_id).or_default().push(episode_id);
                }
                episode_batches = grouped.into_iter().collect();
            }
            (follows, unfollows, refollowed, episode_batches)
        };

        // 第 2 步（网络）：写回请求（官方 `-` 占位当前 token 用户）。
        let mut done_unfollows: Vec<i64> = Vec::new();
        for subject_id in unfollows {
            let dropped = json!({"type": bangumi::SubjectCollectionType::Dropped.as_u32()});
            let result = match http
                .update_collection(&token, subject_id, &dropped, false)
                .await
            {
                Ok(()) => Ok(()),
                // 远端无收藏记录 → POST 创建 type=5（同语义）。
                Err(bangumi::BangumiApiError::NotFound { .. }) => {
                    http.update_collection(&token, subject_id, &dropped, true)
                        .await
                }
                Err(error) => Err(error),
            };
            match result {
                Ok(()) => {
                    done_unfollows.push(subject_id);
                    report.pushed += 1;
                }
                Err(error) => report
                    .errors
                    .push(super::bangumi_commands::request_error_message(error)),
            }
        }
        let mut done_follows: Vec<(i64, String)> = Vec::new();
        for push in follows {
            let mut create = push.create;
            if create {
                // 无拉取基线：先探测远端是否有收藏记录（读端点用 {username}）。
                let username =
                    match super::bangumi_commands::ensure_username(tokens, http, username_cache)
                        .await
                    {
                        Ok(username) => username,
                        Err(message) => {
                            report.errors.push(message);
                            continue;
                        }
                    };
                match http
                    .get_user_collection(&token, &username, push.subject_id)
                    .await
                {
                    Ok(_) => create = false,
                    Err(bangumi::BangumiApiError::NotFound { .. }) => create = true,
                    Err(error) => {
                        report
                            .errors
                            .push(super::bangumi_commands::request_error_message(error));
                        continue;
                    }
                }
            }
            match http
                .update_collection(&token, push.subject_id, &push.payload, create)
                .await
            {
                Ok(()) => {
                    done_follows.push((push.subject_id, push.hash));
                    report.pushed += 1;
                }
                Err(error) => report
                    .errors
                    .push(super::bangumi_commands::request_error_message(error)),
            }
        }
        let mut done_batches: Vec<(i64, Vec<i64>)> = Vec::new();
        for (subject_id, episode_ids) in episode_batches {
            match http
                .update_episode_progress_batch(
                    &token,
                    subject_id,
                    &episode_ids,
                    bangumi::EpisodeCollectionType::Watched,
                )
                .await
            {
                Ok(()) => {
                    done_batches.push((subject_id, episode_ids));
                    report.pushed += 1;
                }
                Err(error) => report
                    .errors
                    .push(super::bangumi_commands::request_error_message(error)),
            }
        }

        // 第 3 步（锁内）：成功后记账。
        {
            let mut guard = state.lock().expect("state lock");
            for subject_id in done_unfollows {
                super::remove_pending_bangumi_unfollow(&mut guard, subject_id);
            }
            for subject_id in refollowed {
                super::remove_pending_bangumi_unfollow(&mut guard, subject_id);
            }
            for (subject_id, hash) in done_follows {
                if let Some(index) = find_entry_index(&guard, subject_id) {
                    guard["following"][index]["lastPushedPayloadHash"] = json!(hash);
                    guard["following"][index]["lastPushedToBangumiAt"] = json!(now_seconds());
                }
            }
            for (subject_id, episode_ids) in done_batches {
                if let Some(tasks) = guard["tasks"].as_array_mut() {
                    for task in tasks.iter_mut() {
                        if value_i64(task.get("subjectId")) == subject_id
                            && task
                                .get("episodeId")
                                .and_then(Value::as_i64)
                                .is_some_and(|episode_id| episode_ids.contains(&episode_id))
                        {
                            task["lastPushedToBangumiAt"] = json!(now_seconds());
                            task["syncUpdatedAt"] = json!(now_millis());
                        }
                    }
                }
            }
        }
        report
    }
}

/// Phase 3 任务 1：合并顶层 `bangumiSyncStatus` 五字段（本地-only，绝不进坚果云
/// 文档——document_from_state 回归测试锁定）。
#[cfg(feature = "standard")]
fn merge_bangumi_sync_status(state: &mut Value, patch: bangumi::BangumiSyncStatus) {
    let mut current: bangumi::BangumiSyncStatus = state
        .get("bangumiSyncStatus")
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or_default();
    if patch.last_full_sync_at.is_some() {
        current.last_full_sync_at = patch.last_full_sync_at;
    }
    if patch.last_web_dav_sync_at.is_some() {
        current.last_web_dav_sync_at = patch.last_web_dav_sync_at;
    }
    if patch.last_bangumi_sync_at.is_some() {
        current.last_bangumi_sync_at = patch.last_bangumi_sync_at;
    }
    if patch.last_schedule_sync_at.is_some() {
        current.last_schedule_sync_at = patch.last_schedule_sync_at;
    }
    if patch.last_sync_error.is_some() {
        current.last_sync_error = patch.last_sync_error;
    }
    if let Some(object) = state.as_object_mut() {
        object.insert(
            "bangumiSyncStatus".into(),
            serde_json::to_value(current).unwrap_or_else(|_| json!({})),
        );
    }
}

/// Phase 4 拆分（LOCAL 方案 §7.4）：Bangumi 全量同步的作用域判定（纯函数，
/// 测试锁定 skipped 语义）。坚果云合并与播出数据刷新**无条件**执行，Bangumi
/// 开关/Token 只决定 Bangumi 网络段（步骤 3-5）是否运行——Android 前台过期
/// 补偿跨进程靠 lastFullSyncAt 更新自然节流，skipped 路径也必须刷新本地
/// 数据并落状态（否则每次前台都会重复触发补偿）。
#[cfg(feature = "standard")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BangumiSyncScope {
    /// 开关开启且 Token 就绪：七步全量。
    Full,
    /// 开关关闭或无 Token：坚果云 + 播出数据照常，仅跳过 Bangumi 网络段。
    LocalOnly {
        /// 用户可读的跳过原因（不包含任何凭据信息）。
        reason: &'static str,
    },
}

#[cfg(feature = "standard")]
fn bangumi_sync_scope(sync_enabled: bool, has_token: bool) -> BangumiSyncScope {
    if !sync_enabled {
        BangumiSyncScope::LocalOnly {
            reason: "Bangumi 同步未启用",
        }
    } else if !has_token {
        BangumiSyncScope::LocalOnly {
            reason: "尚未保存 Bangumi Token",
        }
    } else {
        BangumiSyncScope::Full
    }
}

/// Phase 3 任务 4：`bangumi_sync_now` 完整同步（LOCAL 方案 §7.3 七步）：
/// 1) 坚果云同步 2) 主数据轻刷新 3) 收藏拉取合并 4) 合并（引擎内落账）
/// 5) 写回（按开关）6) 唤醒 WebDAV 上传 7) 重排 + 同步状态更新。
/// Phase 4 拆分：步骤 1/2/6/7 无条件执行（skipped 语义见 [`BangumiSyncScope`]），
/// Bangumi 开关/Token 只门控步骤 3-5。
/// 错误摘要只含用户可读文案（BangumiApiError Display 经测试锁定不含
/// Token/Authorization），截断 300 字符。
#[cfg(feature = "standard")]
async fn run_full_bangumi_sync(app: &AppHandle, context: &AppContext) -> Result<Value, String> {
    let sync_enabled = {
        let state = context.state.lock().map_err(|_| "状态锁不可用")?;
        bangumi_sync::sync_settings(&state).sync_enabled
    };
    let has_token = context
        .bangumi_tokens
        .load()
        .ok()
        .flatten()
        .is_some_and(|token| !token.trim().is_empty());
    let scope = bangumi_sync_scope(sync_enabled, has_token);
    // LocalOnly 且 WebDAV 未启用时静默跳过坚果云步骤：不给未使用坚果云的
    // 用户写 lastSyncError（否则前台错误重试兜底会无意义地反复触发）。
    // Full 作用域保持 Phase 3 行为：始终尝试，失败计入错误摘要。
    let webdav = if scope == BangumiSyncScope::Full || webdav_is_enabled(app, context) {
        Some(perform_platform_webdav_sync(app, context))
    } else {
        None
    };
    // 2) 主数据轻刷新：Android 桥为同步调用（错误忽略，保持 Phase 3 `let _`
    //    语义，经 async 块延迟到步骤 2 位置执行）；桌面复用 sync_now_inner。
    #[cfg(target_os = "android")]
    let schedule = async move {
        let _ = mobile::sync_native(app, context);
        Ok::<Value, String>(json!({}))
    };
    #[cfg(not(target_os = "android"))]
    let schedule = sync_now_inner(app, context);
    let payload = run_full_bangumi_sync_core(context, scope, webdav, schedule).await?;
    refresh_mobile_configuration(app, context)?;
    emit_state(app, context);
    Ok(payload)
}

/// Phase 4 拆分：七步编排核心。`webdav` / `schedule` 两个步骤 future 由调用方
/// 注入（生产 = 平台实现；测试 = 记录型闭包），其余五步直接驱动 `context`，
/// 使"无 Token → 坚果云步骤仍执行"可离线回归测试（无需 AppHandle）。
#[cfg(feature = "standard")]
async fn run_full_bangumi_sync_core(
    context: &AppContext,
    scope: BangumiSyncScope,
    webdav: Option<impl std::future::Future<Output = anyhow::Result<Value>>>,
    schedule: impl std::future::Future<Output = Result<Value, String>>,
) -> Result<Value, String> {
    let mut report = bangumi::BangumiSyncReport::default();
    let mut error_summary: Vec<String> = Vec::new();
    // 1) 坚果云同步（三字段业务数据先合流）。Phase 4：不受 Bangumi 开关/Token
    //    门控；webdav=None 表示本机未启用坚果云（仅 LocalOnly 场景），静默跳过。
    let webdav_ran = webdav.is_some();
    if let Some(webdav) = webdav {
        match webdav.await {
            Ok(_) => {
                let mut state = context.state.lock().map_err(|_| "状态锁不可用")?;
                merge_bangumi_sync_status(
                    &mut state,
                    bangumi::BangumiSyncStatus {
                        last_web_dav_sync_at: Some(now_seconds()),
                        ..bangumi::BangumiSyncStatus::default()
                    },
                );
            }
            Err(error) => error_summary.push(format!("坚果云同步失败：{error}")),
        }
    }
    // 2) 主数据轻刷新（播出调度 / 任务）——schedule future 由调用方注入
    //    （Android = mobile 桥；桌面 = sync_now_inner）。
    if let Err(error) = schedule.await {
        error_summary.push(format!("主数据刷新失败：{error}"));
    }
    {
        let mut state = context.state.lock().map_err(|_| "状态锁不可用")?;
        merge_bangumi_sync_status(
            &mut state,
            bangumi::BangumiSyncStatus {
                last_schedule_sync_at: Some(now_seconds()),
                ..bangumi::BangumiSyncStatus::default()
            },
        );
    }
    // 3+4) 收藏拉取合并（引擎内落账）+ 5) 写回——仅 Full 作用域执行
    // （Phase 4 拆分：开关关闭/无 Token 时跳过 Bangumi 网络段）。
    if scope == BangumiSyncScope::Full {
        let base = {
            let state = context.state.lock().map_err(|_| "状态锁不可用")?;
            bangumi_base_urls(&state)
        };
        let http = bangumi::HttpBangumiClient::new(context.client.clone(), base);
        let pull_report = bangumi_sync::run_bangumi_collection_sync(
            &http,
            context.bangumi_tokens.as_ref(),
            &context.bangumi_username_cache,
            &context.state,
            &context.offline_bangumi,
        )
        .await;
        report.pulled = pull_report.pulled;
        report.followed = pull_report.followed;
        report.unfollowed = pull_report.unfollowed;
        report.completed_tasks = pull_report.completed_tasks;
        report.suggestions = pull_report.suggestions;
        report.conflicts = pull_report.conflicts;
        for error in &pull_report.errors {
            report.errors.push(error.clone());
        }
        error_summary.extend(pull_report.errors.iter().cloned());
        let pull_touched = pull_report.followed > 0
            || pull_report.unfollowed > 0
            || pull_report.completed_tasks > 0
            || !pull_report.errors.is_empty();
        if pull_touched {
            context.save_state().map_err(|error| error.to_string())?;
        }
        // 5) 写回（按开关）。cache_dir 供进度写回的集数列表缓存
        // （bangumi-cache/episodes-{id}.json）。
        let push_report = bangumi_sync::push_local_changes(
            &http,
            context.bangumi_tokens.as_ref(),
            &context.bangumi_username_cache,
            &context.state,
            &bangumi_cache_dir(context),
        )
        .await;
        report.pushed += push_report.pushed;
        for error in &push_report.errors {
            report.errors.push(error.clone());
        }
        error_summary.extend(push_report.errors.iter().cloned());
        if push_report.pushed > 0 {
            context.save_state().map_err(|error| error.to_string())?;
        }
    }
    // 6) 唤醒 WebDAV 后台上传合并后的三字段文档。
    context.webdav_wakeup.notify_one();
    // 7) 重排 + 同步状态更新（成功/失败/skipped 都更新 lastFullSyncAt，
    //    供 Android 前台过期补偿跨进程节流）。
    {
        let mut state = context.state.lock().map_err(|_| "状态锁不可用")?;
        let joined = error_summary.join("；");
        merge_bangumi_sync_status(
            &mut state,
            bangumi::BangumiSyncStatus {
                last_bangumi_sync_at: Some(now_seconds()),
                last_full_sync_at: Some(now_seconds()),
                last_sync_error: Some(if joined.is_empty() {
                    String::new()
                } else {
                    joined.chars().take(300).collect()
                }),
                ..bangumi::BangumiSyncStatus::default()
            },
        );
    }
    context.save_state().map_err(|error| error.to_string())?;
    let ok = report.errors.is_empty();
    let message = match scope {
        BangumiSyncScope::Full => {
            if ok {
                "Bangumi 同步完成".to_string()
            } else {
                format!("同步完成，但出现 {} 条错误", report.errors.len())
            }
        }
        BangumiSyncScope::LocalOnly { reason } => {
            if webdav_ran {
                format!("{reason}；坚果云与播出数据已按需刷新")
            } else {
                format!("{reason}；播出数据已按需刷新")
            }
        }
    };
    Ok(
        json!({"ok": ok, "message": message, "report": serde_json::to_value(&report).unwrap_or_default()}),
    )
}

// ---------------------------------------------------------------------------
// Phase 4：Android 前台过期同步补偿（LOCAL 方案 §7.4）。
//
// 主保障是 Java 层 WorkManager（~6h，CONNECTED 约束）+ AlarmManager；Rust 侧
// 仅做前台轻量兜底：应用启动（setup，任务 1）与 get_state（任务 2，含
// consume_events 事件循环之后）时检查顶层 bangumiSyncStatus.lastFullSyncAt
// 是否过期（无记录视为过期；距 now 超 15 分钟过期），过期则在本进程内补偿
// 一次 run_full_bangumi_sync（内部已含开关/Token 判断与七步编排）；另对
// "上次同步有错误"场景（任务 3）按 30 分钟节流做前台重试。同步完成/失败/
// skipped 都由 run_full_bangumi_sync 步骤 7 落 bangumiSyncStatus，跨进程靠
// lastFullSyncAt 已更新自然节流。Windows 桌面路径零变化（下方接线全部
// Android + standard 双门控编译；桌面后台仍是 start_webdav_background /
// start_desktop_background 原逻辑）。
// ---------------------------------------------------------------------------

/// Phase 4：前台过期同步补偿的纯判定逻辑 + single-flight 门。生产接线只在
/// Android 编译；桌面构建下这些项仅供离线测试使用（allow(dead_code)）。
#[cfg(feature = "standard")]
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
mod foreground_sync {
    use std::sync::atomic::{AtomicBool, Ordering};

    /// 前台过期阈值：距上次全量同步超过 15 分钟（900 秒）视为过期。
    pub const STALE_AFTER_SECS: i64 = 900;
    /// 错误重试阈值：距上次同步尝试超过 30 分钟才再次补偿。
    pub const ERROR_RETRY_AFTER_SECS: i64 = 1_800;

    /// lastFullSyncAt 三态（now 注入，便于测试边界）。
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum SyncStaleness {
        /// 无 lastFullSyncAt 记录，视为过期需补偿。
        Missing,
        /// 距 now 超过阈值，需补偿。
        Stale,
        /// 新鲜（含恰好等于阈值的边界："超 15 分钟"为严格大于），无需补偿。
        Fresh,
    }

    pub fn staleness(last_full_sync_at: Option<i64>, now: i64) -> SyncStaleness {
        match last_full_sync_at {
            None => SyncStaleness::Missing,
            Some(at) if now.saturating_sub(at) > STALE_AFTER_SECS => SyncStaleness::Stale,
            Some(_) => SyncStaleness::Fresh,
        }
    }

    /// 任务 3 错误重试判定：lastSyncError 非空且距上次尝试超 30 分钟。
    /// 上次尝试时间缺失（None）时不重试（从未同步过没有可重试的错误语境）；
    /// 每次尝试都会刷新 lastBangumiSyncAt，失败场景最多每 30 分钟兜底一次。
    pub fn error_retry_due(
        last_sync_error: Option<&str>,
        last_attempt_at: Option<i64>,
        now: i64,
    ) -> bool {
        let has_error = last_sync_error.is_some_and(|error| !error.trim().is_empty());
        has_error
            && last_attempt_at.is_some_and(|at| now.saturating_sub(at) > ERROR_RETRY_AFTER_SECS)
    }

    /// 进程内 single-flight 门：compare_exchange 保证并发下只有一个赢家
    /// （任务 1/2/3 共用，同一时刻最多一个补偿同步在跑）。
    pub struct SingleFlightGate(AtomicBool);

    impl SingleFlightGate {
        pub const fn new() -> Self {
            Self(AtomicBool::new(false))
        }
        /// 尝试占用门；已有任务在跑时返回 false。
        pub fn try_begin(&self) -> bool {
            self.0
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
        }
        /// 释放门（补偿同步结束后调用）。
        pub fn finish(&self) {
            self.0.store(false, Ordering::Release);
        }
        pub fn is_running(&self) -> bool {
            self.0.load(Ordering::Acquire)
        }
    }
}

/// 进程生命周期内过期补偿只执行一次的标志（任务 1/2 共用：防应用频繁重启
/// 场景下同进程重复触发；跨进程由 lastFullSyncAt 已更新自然节流）。
#[cfg(all(feature = "standard", target_os = "android"))]
static STALE_COMPENSATION_DONE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// 任务 1/2/3 共用的 single-flight 门：同一时刻最多一个补偿同步在跑。
#[cfg(all(feature = "standard", target_os = "android"))]
static COMPENSATION_GATE: foreground_sync::SingleFlightGate =
    foreground_sync::SingleFlightGate::new();

/// 读取顶层 bangumiSyncStatus 五字段（本地-only，document_from_state 不外发；
/// 缺失/损坏按默认值处理，即无 lastFullSyncAt → 视为过期）。
#[cfg(all(feature = "standard", target_os = "android"))]
fn current_bangumi_sync_status(context: &AppContext) -> bangumi::BangumiSyncStatus {
    context
        .state
        .lock()
        .ok()
        .and_then(|state| state.get("bangumiSyncStatus").cloned())
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or_default()
}

/// Android 前台过期补偿统一入口（任务 1/2/3）：判定过期/错误重试后以
/// single-flight 方式 spawn 后台 run_full_bangumi_sync，不阻塞调用方。
#[cfg(all(feature = "standard", target_os = "android"))]
fn maybe_spawn_foreground_sync(app: &AppHandle, context: &AppContext) {
    if COMPENSATION_GATE.is_running() {
        return;
    }
    let status = current_bangumi_sync_status(context);
    let now = now_seconds();
    // 任务 1/2：过期补偿（进程内一次；Missing 视为过期）。
    let stale_due = !STALE_COMPENSATION_DONE.load(Ordering::Acquire)
        && foreground_sync::staleness(status.last_full_sync_at, now)
            != foreground_sync::SyncStaleness::Fresh;
    // 任务 3：错误重试（与过期补偿独立节流：lastSyncError 非空且距上次
    // 尝试 lastBangumiSyncAt 超 30 分钟；Rust 仅前台兜底，主保障是 Java
    // WorkManager 的 CONNECTED 约束 + BootReceiver）。
    let error_due = foreground_sync::error_retry_due(
        status.last_sync_error.as_deref(),
        status.last_bangumi_sync_at,
        now,
    );
    if !stale_due && !error_due {
        return;
    }
    if stale_due {
        // 先置位再 spawn：并发调用方（setup / get_state）不会重复触发。
        STALE_COMPENSATION_DONE.store(true, Ordering::Release);
    }
    if !COMPENSATION_GATE.try_begin() {
        return;
    }
    let app = app.clone();
    let context = context.clone();
    tauri::async_runtime::spawn(async move {
        // 成功/失败/skipped 都由 run_full_bangumi_sync 落 bangumiSyncStatus，
        // 错误无需在此重复记录。
        let _ = run_full_bangumi_sync(&app, &context).await;
        COMPENSATION_GATE.finish();
    });
}

// ---------------------------------------------------------------------------
// 问题 2b（验收第 2 轮，P0 追番/评分不自动写回；跨平台化）：自动 Bangumi
// 同步循环（standard 编译，Windows/Android 两端挂载；original 不编译本节）。
//
// 此前 run_full_bangumi_sync 只有手动命令（bangumi_sync_now）与 Android 前台
// 15 分钟过期补偿，追番/完成任务/评分的写回（push_local_changes）只能等用户
// 手动点"立即同步 Bangumi"。新增两条触发路径：
// - 周期：每 max(pollIntervalMinutes, 15) 分钟调用一次 run_full_bangumi_sync
//   （内部开关自会 skip：同步未启用/无 Token 时只做坚果云与播出数据按需刷新，
//   与现有后台兼容）。设置页"同步间隔"现有选择 1/5/10/15 分钟，直接透传会
//   对 Bangumi API 形成高频轮询，统一抬到下限 15 分钟保护；
// - 动作唤醒：toggle_follow（新增/取消）、toggle_task（bangumi 任务完成）、
//   bangumi_set_rating 触发 [`BANGUMI_SYNC_WAKEUP`]，进入 30 秒静默期（静默
//   期内再次唤醒则重新计时，合并密集动作，同 start_webdav_background 的
//   5 秒静默合并写法）后执行一轮。
// Android 约束：循环只在进程存活期间运行、随进程死亡——≥15 分钟周期 + 动作
// 唤醒，不要求常驻后台，也不是高频轮询。
// 执行前检查 Token 存在（load Ok(Some)），否则跳过本轮（坚果云/播出刷新由
// 各自的后台循环负责，不在此重复触发）。single-flight 门防本循环重入；手动
// 命令保持既有行为不进门。不用 tokio::select!（tokio 依赖无 macros feature）
// —— 用 Notify::notified() + tokio::time::timeout 组合（仓库既有做法）。
// ---------------------------------------------------------------------------

#[cfg(feature = "standard")]
mod bangumi_sync_loop {
    use std::time::Duration;

    /// 周期下限（分钟）：设置页"同步间隔"选择 1/5/10/15，下限 15 保护
    /// Bangumi API（旧固定值为 60 分钟，第 8 轮问题 3 改为跟随设置）。
    pub const MIN_INTERVAL_MINUTES: i64 = 15;
    /// 周期上限（分钟）：与桌面主同步循环的 1..=1440 钳制同口径。
    pub const MAX_INTERVAL_MINUTES: i64 = 1_440;
    /// 动作唤醒后的静默期：30 秒内后续唤醒合并为一次执行。
    pub const QUIET_SECS: u64 = 30;

    /// 周期内核（纯函数）：settings.pollIntervalMinutes → 周期秒数 =
    /// max(值, 15) 分钟（另设 1440 分钟上限）。缺失/无法读取设置（<=0，
    /// 与 value_i64 缺省一致）→ 15 分钟兜底。
    pub fn interval_secs(poll_interval_minutes: i64) -> u64 {
        (poll_interval_minutes.clamp(MIN_INTERVAL_MINUTES, MAX_INTERVAL_MINUTES) as u64) * 60
    }

    /// 循环内核（纯函数，静默期/节流可测）：下一次等待时长。
    /// - 处于静默期（刚收到动作唤醒）→ 等 [`QUIET_SECS`]；
    /// - 否则等周期 interval_secs（由 [`interval_secs`] 按
    ///   settings.pollIntervalMinutes 现值计算，动作唤醒可提前打断）。
    pub fn wait_duration(quiet_pending: bool, interval_secs: u64) -> Duration {
        Duration::from_secs(if quiet_pending {
            QUIET_SECS
        } else {
            interval_secs
        })
    }

    /// 执行前判定（纯函数）：Token 存在且 single-flight 门空闲才执行。
    pub fn should_execute(has_token: bool, gate_acquired: bool) -> bool {
        has_token && gate_acquired
    }
}

#[cfg(feature = "standard")]
static BANGUMI_SYNC_WAKEUP: std::sync::LazyLock<tokio::sync::Notify> =
    std::sync::LazyLock::new(tokio::sync::Notify::new);

/// 自动同步 single-flight 门（与 Android 前台补偿同思路：同一时刻最多
/// 一轮自动全量同步在跑）。
#[cfg(feature = "standard")]
static BANGUMI_SYNC_LOOP_GATE: foreground_sync::SingleFlightGate =
    foreground_sync::SingleFlightGate::new();

#[cfg(feature = "standard")]
fn start_bangumi_sync_loop(app: AppHandle, context: AppContext) {
    tauri::async_runtime::spawn(async move {
        let mut quiet_pending = false;
        loop {
            // 第 8 轮问题 3：周期每次迭代运行时读取设置（INTERVAL_SECS 常量
            // 改为 interval_secs 函数）——设置页调整"同步间隔"后下一轮生效，
            // 无需重启；读取失败（锁不可用）按缺失 0 处理 → 15 分钟兜底。
            let poll_interval_minutes = {
                let state = context.state.lock().ok();
                state
                    .as_ref()
                    .and_then(|state| state["settings"].get("pollIntervalMinutes"))
                    .and_then(Value::as_i64)
                    .unwrap_or(0)
            };
            let wait_elapsed = tokio::time::timeout(
                bangumi_sync_loop::wait_duration(
                    quiet_pending,
                    bangumi_sync_loop::interval_secs(poll_interval_minutes),
                ),
                BANGUMI_SYNC_WAKEUP.notified(),
            )
            .await
            .is_err();
            if !wait_elapsed {
                // 收到动作唤醒：进入/重置 30 秒静默期（静默期内再次唤醒则
                // 重新计时——wait_duration 返回 QUIET_SECS，效果同重置）。
                quiet_pending = true;
                continue;
            }
            // 静默期无新唤醒后到期，或周期到期 → 执行一轮。
            quiet_pending = false;
            let has_token = context
                .bangumi_tokens
                .load()
                .ok()
                .flatten()
                .is_some_and(|token| !token.trim().is_empty());
            if !bangumi_sync_loop::should_execute(has_token, BANGUMI_SYNC_LOOP_GATE.try_begin()) {
                continue;
            }
            // 成功/失败/skipped 都由 run_full_bangumi_sync 落 bangumiSyncStatus。
            let _ = run_full_bangumi_sync(&app, &context).await;
            BANGUMI_SYNC_LOOP_GATE.finish();
        }
    });
}

/// Original 版 `bangumi_sync_now` 的统一拒绝返回（report 零值，camelCase 形状
/// 与 standard 版一致，供前端类型稳定）。
fn bangumi_sync_now_rejected() -> Value {
    json!({
        "ok": false,
        "message": "Original 版不支持 Bangumi",
        "report": {
            "pulled": 0, "followed": 0, "unfollowed": 0, "completedTasks": 0,
            "suggestions": [], "conflicts": 0, "pushed": 0, "errors": []
        }
    })
}

#[tauri::command]
async fn bangumi_sync_now(app: AppHandle, context: State<'_, AppContext>) -> Result<Value, String> {
    if context.original {
        return Ok(bangumi_sync_now_rejected());
    }
    #[cfg(feature = "standard")]
    {
        return run_full_bangumi_sync(&app, &context).await;
    }
    #[cfg(not(feature = "standard"))]
    {
        let _ = (&app, &context);
        Ok(bangumi_sync_now_rejected())
    }
}

/// Phase 3：`bangumi_update_sync_settings`（扁平参数，前端契约；只更新提供的键，
/// conflictPolicy 非法值忽略）。Original 运行即拒绝。
/// Phase 3：`bangumi_update_sync_settings`（扁平参数，前端契约；只更新提供的键，
/// conflictPolicy 非法值忽略）。Original 运行即拒绝。
///
/// 问题 1（验收第 2 轮，P0 写回从不发生）：`syncEnabled` 总开关默认 false 且
/// 旧 UI 无该开关，四个子开关全开也会让 run_bangumi_collection_sync /
/// push_local_changes 全部 skipped（零值 report + "Bangumi 同步未启用"）。
/// 服务端便利语义：开任一子开关即视为启用同步。
#[cfg(feature = "standard")]
fn resolve_sync_enabled(explicit: Option<bool>, sub_switches: [Option<bool>; 4]) -> Option<bool> {
    // 显式参数（含 false）优先：用户明确关总开关时不被子开关覆盖。
    explicit.or_else(|| {
        sub_switches
            .into_iter()
            .flatten()
            .any(std::convert::identity)
            .then_some(true)
    })
}

#[tauri::command]
fn bangumi_update_sync_settings(
    app: AppHandle,
    context: State<'_, AppContext>,
    sync_enabled: Option<bool>,
    pull_collections: Option<bool>,
    push_local_changes: Option<bool>,
    push_completed_episodes: Option<bool>,
    pull_external_status: Option<bool>,
    conflict_policy: Option<String>,
) -> Result<Value, String> {
    if context.original {
        return Ok(bangumi_command_rejected());
    }
    #[cfg(feature = "standard")]
    {
        {
            let mut state = context.state.lock().map_err(|_| "状态锁不可用")?;
            let Some(block) = state.get_mut("bangumi").and_then(Value::as_object_mut) else {
                return Err("Bangumi 设置块不存在".into());
            };
            // 问题 1：显式 sync_enabled 优先；未提供且任一子开关为 true →
            // 隐式同时置 syncEnabled=true（"开任一子开关即视为启用同步"，
            // 旧 UI 无总开关，否则子开关全开也被 skipped）。
            if let Some(value) = resolve_sync_enabled(
                sync_enabled,
                [
                    pull_collections,
                    push_local_changes,
                    push_completed_episodes,
                    pull_external_status,
                ],
            ) {
                block.insert("syncEnabled".into(), json!(value));
            }
            if let Some(value) = pull_collections {
                block.insert("pullCollections".into(), json!(value));
            }
            if let Some(value) = push_local_changes {
                block.insert("pushLocalChanges".into(), json!(value));
            }
            if let Some(value) = push_completed_episodes {
                block.insert("pushCompletedEpisodes".into(), json!(value));
            }
            if let Some(value) = pull_external_status {
                block.insert("pullExternalStatus".into(), json!(value));
            }
            if let Some(policy) = conflict_policy.map(|policy| policy.trim().to_lowercase()) {
                if ["latest", "local-first", "bangumi-first"].contains(&policy.as_str()) {
                    block.insert("conflictPolicy".into(), json!(policy));
                }
            }
        }
        context.save_state().map_err(|error| error.to_string())?;
        refresh_mobile_configuration(&app, &context)?;
        emit_state(&app, &context);
        return Ok(context.public_state());
    }
    #[cfg(not(feature = "standard"))]
    {
        let _ = (
            &app,
            sync_enabled,
            pull_collections,
            push_local_changes,
            push_completed_episodes,
            pull_external_status,
            conflict_policy,
        );
        Ok(bangumi_command_rejected())
    }
}

/// 状态驱动追踪（任务 1）`bangumi_set_collection_status` 的纯内核：按目标
/// 收藏状态应用本地语义，返回是否发生变更（条目按 id/bangumiId==subjectId
/// 定位，缺失返回 false）。
/// - dropped → 复用取消追番语义（[`remove_following`]：pending 删、completed
///   留、墓碑；bangumi 来源入「最近取消队列」供写回 PATCH type=5）；
/// - wish → status=wish + 删除该作品 pending（completed 作为观看历史保留）；
/// - on_hold → status=on_hold（pending 保留）；
/// - done → status=done + 该作品 pending 全部标记 completed（completedAt=now
///   秒、lastChangedBy=local；不新建——门控已拦）；
/// - doing → status=doing（恢复追踪）。
/// 除 dropped（条目已删）外全路径 `lastChangedBy="local"`。
#[cfg(feature = "standard")]
fn apply_bangumi_collection_status(state: &mut Value, subject_id: i64, status: &str) -> bool {
    let Some(index) = state["following"].as_array().and_then(|items| {
        items.iter().position(|item| {
            value_i64(item.get("id")) == subject_id
                || value_i64(item.get("bangumiId")) == subject_id
        })
    }) else {
        return false;
    };
    let entry_id = value_i64(state["following"][index].get("id"));
    if status == "dropped" {
        remove_following(state, entry_id);
        return true;
    }
    {
        let entry = &mut state["following"][index];
        entry["bangumiStatus"] = json!(status);
        entry["lastChangedBy"] = json!("local");
    }
    // syncUpdatedAt=now 毫秒 + 清除该 id 的既有墓碑（与其他追番写路径一致）。
    mark_following_changed(state, entry_id);
    // 任务匹配与拉取引擎 done 分支一致（animeId；subjectId 兜底跨键任务）。
    let task_matches = |task: &Value| {
        value_i64(task.get("animeId")) == entry_id || value_i64(task.get("subjectId")) == entry_id
    };
    match status {
        // wish：收录不追踪，删除该作品未完成任务（completed 保留）。
        "wish" => {
            state["tasks"].as_array_mut().unwrap().retain(|task| {
                !(task_matches(task) && value_string(task.get("status")) == "pending")
            });
        }
        // done：全部看完，该作品 pending 全部标记完成。
        "done" => {
            let completed_at = now_seconds();
            let synced_at = now_millis();
            for task in state["tasks"].as_array_mut().unwrap().iter_mut() {
                if task_matches(task) && value_string(task.get("status")) == "pending" {
                    task["status"] = json!("completed");
                    task["completedAt"] = json!(completed_at);
                    task["syncUpdatedAt"] = json!(synced_at);
                    task["lastChangedBy"] = json!("local");
                }
            }
        }
        // doing / on_hold：pending 保留。
        _ => {}
    }
    true
}

/// 状态驱动追踪（任务 1）：`bangumi_set_collection_status({ subjectId, status })`
/// → `{ ok, message, state }`。status ∈ wish|doing|done|on_hold|dropped，本地
/// 语义见 [`apply_bangumi_collection_status`]。有 Token 时立即写回
/// `PATCH /v0/users/-/collections/{subject_id}`（404 → POST 创建；payload 与
/// 写回引擎一致，type 由 bangumiStatus 映射 + 可选 rate），成功后记账
/// lastPushedPayloadHash（dropped 成功则清出取消队列），失败不阻断本地生效
/// （后续同步由 push_local_changes 重试）；无 Token 仅本地生效。original 运行
/// 即拒绝。
#[tauri::command]
async fn bangumi_set_collection_status(
    app: AppHandle,
    context: State<'_, AppContext>,
    subject_id: i64,
    status: String,
) -> Result<Value, String> {
    if context.original {
        return Ok(json!({
            "ok": false,
            "message": "Original 版不支持 Bangumi",
            "state": context.public_state()
        }));
    }
    #[cfg(feature = "standard")]
    {
        const VALID_STATUSES: [&str; 5] = ["wish", "doing", "done", "on_hold", "dropped"];
        if !VALID_STATUSES.contains(&status.as_str()) {
            return Ok(json!({
                "ok": false,
                "message": "无效的收藏状态",
                "state": context.public_state()
            }));
        }
        let dropped = status == "dropped";
        let (has_token, base, write_payload) = {
            let mut state = context.state.lock().map_err(|_| "状态锁不可用")?;
            if !apply_bangumi_collection_status(&mut state, subject_id, &status) {
                return Ok(json!({
                    "ok": false,
                    "message": "未找到对应追番条目",
                    "state": context.public_state()
                }));
            }
            let has_token = context
                .bangumi_tokens
                .load()
                .ok()
                .flatten()
                .is_some_and(|token| !token.trim().is_empty());
            let write_payload = if dropped {
                Some(json!({"type": bangumi::SubjectCollectionType::Dropped.as_u32()}))
            } else {
                state["following"].as_array().and_then(|items| {
                    items
                        .iter()
                        .find(|item| {
                            value_i64(item.get("id")) == subject_id
                                || value_i64(item.get("bangumiId")) == subject_id
                        })
                        .map(bangumi_sync::local_collection_payload)
                })
            };
            let base = bangumi_base_urls(&state);
            (has_token, base, write_payload)
        };
        let mut message = String::new();
        if has_token {
            if let (Some(token), Some(payload)) =
                (context.bangumi_tokens.load().ok().flatten(), write_payload)
            {
                let client = bangumi::HttpBangumiClient::new(context.client.clone(), base);
                // 远端已有收藏记录 → PATCH；404 → POST 创建（官方 `-` 占位）。
                let result = match client
                    .update_collection(&token, subject_id, &payload, false)
                    .await
                {
                    Ok(()) => Ok(()),
                    Err(bangumi::BangumiApiError::NotFound { .. }) => {
                        client
                            .update_collection(&token, subject_id, &payload, true)
                            .await
                    }
                    Err(error) => Err(error),
                };
                match result {
                    Ok(()) => {
                        let mut state = context.state.lock().map_err(|_| "状态锁不可用")?;
                        if dropped {
                            // 立即写回成功 → 清出取消队列（防重复 PATCH type=5）。
                            remove_pending_bangumi_unfollow(&mut state, subject_id);
                        } else if let Some(index) =
                            state["following"].as_array().and_then(|items| {
                                items.iter().position(|item| {
                                    value_i64(item.get("id")) == subject_id
                                        || value_i64(item.get("bangumiId")) == subject_id
                                })
                            })
                        {
                            let hash =
                                bangumi_sync::local_collection_hash(&state["following"][index]);
                            state["following"][index]["lastPushedPayloadHash"] = json!(hash);
                            state["following"][index]["lastPushedToBangumiAt"] =
                                json!(now_seconds());
                        }
                        message = "收藏状态已更新并写回 Bangumi".into();
                    }
                    Err(error) => {
                        // 写回失败不阻断本地生效：push_local_changes 后续重试。
                        message = format!(
                            "本地已生效，Bangumi 写回失败：{}",
                            bangumi_commands::request_error_message(error)
                        );
                    }
                }
            }
        } else {
            message = "未连接 Bangumi，仅本地生效".into();
        }
        context.save_state().map_err(|error| error.to_string())?;
        context.webdav_wakeup.notify_one();
        // 本地变更 → 唤醒桌面自动同步（拉取对齐 + 失败兜底重试，PATCH 正确 type）。
        notify_bangumi_sync_wakeup(true);
        refresh_mobile_configuration(&app, &context)?;
        emit_state(&app, &context);
        return Ok(json!({"ok": true, "message": message, "state": context.public_state()}));
    }
    #[cfg(not(feature = "standard"))]
    {
        let _ = (&app, subject_id, status);
        Ok(json!({
            "ok": false,
            "message": "Original 版不支持 Bangumi",
            "state": context.public_state()
        }))
    }
}

/// Phase 3：`bangumi_set_rating({ subjectId, rating })`。本地评分写入 following
/// 条目并标记 lastChangedBy=local（H_local 随之变化，push_local_changes 幂等
/// 判定后写回 PATCH rate；rating=null 表示清除评分）。
#[tauri::command]
fn bangumi_set_rating(
    app: AppHandle,
    context: State<'_, AppContext>,
    subject_id: i64,
    rating: Option<u8>,
) -> Result<Value, String> {
    if context.original {
        return Ok(bangumi_command_rejected());
    }
    #[cfg(feature = "standard")]
    {
        if rating.is_some_and(|value| value > 10) {
            return Ok(json!({"ok": false, "message": "评分需在 0-10 之间"}));
        }
        let mut state = context.state.lock().map_err(|_| "状态锁不可用")?;
        let index = state["following"].as_array().and_then(|items| {
            items.iter().position(|item| {
                value_i64(item.get("id")) == subject_id
                    || value_i64(item.get("bangumiId")) == subject_id
            })
        });
        let Some(index) = index else {
            return Ok(json!({"ok": false, "message": "未找到对应追番条目"}));
        };
        let anime_id = value_i64(state["following"][index].get("id"));
        state["following"][index]["rating"] =
            rating.map(|value| json!(value)).unwrap_or(Value::Null);
        state["following"][index]["lastChangedBy"] = json!("local");
        mark_following_changed(&mut state, anime_id);
        drop(state);
        context.save_state().map_err(|error| error.to_string())?;
        context.webdav_wakeup.notify_one();
        // 问题 2b：评分是本地变更（上方已置 lastChangedBy=local，核对无误）
        // → 唤醒桌面自动同步（写回 PATCH rate）。
        notify_bangumi_sync_wakeup(true);
        emit_state(&app, &context);
        return Ok(json!({"ok": true, "message": "评分已保存，将在同步时写回 Bangumi"}));
    }
    #[cfg(not(feature = "standard"))]
    {
        let _ = (&app, subject_id, rating);
        Ok(bangumi_command_rejected())
    }
}

#[tauri::command]
fn bangumi_auth_status(context: State<'_, AppContext>) -> Result<Value, String> {
    if context.original {
        return Ok(bangumi_auth_status_rejected());
    }
    #[cfg(feature = "standard")]
    {
        let state = context.state.lock().map_err(|_| "状态锁不可用")?;
        return Ok(bangumi_commands::auth_status(
            &state,
            context.bangumi_tokens.as_ref(),
        ));
    }
    #[cfg(not(feature = "standard"))]
    Ok(bangumi_auth_status_rejected())
}

#[tauri::command]
fn bangumi_save_token(context: State<'_, AppContext>, token: String) -> Result<Value, String> {
    if context.original {
        return Ok(bangumi_command_rejected());
    }
    #[cfg(feature = "standard")]
    {
        return Ok(bangumi_commands::save_token(
            context.bangumi_tokens.as_ref(),
            &token,
        ));
    }
    #[cfg(not(feature = "standard"))]
    {
        let _ = token;
        Ok(bangumi_command_rejected())
    }
}

#[tauri::command]
fn bangumi_disconnect(context: State<'_, AppContext>) -> Result<Value, String> {
    if context.original {
        return Ok(bangumi_command_rejected());
    }
    #[cfg(feature = "standard")]
    {
        return Ok(bangumi_commands::disconnect(
            context.bangumi_tokens.as_ref(),
            &context.bangumi_username_cache,
        ));
    }
    #[cfg(not(feature = "standard"))]
    Ok(bangumi_command_rejected())
}

#[tauri::command]
async fn bangumi_test_connection(
    context: State<'_, AppContext>,
    base_url: Option<String>,
) -> Result<Value, String> {
    if context.original {
        return Ok(bangumi_command_rejected());
    }
    #[cfg(feature = "standard")]
    {
        // baseUrl 参数优先（测试连接按钮可先验证未保存的地址），否则按状态解析。
        let base = match base_url
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            Some(configured) => bangumi::resolve_base_urls(configured),
            None => {
                let state = context.state.lock().map_err(|_| "状态锁不可用")?;
                bangumi_base_urls(&state)
            }
        };
        let client = bangumi::HttpBangumiClient::new(context.client.clone(), base);
        return Ok(bangumi_commands::test_connection(
            context.bangumi_tokens.as_ref(),
            &client,
            &context.bangumi_username_cache,
        )
        .await);
    }
    #[cfg(not(feature = "standard"))]
    {
        let _ = base_url;
        Ok(bangumi_command_rejected())
    }
}

#[tauri::command]
async fn bangumi_get_user_profile(context: State<'_, AppContext>) -> Result<Value, String> {
    if context.original {
        return Ok(Value::Null);
    }
    #[cfg(feature = "standard")]
    {
        let base = {
            let state = context.state.lock().map_err(|_| "状态锁不可用")?;
            bangumi_base_urls(&state)
        };
        let client = bangumi::HttpBangumiClient::new(context.client.clone(), base);
        return Ok(bangumi_commands::user_profile(context.bangumi_tokens.as_ref(), &client).await);
    }
    #[cfg(not(feature = "standard"))]
    Ok(Value::Null)
}

#[tauri::command]
async fn bangumi_get_user_collections(
    context: State<'_, AppContext>,
    offset: Option<u32>,
    limit: Option<u32>,
) -> Result<Value, String> {
    if context.original {
        return Ok(json!({
            "ok": false,
            "message": "Original 版不支持 Bangumi",
            "total": 0,
            "items": []
        }));
    }
    #[cfg(feature = "standard")]
    {
        let base = {
            let state = context.state.lock().map_err(|_| "状态锁不可用")?;
            bangumi_base_urls(&state)
        };
        let client = bangumi::HttpBangumiClient::new(context.client.clone(), base);
        return Ok(bangumi_commands::user_collections(
            context.bangumi_tokens.as_ref(),
            &client,
            &context.bangumi_username_cache,
            offset,
            limit,
        )
        .await);
    }
    #[cfg(not(feature = "standard"))]
    {
        let _ = (offset, limit);
        Ok(json!({"total": 0, "items": []}))
    }
}

#[tauri::command]
fn bangumi_set_api_base_url(
    app: AppHandle,
    context: State<'_, AppContext>,
    base_url: String,
) -> Result<Value, String> {
    if context.original {
        return Ok(bangumi_command_rejected());
    }
    #[cfg(feature = "standard")]
    {
        bangumi_commands::set_api_base_url(&context, &base_url)?;
        refresh_mobile_configuration(&app, &context)?;
        emit_state(&app, &context);
        Ok(context.public_state())
    }
    #[cfg(not(feature = "standard"))]
    {
        let _ = (&app, base_url);
        Ok(bangumi_command_rejected())
    }
}

/// Phase 2 任务 3 契约：`bangumi_get_subject_extras({ subjectId })` ->
/// extras JSON | null。original / 无效 id → null（前端按 null 隐藏区块）。
/// 详情惰性 24h 缓存（阻塞刷新、失败回落旧缓存，见 bangumi_commands::subject_extras）。
#[tauri::command]
async fn bangumi_get_subject_extras(
    context: State<'_, AppContext>,
    subject_id: i64,
) -> Result<Value, String> {
    if context.original || subject_id <= 0 {
        return Ok(Value::Null);
    }
    #[cfg(feature = "standard")]
    {
        let base = {
            let state = context.state.lock().map_err(|_| "状态锁不可用")?;
            bangumi_base_urls(&state)
        };
        let client = bangumi::HttpBangumiClient::new(context.client.clone(), base);
        let cache_path = bangumi_cache_dir(&context).join(format!("subject-{subject_id}.json"));
        return Ok(bangumi_commands::subject_extras(
            &client,
            &cache_path,
            subject_id,
            now_seconds(),
        )
        .await);
    }
    #[cfg(not(feature = "standard"))]
    Ok(Value::Null)
}

// ---------------------------------------------------------------------------
// Phase 2 主键迁移命令（任务 3 契约，前端并行开发基准）：
// - bangumi_resolve_mapping({ animeId }) ->
//     { status: "mapped"|"pending"|"unavailable", subjectId: number|null,
//       candidates: [{subjectId, name, nameCn, date, platform?, begin?, score}],
//       anime: {id, displayTitle, seasonYear, format, coverImage} | null }
// - bangumi_confirm_mapping({ animeId, subjectId }) -> AppState（public_state）
// - bangumi_skip_mapping({ animeId }) -> AppState
// original 下全部运行即拒绝（固定文案 "Original 版不支持 Bangumi"）。
// ---------------------------------------------------------------------------

#[tauri::command]
fn bangumi_resolve_mapping(
    _app: AppHandle,
    context: State<'_, AppContext>,
    anime_id: i64,
) -> Result<Value, String> {
    if context.original {
        return Ok(bangumi_command_rejected());
    }
    #[cfg(feature = "standard")]
    {
        let state = context.state.lock().map_err(|_| "状态锁不可用")?;
        return Ok(resolve_mapping_entry(
            &state,
            &context.offline_bangumi,
            anime_id,
        ));
    }
    #[cfg(not(feature = "standard"))]
    {
        let _ = anime_id;
        Ok(bangumi_command_rejected())
    }
}

#[tauri::command]
fn bangumi_confirm_mapping(
    app: AppHandle,
    context: State<'_, AppContext>,
    anime_id: i64,
    subject_id: i64,
) -> Result<Value, String> {
    if context.original {
        return Ok(bangumi_command_rejected());
    }
    #[cfg(feature = "standard")]
    {
        {
            let mut state = context.state.lock().map_err(|_| "状态锁不可用")?;
            let exists = state["following"].as_array().is_some_and(|items| {
                items
                    .iter()
                    .any(|item| value_i64(item.get("id")) == anime_id)
            });
            if !exists {
                return Err("未找到对应追番条目".into());
            }
            // 手动确认走任务 2 契约入口：method="manual"、confidence="high"；
            // 重复 confirm 幂等。
            apply_mapping(&mut state, anime_id, subject_id, true);
        }
        context.save_state().map_err(|error| error.to_string())?;
        context.webdav_wakeup.notify_one();
        refresh_mobile_configuration(&app, &context)?;
        emit_state(&app, &context);
        return Ok(context.public_state());
    }
    #[cfg(not(feature = "standard"))]
    {
        let _ = (&app, anime_id, subject_id);
        Ok(bangumi_command_rejected())
    }
}

#[tauri::command]
fn bangumi_skip_mapping(
    app: AppHandle,
    context: State<'_, AppContext>,
    anime_id: i64,
) -> Result<Value, String> {
    if context.original {
        return Ok(bangumi_command_rejected());
    }
    #[cfg(feature = "standard")]
    {
        {
            let mut state = context.state.lock().map_err(|_| "状态锁不可用")?;
            let exists = state["following"].as_array().is_some_and(|items| {
                items
                    .iter()
                    .any(|item| value_i64(item.get("id")) == anime_id)
            });
            if !exists {
                return Err("未找到对应追番条目".into());
            }
            // 重复 skip 幂等：已处于 low/local 状态时无副作用。
            skip_mapping_entry(&mut state, anime_id);
        }
        context.save_state().map_err(|error| error.to_string())?;
        context.webdav_wakeup.notify_one();
        refresh_mobile_configuration(&app, &context)?;
        emit_state(&app, &context);
        return Ok(context.public_state());
    }
    #[cfg(not(feature = "standard"))]
    {
        let _ = (&app, anime_id);
        Ok(bangumi_command_rejected())
    }
}

#[cfg(not(target_os = "android"))]
fn default_webdav_config() -> Value {
    json!({"supported": true, "enabled": false, "baseUrl": "", "username": "", "hasPassword": false, "lastSyncAt": 0, "lastError": ""})
}

#[cfg(not(target_os = "android"))]
fn webdav_config_path(context: &AppContext) -> PathBuf {
    context.data_dir.join("webdav-tauri.json")
}

#[cfg(not(target_os = "android"))]
fn webdav_config(context: &AppContext) -> Value {
    let mut config = fs::read_to_string(webdav_config_path(context))
        .ok()
        .and_then(|body| serde_json::from_str::<Value>(&body).ok())
        .unwrap_or_else(default_webdav_config);
    config["supported"] = json!(cfg!(target_os = "windows"));
    config["hasPassword"] = json!(load_webdav_password().is_ok());
    config
}

#[cfg(not(target_os = "android"))]
fn persist_webdav_config(context: &AppContext, config: &Value) -> anyhow::Result<()> {
    let mut private = config.clone();
    private
        .as_object_mut()
        .map(|object| object.remove("hasPassword"));
    let temporary = webdav_config_path(context).with_extension("json.tmp");
    fs::write(&temporary, serde_json::to_vec_pretty(&private)?)?;
    fs::rename(temporary, webdav_config_path(context))?;
    Ok(())
}

#[cfg(target_os = "windows")]
fn dpapi_unprotect(mut encrypted: Vec<u8>) -> anyhow::Result<Vec<u8>> {
    use windows::Win32::Foundation::{HLOCAL, LocalFree};
    use windows::Win32::Security::Cryptography::{CRYPT_INTEGER_BLOB, CryptUnprotectData};

    let input = CRYPT_INTEGER_BLOB {
        cbData: encrypted
            .len()
            .try_into()
            .context("DPAPI input is too large")?,
        pbData: encrypted.as_mut_ptr(),
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    unsafe {
        CryptUnprotectData(&input, None, None, None, None, 0, &mut output)
            .context("Windows DPAPI could not decrypt the legacy value")?;
        let decrypted = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
        let _ = LocalFree(Some(HLOCAL(output.pbData.cast())));
        Ok(decrypted)
    }
}

#[cfg(target_os = "windows")]
fn legacy_electron_encryption_key(data_dir: &Path) -> anyhow::Result<Vec<u8>> {
    let local_state_paths = [
        data_dir.join("Local State"),
        data_dir.join("Session Data").join("Local State"),
    ];
    for path in local_state_paths {
        let Some(encoded) = fs::read_to_string(&path)
            .ok()
            .and_then(|body| serde_json::from_str::<Value>(&body).ok())
            .and_then(|value| {
                value
                    .pointer("/os_crypt/encrypted_key")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
        else {
            continue;
        };
        let encrypted = BASE64_STANDARD
            .decode(encoded)
            .context("legacy Electron encryption key is not valid Base64")?;
        let protected = encrypted
            .strip_prefix(b"DPAPI")
            .context("legacy Electron encryption key has an unsupported format")?;
        return dpapi_unprotect(protected.to_vec());
    }
    Err(anyhow!("legacy Electron Local State was not found"))
}

#[cfg(target_os = "windows")]
fn decrypt_legacy_webdav_password(data_dir: &Path, encoded: &str) -> anyhow::Result<String> {
    let encrypted = BASE64_STANDARD
        .decode(encoded)
        .context("legacy WebDAV password is not valid Base64")?;
    let decrypted = if let Some(payload) = encrypted.strip_prefix(b"v10") {
        if payload.len() < 12 + 16 {
            return Err(anyhow!("legacy WebDAV password is truncated"));
        }
        let key = legacy_electron_encryption_key(data_dir)?;
        let cipher = Aes256Gcm::new_from_slice(&key)
            .map_err(|_| anyhow!("legacy Electron encryption key has an invalid length"))?;
        cipher
            .decrypt(aes_gcm::Nonce::from_slice(&payload[..12]), &payload[12..])
            .map_err(|_| anyhow!("legacy WebDAV password authentication failed"))?
    } else {
        dpapi_unprotect(encrypted)?
    };
    String::from_utf8(decrypted).context("legacy WebDAV password is not valid UTF-8")
}

#[cfg(all(not(target_os = "android"), not(target_os = "windows")))]
fn decrypt_legacy_webdav_password(_data_dir: &Path, _encoded: &str) -> anyhow::Result<String> {
    Err(anyhow!(
        "legacy WebDAV password migration is only available on Windows"
    ))
}

#[cfg(not(target_os = "android"))]
fn migrate_legacy_webdav_config(context: &AppContext) -> anyhow::Result<bool> {
    if let Some(current) = fs::read_to_string(webdav_config_path(context))
        .ok()
        .and_then(|body| serde_json::from_str::<Value>(&body).ok())
    {
        let migration_completed = value_bool(current.get("legacyMigrationCompleted"));
        let already_configured = !value_string(current.get("baseUrl")).is_empty()
            || !value_string(current.get("username")).is_empty();
        if migration_completed || already_configured {
            return Ok(false);
        }
    }
    let legacy_path = context.data_dir.join("webdav-config.json");
    if !legacy_path.exists() {
        return Ok(false);
    }
    let legacy: Value = serde_json::from_slice(
        &fs::read(&legacy_path).context("read legacy WebDAV configuration")?,
    )
    .context("parse legacy WebDAV configuration")?;
    let raw_base = value_string(legacy.get("baseUrl"));
    let base_url = if raw_base.is_empty() {
        String::new()
    } else {
        normalize_url(&raw_base, None).context("normalize legacy WebDAV address")?
    };
    let username = value_string(legacy.get("username")).trim().to_string();
    let encrypted_password = value_string(legacy.get("encryptedPassword"));
    let mut migration_error = String::new();
    let has_password = if encrypted_password.is_empty() {
        false
    } else {
        match decrypt_legacy_webdav_password(&context.data_dir, &encrypted_password)
            .and_then(|password| store_webdav_password(&password))
        {
            Ok(()) => true,
            Err(error) => {
                migration_error = format!("旧版 WebDAV 密码未能迁移，请重新输入密码：{error}");
                false
            }
        }
    };
    let enabled = value_bool(legacy.get("enabled"))
        && !base_url.is_empty()
        && !username.is_empty()
        && has_password;
    let config = json!({
        "supported": true,
        "enabled": enabled,
        "baseUrl": base_url,
        "username": username,
        "hasPassword": has_password,
        "lastSyncAt": value_i64(legacy.get("lastSyncAt")),
        "lastError": if migration_error.is_empty() { value_string(legacy.get("lastError")) } else { migration_error },
        "legacyMigrationCompleted": true,
    });
    persist_webdav_config(context, &config)?;
    info!("migrated legacy Electron WebDAV configuration");
    Ok(true)
}

#[cfg(all(not(target_os = "android"), target_os = "windows"))]
fn credential_entry() -> anyhow::Result<keyring::Entry> {
    Ok(keyring::Entry::new(WEBDAV_CREDENTIAL_SERVICE, "default")?)
}
#[cfg(all(not(target_os = "android"), target_os = "windows"))]
fn load_webdav_password() -> anyhow::Result<String> {
    Ok(credential_entry()?.get_password()?)
}
#[cfg(all(not(target_os = "android"), target_os = "windows"))]
fn store_webdav_password(password: &str) -> anyhow::Result<()> {
    credential_entry()?.set_password(password)?;
    Ok(())
}
#[cfg(all(not(target_os = "android"), not(target_os = "windows")))]
fn load_webdav_password() -> anyhow::Result<String> {
    Err(anyhow!("当前平台的安全凭据存储尚未接入"))
}
#[cfg(all(not(target_os = "android"), not(target_os = "windows")))]
fn store_webdav_password(_password: &str) -> anyhow::Result<()> {
    Err(anyhow!("当前平台的安全凭据存储尚未接入"))
}

#[cfg(not(target_os = "android"))]
fn webdav_endpoint(config: &Value, name: &str) -> anyhow::Result<String> {
    let base = url::Url::parse(&value_string(config.get("baseUrl")))?;
    Ok(base
        .join(&format!("{WEBDAV_COLLECTION}/{name}"))?
        .to_string())
}

#[cfg(not(target_os = "android"))]
async fn webdav_request(
    context: &AppContext,
    config: &Value,
    method: reqwest::Method,
    url: String,
    body: Option<String>,
    etag: Option<&str>,
    new_file: bool,
) -> anyhow::Result<reqwest::Response> {
    let password = load_webdav_password().context("WebDAV 密码无法读取，请重新输入密码")?;
    let mut request = context
        .client
        .request(method, url)
        .basic_auth(value_string(config.get("username")), Some(password))
        .header(USER_AGENT, "AniLog Tauri WebDAV sync");
    if let Some(body) = body {
        request = request
            .header(CONTENT_TYPE, "application/json; charset=utf-8")
            .body(body);
    }
    if let Some(etag) = etag.filter(|value| !value.is_empty()) {
        request = request.header(IF_MATCH, etag);
    } else if new_file {
        request = request.header(IF_NONE_MATCH, "*");
    }
    Ok(request
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await?)
}

#[cfg(not(target_os = "android"))]
async fn ensure_webdav_collection(context: &AppContext, config: &Value) -> anyhow::Result<()> {
    let response = webdav_request(
        context,
        config,
        reqwest::Method::from_bytes(b"MKCOL")?,
        webdav_endpoint(config, "")?,
        None,
        None,
        false,
    )
    .await?;
    if response.status().is_success() || response.status().as_u16() == 405 {
        Ok(())
    } else if matches!(response.status().as_u16(), 401 | 403) {
        Err(anyhow!("WebDAV 认证失败，请检查账号和应用密码"))
    } else {
        Err(anyhow!(
            "无法创建 AniLog 同步目录（HTTP {}）",
            response.status()
        ))
    }
}

#[cfg(not(target_os = "android"))]
async fn download_webdav_document(
    context: &AppContext,
    config: &Value,
) -> anyhow::Result<(bool, String, Option<Value>)> {
    let response = webdav_request(
        context,
        config,
        reqwest::Method::GET,
        webdav_endpoint(config, WEBDAV_DOCUMENT)?,
        None,
        None,
        false,
    )
    .await?;
    if response.status().as_u16() == 404 {
        return Ok((false, String::new(), None));
    }
    if matches!(response.status().as_u16(), 401 | 403) {
        return Err(anyhow!("WebDAV 认证失败，请检查账号和应用密码"));
    }
    if !response.status().is_success() {
        return Err(anyhow!(
            "读取 WebDAV 同步文件失败（HTTP {}）",
            response.status()
        ));
    }
    if response.content_length().unwrap_or_default() as usize > MAX_SYNC_BYTES {
        return Err(anyhow!("WebDAV 同步文件超过 5 MB，已停止读取"));
    }
    let etag = response
        .headers()
        .get(ETAG)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let bytes = response.bytes().await?;
    if bytes.len() > MAX_SYNC_BYTES {
        return Err(anyhow!("WebDAV 同步文件超过 5 MB，已停止读取"));
    }
    let document =
        serde_json::from_slice::<Value>(&bytes).context("WebDAV 同步文件不是有效的 JSON")?;
    Ok((true, etag, Some(normalize_document(&document)?)))
}

#[cfg(not(target_os = "android"))]
async fn perform_webdav_sync(app: &AppHandle, context: &AppContext) -> anyhow::Result<Value> {
    let mut config = webdav_config(context);
    if !value_bool(config.get("enabled")) {
        return Err(anyhow!("请先启用 WebDAV 同步"));
    }
    if load_webdav_password().is_err() {
        return Err(anyhow!("请重新输入 WebDAV 应用密码"));
    }
    ensure_webdav_collection(context, &config).await?;
    let mut local_changed = false;
    for attempt in 0..3 {
        let (found, etag, remote) = download_webdav_document(context, &config).await?;
        let (merged, remote_changed) = {
            let mut state = context.state.lock().map_err(|_| anyhow!("状态锁不可用"))?;
            if let Some(remote) = &remote {
                let (changed, merged, remote_changed) =
                    merge_document_into_state(&mut state, remote)?;
                local_changed |= changed;
                // 问题 B：WebDAV 合并进来的旧 AniList 键记录即时跨键合并/
                // 映射（自动映射只在启动跑一次，远端合并不再处理会导致同一部
                // 番两条追番并存）。合并若产生变更，重算上传文档与 remote 变更
                // 标记（墓碑等结果随本轮回写坚果云）。
                #[cfg(feature = "standard")]
                let (merged, remote_changed) = {
                    let mut merged = merged;
                    let mut remote_changed = remote_changed;
                    if reconcile_following_entries(
                        &mut state,
                        &context.offline_bangumi,
                        context.original,
                    ) {
                        local_changed = true;
                        merged = document_from_state(&mut state);
                        remote_changed =
                            comparable_document(remote)? != comparable_document(&merged)?;
                    }
                    // 第 8 轮问题 1（桌面止血）：云端合并 + reconcile 之后、构建
                    // 上传文档之前，套用缓存的 AniList 权威数据（TTL 30 分钟内
                    // 不触网）——云端复活的小时级假票当次上传前再死一次，上传
                    // 文档自愈；无/过期缓存 → false 跳过（权威纠正由 sync_now
                    // 重新抓取承担）。
                    if apply_cached_anilist_authority_with_mode(
                        &mut state,
                        &context.offline_bangumi,
                        &bangumi_cache_dir(context),
                        now_seconds(),
                        true,
                    ) {
                        local_changed = true;
                        merged = document_from_state(&mut state);
                        remote_changed =
                            comparable_document(remote)? != comparable_document(&merged)?;
                    }
                    // Standard 的逐集 Bangumi 表是最终播出权威。AniList 上游
                    // 可能暂时 403，且云端仍可能保存旧的未来假票；上传前用本地
                    // episode 缓存再做一次不触网愈合，防止错误任务跨设备复活。
                    #[cfg(feature = "standard")]
                    if apply_cached_bangumi_episode_authority(
                        &mut state,
                        &bangumi_cache_dir(context),
                        now_seconds(),
                    ) {
                        local_changed = true;
                        merged = document_from_state(&mut state);
                        remote_changed = comparable_document(remote).unwrap_or_default()
                            != comparable_document(&merged).unwrap_or_default();
                    }
                    (merged, remote_changed)
                };
                (merged, remote_changed)
            } else {
                (document_from_state(&mut state), true)
            }
        };
        if local_changed {
            context.save_state()?;
            emit_state(app, context);
        }
        if !remote_changed
            || remote.as_ref().is_some_and(|remote| {
                comparable_document(remote).ok() == comparable_document(&merged).ok()
            })
        {
            break;
        }
        let response = webdav_request(
            context,
            &config,
            reqwest::Method::PUT,
            webdav_endpoint(&config, WEBDAV_DOCUMENT)?,
            Some(serde_json::to_string_pretty(&merged)?),
            found.then_some(etag.as_str()),
            !found,
        )
        .await?;
        if response.status().is_success() {
            break;
        }
        if matches!(response.status().as_u16(), 409 | 412) {
            if attempt == 2 {
                return Err(anyhow!("WebDAV 文件在同步期间反复变化，请稍后重试"));
            }
            continue;
        }
        return Err(anyhow!(
            "写入 WebDAV 同步文件失败（HTTP {}）",
            response.status()
        ));
    }
    config["lastSyncAt"] = json!(now_seconds());
    config["lastError"] = json!("");
    persist_webdav_config(context, &config)?;
    Ok(
        json!({"ok": true, "changed": local_changed, "syncedAt": config["lastSyncAt"], "message": if local_changed { "已合并另一台设备的更新" } else { "两端数据已同步" }}),
    )
}

#[tauri::command]
fn get_webdav_config(_app: AppHandle, context: State<'_, AppContext>) -> Result<Value, String> {
    #[cfg(target_os = "android")]
    {
        let _ = &context;
        return mobile::get_webdav_config(&_app).map_err(|error| error.to_string());
    }
    #[cfg(not(target_os = "android"))]
    Ok(webdav_config(&context))
}

#[tauri::command]
fn save_webdav_config(
    _app: AppHandle,
    context: State<'_, AppContext>,
    config: Value,
) -> Result<Value, String> {
    #[cfg(target_os = "android")]
    {
        let mut config = config;
        config["baseUrl"] = json!(
            normalize_url(&value_string(config.get("baseUrl")), None)
                .map_err(|error| error.to_string())?
        );
        let current = mobile::get_webdav_config(&_app).map_err(|error| error.to_string())?;
        let has_password = !value_string(config.get("password")).is_empty()
            || value_bool(current.get("hasPassword"));
        validate_webdav_config(
            value_bool(config.get("enabled")),
            &value_string(config.get("baseUrl")),
            &value_string(config.get("username")),
            has_password,
        )
        .map_err(|error| error.to_string())?;
        let saved =
            mobile::save_webdav_config(&_app, &config).map_err(|error| error.to_string())?;
        if value_bool(saved.get("enabled")) {
            context.webdav_wakeup.notify_one();
        }
        return Ok(saved);
    }
    #[cfg(not(target_os = "android"))]
    {
        let base = normalize_url(&value_string(config.get("baseUrl")), None)
            .map_err(|error| error.to_string())?;
        let username = value_string(config.get("username"));
        let enabled = value_bool(config.get("enabled"));
        let old = webdav_config(&context);
        let password = value_string(config.get("password"));
        let has_password = !password.is_empty() || value_bool(old.get("hasPassword"));
        validate_webdav_config(enabled, &base, &username, has_password)
            .map_err(|error| error.to_string())?;
        let next = json!({"supported": true, "enabled": enabled, "baseUrl": base, "username": username, "hasPassword": has_password, "lastSyncAt": value_i64(old.get("lastSyncAt")), "lastError": "", "legacyMigrationCompleted": value_bool(old.get("legacyMigrationCompleted"))});
        if !password.is_empty() {
            store_webdav_password(&password).map_err(|error| error.to_string())?;
        }
        persist_webdav_config(&context, &next).map_err(|error| error.to_string())?;
        if enabled {
            context.webdav_wakeup.notify_one();
        }
        Ok(webdav_config(&context))
    }
}

#[tauri::command]
async fn test_webdav_connection(
    _app: AppHandle,
    context: State<'_, AppContext>,
) -> Result<Value, String> {
    #[cfg(target_os = "android")]
    {
        let _ = &context;
        return mobile::test_webdav_connection(&_app).map_err(|error| error.to_string());
    }
    #[cfg(not(target_os = "android"))]
    {
        let config = webdav_config(&context);
        let mut response = webdav_request(&context, &config, reqwest::Method::from_bytes(b"PROPFIND").map_err(|error| error.to_string())?, value_string(config.get("baseUrl")), Some("<?xml version=\"1.0\"?><propfind xmlns=\"DAV:\"><prop><resourcetype/></prop></propfind>".into()), None, false).await.map_err(|error| error.to_string())?;
        if matches!(response.status().as_u16(), 405 | 501) {
            response = webdav_request(
                &context,
                &config,
                reqwest::Method::GET,
                value_string(config.get("baseUrl")),
                None,
                None,
                false,
            )
            .await
            .map_err(|error| error.to_string())?;
        }
        if matches!(response.status().as_u16(), 401 | 403) {
            return Err("WebDAV 认证失败，请检查账号和应用密码".into());
        }
        if !response.status().is_success() && response.status().as_u16() != 207 {
            return Err(format!("WebDAV 连接失败（HTTP {}）", response.status()));
        }
        Ok(json!({"ok": true, "message": "WebDAV 连接成功"}))
    }
}

#[tauri::command]
async fn sync_webdav(app: AppHandle, context: State<'_, AppContext>) -> Result<Value, String> {
    match perform_platform_webdav_sync(&app, &context).await {
        Ok(value) => Ok(value),
        Err(error) => {
            #[cfg(not(target_os = "android"))]
            {
                let mut config = webdav_config(&context);
                config["lastError"] = json!(error.to_string());
                let _ = persist_webdav_config(&context, &config);
            }
            Err(error.to_string())
        }
    }
}

async fn perform_platform_webdav_sync(
    app: &AppHandle,
    context: &AppContext,
) -> anyhow::Result<Value> {
    let _guard = context.webdav_sync_lock.lock().await;
    #[cfg(target_os = "android")]
    return mobile::sync_webdav(app, context);
    #[cfg(not(target_os = "android"))]
    perform_webdav_sync(app, context).await
}

/// 问题 3（同步节奏）：后台 tick 的完整同步 + `bangumiSyncStatus.lastWebDavSyncAt`
/// 记账。完整同步循环（下载→merge→reconcile→上传，无变化时上传自动跳过）
/// 本就由 [`perform_platform_webdav_sync`] 承担；此前缺的是记账——该时间戳
/// 只在 run_full_bangumi_sync 步骤 1 写入，后台 15 分钟空闲 tick 即使完整
/// 执行了同步，UI"上次坚果云同步"也最长停滞一个 Bangumi 周期。现在后台
/// 循环每次 tick（动作唤醒或空闲超时）成功后即记账并保存/广播本地-only
/// 状态（绝不进坚果云文档）。锁语义不变：仍经
/// [`perform_platform_webdav_sync`] 的 webdav_sync_lock 与手动同步互斥。
/// original 不编译记账段（无 bangumiSyncStatus 块），行为不变。
async fn perform_webdav_sync_tracked(
    app: &AppHandle,
    context: &AppContext,
) -> anyhow::Result<Value> {
    let result = perform_platform_webdav_sync(app, context).await;
    if result.is_ok() {
        #[cfg(feature = "standard")]
        match context.state.lock() {
            Ok(mut state) => {
                merge_bangumi_sync_status(
                    &mut state,
                    bangumi::BangumiSyncStatus {
                        last_web_dav_sync_at: Some(now_seconds()),
                        ..bangumi::BangumiSyncStatus::default()
                    },
                );
                drop(state);
                if let Err(error) = context.save_state() {
                    warn!("failed to persist lastWebDavSyncAt: {error}");
                }
                emit_state(app, context);
            }
            Err(_) => warn!("background WebDAV sync: 状态锁不可用，跳过 lastWebDavSyncAt 记账"),
        }
        #[cfg(not(feature = "standard"))]
        let _ = app;
    }
    result
}

fn webdav_is_enabled(app: &AppHandle, context: &AppContext) -> bool {
    #[cfg(target_os = "android")]
    {
        let _ = context;
        return mobile::get_webdav_config(app)
            .ok()
            .is_some_and(|config| value_bool(config.get("enabled")));
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = app;
        value_bool(webdav_config(context).get("enabled"))
    }
}

fn start_webdav_background(app: AppHandle, context: AppContext) {
    tauri::async_runtime::spawn(async move {
        let mut startup = true;
        loop {
            let delay = if startup {
                std::time::Duration::from_secs(8)
            } else {
                std::time::Duration::from_secs(15 * 60)
            };
            let changed = tokio::time::timeout(delay, context.webdav_wakeup.notified())
                .await
                .is_ok();
            if changed {
                while tokio::time::timeout(
                    std::time::Duration::from_secs(5),
                    context.webdav_wakeup.notified(),
                )
                .await
                .is_ok()
                {}
            }
            if webdav_is_enabled(&app, &context) {
                // 问题 3：空闲 tick 与唤醒 tick 一律走完整同步循环（下载→
                // merge→reconcile→上传；无变化时上传自动跳过），成功后记账
                // lastWebDavSyncAt 真实前进；不与手动同步并发的锁语义不变。
                if let Err(error) = perform_webdav_sync_tracked(&app, &context).await {
                    warn!("background WebDAV sync failed: {error}");
                }
            }
            startup = false;
        }
    });
}

#[tauri::command]
fn request_exact_scheduling(_app: AppHandle) -> Result<(), String> {
    #[cfg(target_os = "android")]
    mobile::request_exact_scheduling(&_app).map_err(|error| error.to_string())?;
    Ok(())
}

#[cfg(desktop)]
fn show_main_window(app: &AppHandle) -> anyhow::Result<()> {
    let window = if let Some(window) = app.get_webview_window("main") {
        window
    } else {
        let config = app
            .config()
            .app
            .windows
            .iter()
            .find(|config| config.label == "main")
            .context("main window configuration is unavailable")?;
        let builder = tauri::WebviewWindowBuilder::from_config(app, config)?;
        #[cfg(target_os = "windows")]
        let builder =
            builder.data_directory(app.state::<AppContext>().data_dir.join("webview-data"));
        builder.build()?
    };
    if window.is_minimized()? {
        window.unminimize()?;
    }
    window.show()?;
    window.set_focus()?;
    Ok(())
}

#[cfg(desktop)]
fn request_show_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
        return;
    }
    let Some(context) = app.try_state::<AppContext>() else {
        return;
    };
    let opening = context.main_window_opening.clone();
    if opening
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return;
    }
    let app = app.clone();
    std::thread::spawn(move || {
        if let Err(error) = show_main_window(&app) {
            warn!("failed to recreate AniLog window: {error}");
        }
        opening.store(false, Ordering::Release);
    });
}

#[cfg(target_os = "windows")]
fn windows_notification_app_id(identifier: &str, exe_dir: &Path) -> String {
    let target_debug = Path::new("target").join("debug");
    let target_release = Path::new("target").join("release");
    if exe_dir.ends_with(target_debug) || exe_dir.ends_with(target_release) {
        tauri_winrt_notification::Toast::POWERSHELL_APP_ID.to_string()
    } else {
        identifier.to_string()
    }
}

#[cfg(target_os = "windows")]
fn show_desktop_notification(app: &AppHandle, title: String, body: String) {
    use tauri_winrt_notification::{Duration, Toast};

    let identifier = app.config().identifier.clone();
    let app_id = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
        .map(|dir| windows_notification_app_id(&identifier, &dir))
        .unwrap_or(identifier);
    let activation_app = app.clone();
    std::thread::spawn(move || {
        let toast = Toast::new(&app_id)
            .title(&title)
            .text2(&body)
            .duration(Duration::Short)
            .sound(None)
            .on_activated(move |_| {
                let app = activation_app.clone();
                let callback_app = app.clone();
                let _ = app.run_on_main_thread(move || request_show_main_window(&callback_app));
                Ok(())
            });
        if let Err(error) = toast.show() {
            warn!("failed to show Windows notification: {error}");
        }
    });
}

#[cfg(all(desktop, not(target_os = "windows")))]
fn show_desktop_notification(app: &AppHandle, title: String, body: String) {
    let _ = app.notification().builder().title(title).body(body).show();
}

#[cfg(desktop)]
#[derive(Debug, PartialEq)]
struct TrayLabels {
    open: String,
    sync: String,
    quit: String,
    tooltip: String,
}

#[cfg(desktop)]
fn tray_labels(original: bool, language: &str) -> TrayLabels {
    let english = language == "en-US";
    let product_name = if original {
        "AniLog Original"
    } else {
        "AniLog"
    };
    TrayLabels {
        open: if english {
            format!("Open {product_name}")
        } else {
            format!("打开 {product_name}")
        },
        sync: if english { "Sync now" } else { "立即同步" }.into(),
        quit: if english { "Quit" } else { "退出" }.into(),
        tooltip: if english {
            format!("{product_name} - Anime tracker")
        } else {
            format!("{product_name} - 追番任务")
        },
    }
}

#[cfg(desktop)]
fn setup_tray(app: &AppHandle, context: &AppContext) -> anyhow::Result<()> {
    use tauri::menu::{MenuBuilder, MenuItemBuilder, PredefinedMenuItem};
    use tauri::tray::TrayIconBuilder;
    let (language, visible) = context
        .state
        .lock()
        .ok()
        .map(|state| {
            (
                value_string(state["settings"].get("uiLanguage")),
                show_tray_icon(&state["settings"]),
            )
        })
        .unwrap_or_else(|| ("zh-CN".into(), true));
    let labels = tray_labels(context.original, &language);
    let open = MenuItemBuilder::with_id("open", labels.open.clone()).build(app)?;
    let sync = MenuItemBuilder::with_id("sync", labels.sync.clone()).build(app)?;
    let quit = MenuItemBuilder::with_id("quit", labels.quit.clone()).build(app)?;
    let separator = PredefinedMenuItem::separator(app)?;
    let menu = MenuBuilder::new(app)
        .items(&[&open, &sync, &separator, &quit])
        .build()?;
    let icon = tauri::image::Image::from_bytes(include_bytes!("../../assets/tray.png"))?;
    let _ = app.remove_tray_by_id("main");
    let tray = TrayIconBuilder::with_id("main")
        .icon(icon)
        .menu(&menu)
        .show_menu_on_left_click(false)
        .tooltip(labels.tooltip)
        .on_tray_icon_event(|tray, event| {
            if let tauri::tray::TrayIconEvent::Click {
                button: tauri::tray::MouseButton::Left,
                button_state: tauri::tray::MouseButtonState::Up,
                ..
            } = event
            {
                request_show_main_window(tray.app_handle());
            }
        })
        .on_menu_event(|app, event| match event.id().as_ref() {
            "open" => {
                request_show_main_window(app);
            }
            "sync" => {
                let app = app.clone();
                let context = app.state::<AppContext>().inner().clone();
                tauri::async_runtime::spawn(async move {
                    if let Err(error) = sync_now_inner(&app, &context).await {
                        warn!("tray AniList sync failed: {error}");
                    }
                });
            }
            "quit" => app.exit(0),
            _ => {}
        })
        .build(app)?;
    tray.set_visible(visible)?;
    Ok(())
}

#[cfg(desktop)]
fn show_tray_icon(settings: &Value) -> bool {
    settings
        .get("showTrayIcon")
        .and_then(Value::as_bool)
        .unwrap_or(true)
}

#[cfg(desktop)]
fn reconcile_autostart(app: &AppHandle, enabled: bool) -> anyhow::Result<()> {
    let autostart = app.autolaunch();
    if autostart.is_enabled()? == enabled {
        return Ok(());
    }
    if enabled {
        autostart.enable()?;
    } else {
        autostart.disable()?;
    }
    Ok(())
}

#[cfg(desktop)]
fn start_desktop_background(app: AppHandle, context: AppContext) {
    tauri::async_runtime::spawn(async move {
        loop {
            if let Err(error) = sync_now_inner(&app, &context).await {
                warn!("background AniList sync failed: {error}");
            }
            loop {
                let minutes = {
                    let state = context.state.lock().ok();
                    state
                        .as_ref()
                        .map(|state| {
                            value_i64(state["settings"].get("pollIntervalMinutes")).clamp(1, 1440)
                        })
                        .unwrap_or(5)
                };
                if tokio::time::timeout(
                    std::time::Duration::from_secs((minutes * 60) as u64),
                    context.sync_wakeup.notified(),
                )
                .await
                .is_err()
                {
                    break;
                }
            }
        }
    });
}

#[cfg(desktop)]
fn claim_daily_task_reminder(
    state: &mut Value,
    today: &str,
    current_time: &str,
) -> Option<(usize, String)> {
    let reminder_time = value_string(state["settings"].get("dailyTaskReminderTime"));
    if !value_bool(state["settings"].get("dailyTaskReminderEnabled"))
        || value_string(state.get("lastTaskReminderDate")) == today
        || current_time < reminder_time.as_str()
    {
        return None;
    }
    let pending = state["tasks"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|task| value_string(task.get("status")) == "pending")
        .count();
    if pending == 0 {
        return None;
    }
    state["lastTaskReminderDate"] = json!(today);
    Some((pending, value_string(state["settings"].get("uiLanguage"))))
}

#[cfg(desktop)]
fn send_daily_task_reminder_if_due(app: &AppHandle, context: &AppContext) -> anyhow::Result<bool> {
    let now = Local::now();
    let today = now.format("%Y-%m-%d").to_string();
    let current_time = now.format("%H:%M").to_string();
    let Some((pending, language)) = ({
        let mut state = context.state.lock().map_err(|_| anyhow!("状态锁不可用"))?;
        claim_daily_task_reminder(&mut state, &today, &current_time)
    }) else {
        return Ok(false);
    };
    context.save_state()?;
    emit_state(app, context);
    let (title, body) = if language == "en-US" {
        (
            "Watch tasks reminder".to_string(),
            format!("You still have {pending} episode(s) to watch."),
        )
    } else {
        (
            "待看任务提醒".to_string(),
            format!("你还有 {pending} 个待看任务尚未完成。"),
        )
    };
    show_desktop_notification(app, title, body);
    Ok(true)
}

#[cfg(desktop)]
fn start_desktop_task_reminders(app: AppHandle, context: AppContext) {
    tauri::async_runtime::spawn(async move {
        loop {
            if let Err(error) = send_daily_task_reminder_if_due(&app, &context) {
                warn!("daily task reminder failed: {error}");
            }
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
        }
    });
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let original = cfg!(feature = "original");
    #[cfg(desktop)]
    let start_hidden = std::env::args().any(|argument| argument == "--hidden");
    let builder = tauri::Builder::default();
    #[cfg(target_os = "windows")]
    let builder = builder.plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
        PENDING_WINDOW_ACTIVATION.store(true, Ordering::Release);
        request_show_main_window(app);
    }));
    let builder = builder
        .plugin(tauri_plugin_log::Builder::default().build())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init());
    #[cfg(desktop)]
    let builder = builder.plugin(tauri_plugin_autostart::init(
        tauri_plugin_autostart::MacosLauncher::LaunchAgent,
        Some(vec!["--hidden"]),
    ));
    #[cfg(target_os = "android")]
    let builder = builder.plugin(mobile::init());
    builder
        .setup(move |app| {
            let context = load_context(app.handle(), original)?;
            app.manage(context.clone());
            #[cfg(target_os = "android")]
            {
                mobile::import_legacy_state(app.handle(), &context)?;
                mobile::configure(app.handle(), &context)?;
                mobile::consume_events(app.handle(), &context)?;
            }
            // Phase 4 任务 1：Android 前台过期同步补偿（setup 完成后异步执行，
            // 仅 standard edition；Windows 桌面启动路径零变化）。
            #[cfg(all(feature = "standard", target_os = "android"))]
            maybe_spawn_foreground_sync(app.handle(), &context);
            // 问题 2b 跨平台化：Android 同样挂载自动 Bangumi 同步循环（追番/
            // 评分/完成任务后约 1 分钟内写回，此前只有前台 15 分钟过期补偿）。
            // 循环只在进程存活期间运行、随进程死亡：60 分钟周期 + 动作唤醒
            // （30 秒静默合并），不要求常驻后台、不是高频轮询。
            #[cfg(all(feature = "standard", target_os = "android"))]
            start_bangumi_sync_loop(app.handle().clone(), context.clone());
            #[cfg(desktop)]
            {
                tauri::window::WindowBuilder::new(app, "background")
                    .visible(false)
                    .skip_taskbar(true)
                    .build()?;
                setup_tray(app.handle(), &context)?;
                let launch_at_login = context
                    .state
                    .lock()
                    .ok()
                    .map(|state| value_bool(state["settings"].get("launchAtLogin")))
                    .unwrap_or(false);
                if let Err(error) = reconcile_autostart(app.handle(), launch_at_login) {
                    warn!("failed to reconcile autostart setting: {error}");
                }
                start_desktop_background(app.handle().clone(), context.clone());
                // 问题 2b：桌面挂载自动 Bangumi 同步循环（max(pollIntervalMinutes,
                // 15) 分钟周期 + 动作唤醒；standard 挂载点，Android 挂载见上方
                // Android 分支）。
                #[cfg(feature = "standard")]
                start_bangumi_sync_loop(app.handle().clone(), context.clone());
                start_desktop_task_reminders(app.handle().clone(), context.clone());
                if !start_hidden {
                    show_main_window(app.handle())?;
                } else if let Some(window) = app.get_webview_window("main") {
                    window.destroy()?;
                }
                if PENDING_WINDOW_ACTIVATION.swap(false, Ordering::AcqRel) {
                    request_show_main_window(app.handle());
                }
            }
            start_webdav_background(app.handle().clone(), context.clone());
            info!(
                "AniLog Tauri started ({})",
                if original { "original" } else { "standard" }
            );
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_state,
            fetch_season,
            toggle_follow,
            update_follow_title,
            resolve_bangumi_title,
            test_bangumi_connection,
            bangumi_auth_status,
            bangumi_save_token,
            bangumi_disconnect,
            bangumi_test_connection,
            bangumi_get_user_profile,
            bangumi_get_user_collections,
            bangumi_set_api_base_url,
            bangumi_resolve_mapping,
            bangumi_confirm_mapping,
            bangumi_skip_mapping,
            bangumi_get_subject_extras,
            bangumi_sync_now,
            bangumi_update_sync_settings,
            bangumi_set_rating,
            bangumi_set_collection_status,
            toggle_task,
            update_settings,
            sync_now,
            get_cache_info,
            clear_cache,
            get_webdav_config,
            save_webdav_config,
            test_webdav_connection,
            sync_webdav,
            request_exact_scheduling,
            open_external
        ])
        .on_window_event(|window, event| {
            #[cfg(not(desktop))]
            let _ = (&window, &event);
            #[cfg(desktop)]
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                let keep_in_tray = window
                    .app_handle()
                    .state::<AppContext>()
                    .state
                    .lock()
                    .ok()
                    .map(|state| value_bool(state["settings"].get("minimizeToTray")))
                    .unwrap_or(true);
                if keep_in_tray {
                    api.prevent_close();
                    let _ = window.destroy();
                } else {
                    api.prevent_close();
                    window.app_handle().exit(0);
                }
            }
            #[cfg(desktop)]
            if matches!(event, tauri::WindowEvent::Resized(_))
                && window.is_minimized().unwrap_or(false)
            {
                let keep_in_tray = window
                    .app_handle()
                    .state::<AppContext>()
                    .state
                    .lock()
                    .ok()
                    .map(|state| value_bool(state["settings"].get("minimizeToTray")))
                    .unwrap_or(true);
                if keep_in_tray {
                    let _ = window.destroy();
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running AniLog Tauri application");
}

#[cfg(test)]
mod tests {
    use super::*;
    // Phase 3 测试：MemoryTokenStore 的 store/load 方法来自该 trait。
    #[cfg(feature = "standard")]
    use crate::bangumi::BangumiTokenStore as _;

    fn followed(id: i64, updated_at: i64) -> Value {
        json!({
            "id": id,
            "title": {"romaji": format!("Anime {id}"), "english": null, "native": null},
            "displayTitle": format!("Anime {id}"),
            "followedAt": 1,
            "syncUpdatedAt": updated_at
        })
    }

    fn task(id: &str, anime_id: i64, status: &str, updated_at: i64) -> Value {
        json!({
            "id": id,
            "animeId": anime_id,
            "animeTitle": format!("Anime {anime_id}"),
            "episode": 1,
            "airingAt": 10,
            "status": status,
            "createdAt": 10,
            "completedAt": if status == "completed" { json!(20) } else { Value::Null },
            "syncUpdatedAt": updated_at
        })
    }

    #[test]
    fn newer_tombstone_removes_following_and_pending_tasks() {
        let mut state = default_state(false);
        state["following"] = json!([followed(1, 1_000)]);
        state["tasks"] = json!([task("1-1", 1, "pending", 1_000)]);
        let remote = json!({
            "version": SYNC_VERSION,
            "following": [],
            "tasks": [],
            "followingDeletedAt": {"1": 2_000}
        });

        let (changed, merged, _) = merge_document_into_state(&mut state, &remote).unwrap();

        assert!(changed);
        assert!(state["following"].as_array().unwrap().is_empty());
        assert!(state["tasks"].as_array().unwrap().is_empty());
        assert_eq!(merged["followingDeletedAt"]["1"], 2_000);
    }

    #[test]
    fn newest_task_record_wins_conflict() {
        let mut state = default_state(false);
        state["following"] = json!([followed(1, 3_000)]);
        state["tasks"] = json!([task("1-1", 1, "completed", 2_000)]);
        let remote = json!({
            "version": SYNC_VERSION,
            "following": [followed(1, 3_000)],
            "tasks": [task("1-1", 1, "pending", 1_000)],
            "followingDeletedAt": {}
        });

        merge_document_into_state(&mut state, &remote).unwrap();

        assert_eq!(state["tasks"][0]["status"], "completed");
        assert_eq!(state["tasks"][0]["syncUpdatedAt"], 2_000);
    }

    #[test]
    fn removing_following_cleans_only_its_pending_tasks() {
        let mut state = default_state(false);
        state["following"] = json!([followed(1, 1_000), followed(2, 1_000)]);
        state["tasks"] = json!([
            task("1-1", 1, "pending", 1_000),
            task("1-2", 1, "completed", 1_000),
            task("2-1", 2, "pending", 1_000)
        ]);

        assert!(remove_following(&mut state, 1));

        assert_eq!(state["following"].as_array().unwrap().len(), 1);
        assert_eq!(state["tasks"].as_array().unwrap().len(), 2);
        assert!(
            state["tasks"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["id"] == "1-2")
        );
        assert!(
            state["tasks"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["id"] == "2-1")
        );
        assert!(value_i64(state["syncMetadata"]["followingDeletedAt"].get("1")) > 0);
    }

    #[test]
    fn migration_repairs_shapes_and_adds_setting_defaults() {
        let loaded =
            json!({"version": 0, "following": "invalid", "settings": {"pollIntervalMinutes": 15}});

        let migrated = merge_defaults(loaded, false);

        assert_eq!(migrated["version"], STATE_VERSION);
        assert!(migrated["following"].is_array());
        assert!(migrated["tasks"].is_array());
        assert!(migrated["seenAiringEvents"].is_array());
        assert_eq!(migrated["settings"]["pollIntervalMinutes"], 15);
        assert_eq!(migrated["settings"]["createWatchTasks"], true);
        assert_eq!(migrated["settings"]["showTrayIcon"], true);
        assert!(migrated["syncMetadata"]["followingDeletedAt"].is_object());
    }

    #[test]
    fn reminder_time_validation_accepts_only_padded_twenty_four_hour_values() {
        assert!(is_valid_reminder_time("00:00"));
        assert!(is_valid_reminder_time("20:00"));
        assert!(is_valid_reminder_time("23:59"));
        assert!(!is_valid_reminder_time("8:05"));
        assert!(!is_valid_reminder_time("24:00"));
        assert!(!is_valid_reminder_time("20:60"));
        assert!(!is_valid_reminder_time(""));
    }

    #[test]
    fn url_normalization_accepts_only_plain_https_urls() {
        assert_eq!(
            normalize_url("https://dav.example.com/root", None).unwrap(),
            "https://dav.example.com/root/"
        );
        assert_eq!(
            normalize_url("https://proxy.example.com", Some("/v0")).unwrap(),
            "https://proxy.example.com/v0"
        );
        assert!(normalize_url("http://dav.example.com", None).is_err());
        assert!(normalize_url("https://user@dav.example.com", None).is_err());
        assert!(normalize_url("https://dav.example.com?a=1", None).is_err());
    }

    #[test]
    fn enabled_webdav_requires_complete_credentials() {
        assert!(validate_webdav_config(false, "", "", false).is_ok());
        assert!(validate_webdav_config(true, "https://dav.example.com/", "user", true).is_ok());
        assert!(validate_webdav_config(true, "", "user", true).is_err());
        assert!(validate_webdav_config(true, "https://dav.example.com/", " ", true).is_err());
        assert!(validate_webdav_config(true, "https://dav.example.com/", "user", false).is_err());
    }

    #[test]
    fn original_edition_cannot_enable_bangumi() {
        assert!(!can_use_bangumi(true, DEFAULT_BANGUMI_PROXY));
        assert!(can_use_bangumi(false, ""));
        assert!(can_use_bangumi(false, DEFAULT_BANGUMI_PROXY));
        assert_eq!(default_state(true)["settings"]["bangumiApiBaseUrl"], "");
    }

    // 回归锁定（schema §9）：Bangumi 设置块等 additive 字段绝不进坚果云文档，
    // document_from_state 输出恰好 {version, updatedAt, following, tasks,
    // followingDeletedAt} 五键。
    #[cfg(feature = "standard")]
    #[test]
    fn document_from_state_excludes_bangumi_block_and_device_fields() {
        let mut state = default_state(false);
        state["bangumi"] =
            json!({"syncEnabled": true, "apiBaseUrl": "https://proxy.example.com/v0"});
        state["bangumiTitles"]["1"] = json!({"status": "matched", "nameCn": "中文"});
        state["seenAiringEvents"] = json!(["1-1"]);
        state["lastSyncAt"] = json!(123);
        state["settings"]["pollIntervalMinutes"] = json!(15);

        let document = document_from_state(&mut state);

        let mut keys: Vec<&str> = document
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "following",
                "followingDeletedAt",
                "tasks",
                "updatedAt",
                "version"
            ]
        );
        assert_eq!(document["version"], SYNC_VERSION);
    }

    #[cfg(feature = "standard")]
    #[test]
    fn default_state_writes_bangumi_block_only_for_standard() {
        let standard = default_state(false);
        let keys: Vec<String> = standard["bangumi"]
            .as_object()
            .expect("standard default state must carry a bangumi block")
            .keys()
            .cloned()
            .collect();
        assert_eq!(keys.len(), 8);
        assert_eq!(standard["bangumi"]["conflictPolicy"], "latest");

        let original = default_state(true);
        assert!(original.get("bangumi").is_none());
    }

    #[cfg(feature = "standard")]
    fn embedded_bangumi_map() -> Value {
        serde_json::from_str(include_str!(concat!(env!("OUT_DIR"), "/bangumi-map.json")))
            .expect("parse embedded bangumi map")
    }

    #[cfg(feature = "standard")]
    #[test]
    fn embedded_bangumi_map_is_version_2() {
        let map = embedded_bangumi_map();
        assert_eq!(map["version"], 2);
        assert!(map["bySubject"].is_object());
        assert!(map["anilistIndex"].is_object());
        assert!(!map["bySubject"].as_object().unwrap().is_empty());
        assert!(!map["anilistIndex"].as_object().unwrap().is_empty());
    }

    #[cfg(feature = "standard")]
    #[test]
    fn embedded_bangumi_map_carries_schedule_metadata() {
        let map = embedded_bangumi_map();
        let by_subject = map["bySubject"].as_object().unwrap();
        let mut iso_parsed = 0;
        let mut recurrence = 0;
        for entry in by_subject.values() {
            // Item-level begin must be a parseable ISO8601 timestamp.
            if let Some(begin) = entry["begin"].as_str() {
                let normalized = begin.replace('Z', "+00:00");
                assert!(
                    chrono::DateTime::parse_from_rfc3339(&normalized).is_ok(),
                    "begin not ISO8601: {begin}"
                );
                iso_parsed += 1;
            }
            // A broadcast (item-level or any site) is a "R/..." recurrence.
            let site_bc = entry["sites"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|site| site["broadcast"].as_str());
            let has_recurrence = entry["broadcast"]
                .as_str()
                .into_iter()
                .chain(site_bc)
                .any(|value| value.starts_with("R/"));
            if has_recurrence {
                recurrence += 1;
            }
            // Every site begin/broadcast that is present is ISO8601 / "R/" form.
            if let Some(sites) = entry["sites"].as_array() {
                for site in sites {
                    if let Some(begin) = site["begin"].as_str() {
                        let normalized = begin.replace('Z', "+00:00");
                        assert!(
                            chrono::DateTime::parse_from_rfc3339(&normalized).is_ok(),
                            "site begin not ISO8601: {begin}"
                        );
                    }
                    if let Some(broadcast) = site["broadcast"].as_str() {
                        assert!(
                            broadcast.starts_with("R/"),
                            "site broadcast not a recurrence: {broadcast}"
                        );
                    }
                }
            }
        }
        assert!(
            iso_parsed > by_subject.len() / 2,
            "majority of entries should carry begin; got {iso_parsed} of {}",
            by_subject.len()
        );
        assert!(recurrence > 0, "expected some broadcast recurrences");
    }

    #[cfg(feature = "standard")]
    #[test]
    fn offline_bangumi_subject_resolves_by_subject_id() {
        let map = embedded_bangumi_map();
        // Re:Zero's Bangumi subject id is 140001; direct lookup must return the
        // entry and it must agree with the anilistIndex mapping.
        let subject_id = value_i64(map["anilistIndex"].get("21355"));
        assert!(subject_id > 0);
        let entry = offline_bangumi_subject(&map, subject_id).expect("subject entry");
        assert_eq!(value_i64(entry.get("b")), subject_id);
        assert_eq!(value_i64(entry.get("a")), 21355);
        assert_eq!(value_string(entry.get("c")), "Re：从零开始的异世界生活");
        assert!(offline_bangumi_subject(&map, -1).is_none());
    }

    #[cfg(feature = "standard")]
    #[test]
    fn embedded_bangumi_data_resolves_anilist_ids_without_network() {
        let map = embedded_bangumi_map();
        let anime = json!({
            "id": 21355,
            "title": {
                "native": "Re:ゼロから始める異世界生活",
                "romaji": "Re:Zero kara Hajimeru Isekai Seikatsu",
                "english": "Re:ZERO -Starting Life in Another World-"
            },
            "format": "TV",
            "startDate": {"year": 2016, "month": 4, "day": 4}
        });

        let matched = offline_bangumi_match(&map, &anime, 2_000_000_000).unwrap();

        assert_eq!(matched["status"], "matched");
        assert_eq!(matched["nameCn"], "Re：从零开始的异世界生活");
        assert_eq!(matched["source"], "bangumi-data-anilist-id");
    }

    #[cfg(feature = "original")]
    #[test]
    fn original_embedded_bangumi_map_is_empty_version_2() {
        let map: Value =
            serde_json::from_str(include_str!(concat!(env!("OUT_DIR"), "/bangumi-map.json")))
                .expect("parse embedded bangumi map");
        assert_eq!(map["version"], 2);
        assert_eq!(map["bySubject"].as_object().map_or(0, |m| m.len()), 0);
        assert_eq!(map["anilistIndex"].as_object().map_or(0, |m| m.len()), 0);
        assert!(offline_bangumi_subject(&map, 1).is_none());
    }

    #[test]
    fn following_uses_an_existing_bangumi_title_match() {
        let mut state = default_state(false);
        state["bangumiTitles"]["42"] = json!({
            "animeId": 42,
            "status": "matched",
            "subjectId": 9001,
            "nameCn": "中文标题"
        });
        let anime = json!({
            "id": 42,
            "title": {"english": "English title", "romaji": "Romaji title", "native": "日本語"}
        });

        let (title, source, subject_id) = followed_title_fields(&state, &anime, false);

        assert_eq!(title, "中文标题");
        assert_eq!(source, "bangumi");
        assert_eq!(subject_id, 9001);
    }

    #[test]
    fn bangumi_cache_uses_status_and_premiere_aware_ttls() {
        let now = 2_000_000_000;
        let future = json!({
            "id": 42,
            "startDate": {"year": 2035, "month": 1, "day": 1}
        });
        let past = json!({
            "id": 42,
            "startDate": {"year": 2020, "month": 1, "day": 1}
        });
        let mut state = default_state(false);
        state["bangumiTitles"]["42"] = json!({
            "status": "unmatched",
            "checkedAt": now - 12 * 3_600,
            "resolverVersion": BANGUMI_RESOLVER_VERSION
        });
        assert!(cached_bangumi_title(&state, &future, now).is_some());

        state["bangumiTitles"]["42"]["checkedAt"] = json!(now - 2 * 86_400);
        assert!(cached_bangumi_title(&state, &future, now).is_none());
        assert!(cached_bangumi_title(&state, &past, now).is_some());

        state["bangumiTitles"]["42"]["checkedAt"] = json!(now - 8 * 86_400);
        assert!(cached_bangumi_title(&state, &past, now).is_none());
        state["bangumiTitles"]["42"] = json!({
            "status": "matched",
            "nameCn": "中文标题",
            "checkedAt": now - 30 * 86_400
        });
        assert!(cached_bangumi_title(&state, &past, now).is_some());
    }

    #[test]
    fn original_title_preference_updates_related_tasks_but_keeps_custom_titles() {
        let mut state = default_state(true);
        state["settings"]["titlePreference"] = json!("native");
        state["following"] = json!([
            {
                "id": 1,
                "title": {"english": "English 1", "romaji": "Romaji 1", "native": "日本語 1"},
                "displayTitle": "English 1",
                "titleSource": "anilist"
            },
            {
                "id": 2,
                "title": {"english": "English 2", "romaji": "Romaji 2", "native": "日本語 2"},
                "displayTitle": "My title",
                "titleSource": "custom"
            }
        ]);
        state["tasks"] = json!([
            task("1-1", 1, "pending", 1_000),
            task("2-1", 2, "pending", 1_000)
        ]);

        refresh_original_followed_titles(&mut state);

        assert_eq!(state["following"][0]["displayTitle"], "日本語 1");
        assert_eq!(state["tasks"][0]["animeTitle"], "日本語 1");
        assert_eq!(state["following"][1]["displayTitle"], "My title");
        assert_eq!(state["tasks"][1]["animeTitle"], "My title");
    }

    #[test]
    fn historical_seasons_have_longer_cache_ttl() {
        assert_eq!(
            season_cache_ttl_millis("WINTER", 2026, 2026, 7),
            30 * 86_400_000
        );
        assert_eq!(
            season_cache_ttl_millis("SUMMER", 2026, 2026, 7),
            6 * 3_600_000
        );
        assert_eq!(
            season_cache_ttl_millis("FALL", 2025, 2026, 1),
            30 * 86_400_000
        );
        assert_eq!(
            season_cache_ttl_millis("FALL", 2027, 2026, 7),
            6 * 3_600_000
        );
    }

    #[test]
    fn airing_sync_respects_watch_task_setting() {
        let schedule = json!({
            "mediaId": 1,
            "episode": 2,
            "airingAt": 20,
            "media": {
                "coverImage": {"medium": "https://example.com/cover.jpg"},
                "nextAiringEpisode": {"episode": 3, "airingAt": 30}
            }
        });
        let mut state = default_state(false);
        state["following"] = json!([followed(1, 1_000)]);
        state["settings"]["createWatchTasks"] = json!(false);

        assert_eq!(
            apply_airing_schedules(&mut state, &[schedule.clone()], 20),
            AiringOutcome {
                aired: 1,
                created: 0
            }
        );
        assert!(state["tasks"].as_array().unwrap().is_empty());
        assert_eq!(state["following"][0]["nextAiringEpisode"]["episode"], 3);

        state["settings"]["createWatchTasks"] = json!(true);
        assert_eq!(
            apply_airing_schedules(&mut state, &[schedule.clone()], 20),
            AiringOutcome {
                aired: 0,
                created: 1
            }
        );
        assert_eq!(
            apply_airing_schedules(&mut state, &[schedule], 20),
            AiringOutcome {
                aired: 0,
                created: 0
            }
        );
        assert_eq!(state["tasks"].as_array().unwrap().len(), 1);
    }

    #[cfg(desktop)]
    #[test]
    fn tray_labels_follow_edition_and_interface_language() {
        assert_eq!(
            tray_labels(false, "zh-CN"),
            TrayLabels {
                open: "打开 AniLog".into(),
                sync: "立即同步".into(),
                quit: "退出".into(),
                tooltip: "AniLog - 追番任务".into(),
            }
        );
        assert_eq!(
            tray_labels(true, "en-US"),
            TrayLabels {
                open: "Open AniLog Original".into(),
                sync: "Sync now".into(),
                quit: "Quit".into(),
                tooltip: "AniLog Original - Anime tracker".into(),
            }
        );
    }

    #[cfg(desktop)]
    #[test]
    fn tray_visibility_defaults_visible_and_preserves_explicit_false() {
        assert!(show_tray_icon(&json!({})));
        assert!(show_tray_icon(&json!({"showTrayIcon": true})));
        assert!(!show_tray_icon(&json!({"showTrayIcon": false})));
        assert!(show_tray_icon(&json!({"showTrayIcon": "false"})));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_notifications_use_registered_id_only_for_installed_app() {
        let identifier = "io.anilog.app";
        assert_eq!(
            windows_notification_app_id(identifier, Path::new(r"D:\AniList\target\debug")),
            tauri_winrt_notification::Toast::POWERSHELL_APP_ID
        );
        assert_eq!(
            windows_notification_app_id(identifier, Path::new(r"D:\AniList\target\release")),
            tauri_winrt_notification::Toast::POWERSHELL_APP_ID
        );
        assert_eq!(
            windows_notification_app_id(identifier, Path::new(r"D:\Apps\AniLog")),
            identifier
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn decrypts_legacy_electron_safe_storage_password() {
        use windows::Win32::Foundation::{HLOCAL, LocalFree};
        use windows::Win32::Security::Cryptography::{CRYPT_INTEGER_BLOB, CryptProtectData};

        let directory = std::env::temp_dir().join(format!(
            "anilog-electron-credential-test-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).unwrap();

        let key = [0x2a_u8; 32];
        let mut key_input = key.to_vec();
        let input = CRYPT_INTEGER_BLOB {
            cbData: key_input.len() as u32,
            pbData: key_input.as_mut_ptr(),
        };
        let mut output = CRYPT_INTEGER_BLOB::default();
        unsafe {
            CryptProtectData(&input, None, None, None, None, 0, &mut output).unwrap();
        }
        let protected =
            unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec() };
        unsafe {
            let _ = LocalFree(Some(HLOCAL(output.pbData.cast())));
        }
        let mut encoded_key = b"DPAPI".to_vec();
        encoded_key.extend(protected);
        fs::write(
            directory.join("Local State"),
            serde_json::to_vec(&json!({
                "os_crypt": {"encrypted_key": BASE64_STANDARD.encode(encoded_key)}
            }))
            .unwrap(),
        )
        .unwrap();

        let nonce = [0x19_u8; 12];
        let cipher = Aes256Gcm::new_from_slice(&key).unwrap();
        let mut encrypted = b"v10".to_vec();
        encrypted.extend(nonce);
        encrypted.extend(
            cipher
                .encrypt(
                    aes_gcm::Nonce::from_slice(&nonce),
                    b"AniLog migration password".as_slice(),
                )
                .unwrap(),
        );
        assert_eq!(
            decrypt_legacy_webdav_password(&directory, &BASE64_STANDARD.encode(encrypted)).unwrap(),
            "AniLog migration password"
        );
        fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(desktop)]
    #[test]
    fn daily_reminder_is_claimed_once_when_due() {
        let mut state = default_state(false);
        state["settings"]["dailyTaskReminderEnabled"] = json!(true);
        state["settings"]["dailyTaskReminderTime"] = json!("20:00");
        state["tasks"] = json!([task("1-1", 1, "pending", 1_000)]);

        assert_eq!(
            claim_daily_task_reminder(&mut state, "2026-07-29", "19:59"),
            None
        );
        assert_eq!(
            claim_daily_task_reminder(&mut state, "2026-07-29", "20:00"),
            Some((1, "zh-CN".into()))
        );
        assert_eq!(
            claim_daily_task_reminder(&mut state, "2026-07-29", "21:00"),
            None
        );
    }

    // -- Phase 2：STATE_VERSION 3 迁移 + 映射引擎 ----------------------------

    fn legacy_v2_state() -> Value {
        serde_json::from_str(include_str!("../fixtures/bangumi/old-state-v2.json"))
            .expect("parse old-state-v2 fixture")
    }

    #[test]
    fn state_version_is_three() {
        assert_eq!(STATE_VERSION, 3);
        assert_eq!(default_state(false)["version"], 3);
        assert_eq!(default_state(true)["version"], 3);
    }

    #[cfg(feature = "standard")]
    #[test]
    fn merge_defaults_migrates_v2_fixture_to_v3_with_record_defaults() {
        let migrated = merge_defaults(legacy_v2_state(), false);

        assert_eq!(migrated["version"], 3);
        // 旧 v2 following 条目：视为 anilist 来源，id 不动，既有字段原样保留。
        let rezero = &migrated["following"][0];
        assert_eq!(rezero["id"], 21355);
        assert_eq!(rezero["source"], "anilist");
        assert!(rezero["anilistId"].is_null());
        assert!(rezero["mapping"].is_null());
        assert_eq!(rezero["mappingPending"], false);
        assert_eq!(rezero["displayTitle"], "Re:ゼロから始める異世界生活");
        assert_eq!(rezero["followedAt"], 1_770_000_000_000i64);
        // 旧任务：episodeType 缺省 regular，episodeSortKey 取 episode 数字串。
        let pending = &migrated["tasks"][0];
        assert_eq!(pending["id"], "21355-1");
        assert_eq!(pending["episodeType"], "regular");
        assert_eq!(pending["episodeSortKey"], "1");
        assert!(pending["subjectId"].is_null());
        assert!(pending["episodeId"].is_null());
        assert_eq!(migrated["tasks"][2]["episodeType"], "regular");
        // standard 版补 bangumi 块。
        assert!(migrated.get("bangumi").is_some());
    }

    #[cfg(feature = "standard")]
    #[test]
    fn merged_v2_state_document_keeps_five_keys_but_records_carry_new_fields() {
        let mut migrated = merge_defaults(legacy_v2_state(), false);

        let document = document_from_state(&mut migrated);

        let mut keys: Vec<&str> = document
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "following",
                "followingDeletedAt",
                "tasks",
                "updatedAt",
                "version"
            ]
        );
        assert_eq!(document["version"], SYNC_VERSION);
        // 记录体自然携带新字段（属于 following/tasks 数组的记录体，允许）。
        assert_eq!(document["following"][0]["source"], "anilist");
        assert_eq!(document["tasks"][0]["episodeType"], "regular");
    }

    #[cfg(feature = "original")]
    #[test]
    fn original_v3_merge_adds_no_bangumi_or_source_keys() {
        let migrated = merge_defaults(legacy_v2_state(), true);

        assert_eq!(migrated["version"], 3);
        assert!(migrated.get("bangumi").is_none());
        for item in migrated["following"].as_array().unwrap() {
            assert!(
                item.get("source").is_none(),
                "original must not write source"
            );
            assert!(item.get("mapping").is_none());
            assert!(item.get("mappingPending").is_none());
            assert!(item.get("anilistId").is_none());
        }
        for task in migrated["tasks"].as_array().unwrap() {
            assert!(task.get("episodeType").is_none());
            assert!(task.get("episodeSortKey").is_none());
            assert!(task.get("subjectId").is_none());
            assert!(task.get("episodeId").is_none());
        }
        // Anime 形状透传在 original 是 no-op。
        let anime = vec![json!({"id": 1, "title": {}})];
        let annotated = annotate_anime_sources(anime.clone(), true);
        assert!(annotated[0].get("source").is_none());
        assert_eq!(annotated[0]["id"], 1);
    }

    #[cfg(feature = "standard")]
    fn mapping_entry(id: i64, title: &str, start: Value, format: &str) -> Value {
        json!({
            "id": id,
            "title": {"native": title, "romaji": title, "english": title},
            "startDate": start,
            "format": format,
            "seasonYear": start.get("year").cloned().unwrap_or(Value::Null)
        })
    }

    #[cfg(feature = "standard")]
    fn hand_mapping_map() -> Value {
        json!({
            "version": 2,
            "bySubject": {
                "4001": {"b": 4001, "a": 1000, "c": "甲", "t": "Alpha", "d": "2020-01-01", "f": "tv"},
                "7001": {"b": 7001, "a": 0, "c": "丙一", "t": "Beta Show", "d": "2021-04-04", "f": "tv"},
                "7002": {"b": 7002, "a": 0, "c": "丙二", "t": "Beta Show", "d": "2021-04-04", "f": "tv"},
                "6001": {"b": 6001, "a": 0, "c": "丁", "t": "Gamma Tale", "d": "2022-07-01", "f": "tv"},
                "9501": {"b": 9501, "a": 0, "c": "电影甲", "t": "Movie One", "d": "2023-01-01", "f": "movie"},
                "9999": {"b": 9999, "a": 0, "c": "无关", "t": "Unrelated Thing", "d": "1999-01-01", "f": "tv"}
            },
            "anilistIndex": {"1000": 4001}
        })
    }

    #[cfg(feature = "standard")]
    #[test]
    fn mapping_resolves_offline_index_hit_as_high_local() {
        let map = embedded_bangumi_map();
        let subject_id = value_i64(map["anilistIndex"].get("21355"));
        let entry = mapping_entry(
            21355,
            "Re:ゼロから始める異世界生活",
            json!({"year": 2016, "month": 4, "day": 4}),
            "TV",
        );

        match bangumi::resolve_mapping_candidates(&map, &entry) {
            bangumi::MappingResolution::Mapped {
                subject_id: got,
                confidence,
                method,
            } => {
                assert_eq!(got, subject_id);
                assert_eq!(got, 140001);
                assert_eq!(confidence, bangumi::MappingConfidence::High);
                assert_eq!(method, bangumi::MappingMethod::Local);
            }
            other => panic!("expected Mapped, got {other:?}"),
        }
    }

    #[cfg(feature = "standard")]
    #[test]
    fn mapping_multi_candidates_within_gap_are_not_auto_bound() {
        let map = hand_mapping_map();
        let entry = mapping_entry(
            2000,
            "Beta Show",
            json!({"year": 2021, "month": 4, "day": 4}),
            "TV",
        );

        match bangumi::resolve_mapping_candidates(&map, &entry) {
            bangumi::MappingResolution::Candidates(candidates) => {
                assert_eq!(candidates.len(), 2);
                assert_eq!(candidates[0].subject_id, 7001);
                assert_eq!(candidates[0].name_cn, "丙一");
                assert_eq!(candidates[0].date, "2021-04-04");
                assert!(candidates[0].score > 0);
            }
            other => panic!("expected Candidates, got {other:?}"),
        }
    }

    #[cfg(feature = "standard")]
    #[test]
    fn mapping_movie_format_never_auto_binds() {
        let map = hand_mapping_map();
        let entry = mapping_entry(
            4000,
            "Movie One",
            json!({"year": 2023, "month": 1, "day": 1}),
            "MOVIE",
        );

        match bangumi::resolve_mapping_candidates(&map, &entry) {
            bangumi::MappingResolution::Candidates(_) => {}
            other => panic!("movie format must not auto bind, got {other:?}"),
        }

        let ova_entry = mapping_entry(
            4100,
            "Gamma Tale",
            json!({"year": 2022, "month": 7, "day": 1}),
            "OVA",
        );
        match bangumi::resolve_mapping_candidates(&map, &ova_entry) {
            bangumi::MappingResolution::Mapped { .. } => {
                panic!("ova format must not auto bind")
            }
            _ => {}
        }
    }

    #[cfg(feature = "standard")]
    #[test]
    fn mapping_title_without_year_is_never_high() {
        let map = hand_mapping_map();
        // 标题相同但无日期：分数不足（也不会给 high）→ 走 Candidates 待确认。
        let entry = mapping_entry(4200, "Gamma Tale", Value::Null, "TV");

        match bangumi::resolve_mapping_candidates(&map, &entry) {
            bangumi::MappingResolution::Mapped { confidence, .. } => {
                assert_ne!(confidence, bangumi::MappingConfidence::High);
            }
            bangumi::MappingResolution::Candidates(_) => {}
            bangumi::MappingResolution::None => panic!("expected candidates to exist"),
        }
    }

    #[cfg(feature = "standard")]
    #[test]
    fn mapping_title_plus_year_binds_at_medium_title_year() {
        let map = hand_mapping_map();
        // 标题精确 + 同年不同日：100 + 30 + 10 = 140 → medium，不可能是 high。
        let entry = mapping_entry(
            5000,
            "Gamma Tale",
            json!({"year": 2022, "month": 3, "day": 3}),
            "TV",
        );

        match bangumi::resolve_mapping_candidates(&map, &entry) {
            bangumi::MappingResolution::Mapped {
                subject_id,
                confidence,
                method,
            } => {
                assert_eq!(subject_id, 6001);
                assert_eq!(confidence, bangumi::MappingConfidence::Medium);
                assert_eq!(method, bangumi::MappingMethod::TitleYear);
            }
            other => panic!("expected Mapped medium, got {other:?}"),
        }
    }

    #[cfg(feature = "standard")]
    #[test]
    fn apply_mapping_rekeys_entry_and_pending_tasks_but_keeps_completed_history() {
        let mut state = default_state(false);
        state["following"] = json!([followed(21355, 1_000)]);
        state["tasks"] = json!([
            task("21355-1", 21355, "pending", 1_000),
            task("21355-2", 21355, "completed", 2_000)
        ]);

        assert!(apply_mapping(&mut state, 21355, 140001, false));

        let entry = &state["following"][0];
        assert_eq!(entry["id"], 140001);
        assert_eq!(entry["source"], "bangumi");
        assert_eq!(entry["anilistId"], 21355);
        assert_eq!(entry["bangumiId"], 140001);
        assert_eq!(entry["mapping"]["method"], "local");
        assert_eq!(entry["mapping"]["confidence"], "high");
        assert!(value_i64(entry["mapping"].get("updatedAt")) > 0);
        assert_eq!(entry["mappingPending"], false);
        // 旧 anilist id 写墓碑，防止 v0.6 设备把旧记录同步回来。
        assert!(value_i64(state["syncMetadata"]["followingDeletedAt"].get("21355")) > 0);
        // 未完成任务重键。
        let pending = state["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|task| value_string(task.get("id")) == "140001-1")
            .expect("rekeyed pending task");
        assert_eq!(pending["animeId"], 140001);
        assert_eq!(pending["subjectId"], 140001);
        // 已完成任务原样保留（观看历史不重键）。
        let completed = state["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|task| value_string(task.get("id")) == "21355-2")
            .expect("completed history kept");
        assert_eq!(completed["animeId"], 21355);
        assert!(completed.get("subjectId").is_none());

        // 幂等：重复 confirm 同一映射无副作用。
        let snapshot = serde_json::to_string(&state).unwrap();
        assert!(!apply_mapping(&mut state, 21355, 140001, false));
        assert!(!apply_mapping(&mut state, 140001, 140001, true));
        assert_eq!(serde_json::to_string(&state).unwrap(), snapshot);
    }

    #[cfg(feature = "standard")]
    #[test]
    fn skip_mapping_marks_low_confidence_local_idempotently() {
        let mut state = default_state(false);
        state["following"] = json!([followed(42, 1_000)]);
        state["following"][0]["mappingPending"] = json!(true);

        assert!(skip_mapping_entry(&mut state, 42));
        let entry = &state["following"][0];
        assert_eq!(entry["mappingPending"], false);
        assert_eq!(entry["mapping"]["method"], "local");
        assert_eq!(entry["mapping"]["confidence"], "low");
        assert!(value_i64(entry["mapping"].get("updatedAt")) > 0);

        assert!(!skip_mapping_entry(&mut state, 42));
        assert!(!skip_mapping_entry(&mut state, 424242));
    }

    #[cfg(feature = "standard")]
    #[test]
    fn auto_map_following_batch_covers_hit_candidates_and_missing() {
        let map = hand_mapping_map();
        let mut state = default_state(false);
        state["following"] = json!([
            // 命中（anilistIndex）→ 自动绑定 high/local 并重键。
            merge(
                mapping_entry(
                    1000,
                    "Alpha",
                    json!({"year": 2020, "month": 1, "day": 1}),
                    "TV"
                ),
                json!({"syncUpdatedAt": 1})
            ),
            // 多候选（分差 <8）→ mappingPending。
            merge(
                mapping_entry(
                    2000,
                    "Beta Show",
                    json!({"year": 2021, "month": 4, "day": 4}),
                    "TV"
                ),
                json!({"syncUpdatedAt": 2})
            ),
            // 无结果 → mappingPending。
            merge(
                mapping_entry(3000, "Zeta Nothing", Value::Null, "TV"),
                json!({"syncUpdatedAt": 3})
            ),
            // 标题+年份 medium → 自动绑定 medium/title-year。
            merge(
                mapping_entry(
                    5000,
                    "Gamma Tale",
                    json!({"year": 2022, "month": 3, "day": 3}),
                    "TV"
                ),
                json!({"syncUpdatedAt": 4})
            )
        ]);
        use serde_json::Value as V;
        fn merge(mut base: V, patch: V) -> V {
            if let (Some(target), Some(patch)) = (base.as_object_mut(), patch.as_object()) {
                for (key, value) in patch {
                    target.insert(key.clone(), value.clone());
                }
            }
            base
        }

        let applied = auto_map_following(&mut state, &map);
        assert_eq!(applied, 2);

        let entries: Vec<&Value> = state["following"].as_array().unwrap().iter().collect();
        let hit = entries
            .iter()
            .find(|e| value_i64(e.get("id")) == 4001)
            .expect("rekeyed hit");
        assert_eq!(hit["source"], "bangumi");
        assert_eq!(hit["anilistId"], 1000);
        assert_eq!(hit["mapping"]["method"], "local");
        assert_eq!(hit["mapping"]["confidence"], "high");
        assert_eq!(hit["mappingPending"], false);

        let medium = entries
            .iter()
            .find(|e| value_i64(e.get("id")) == 6001)
            .expect("rekeyed medium");
        assert_eq!(medium["anilistId"], 5000);
        assert_eq!(medium["mapping"]["method"], "title-year");
        assert_eq!(medium["mapping"]["confidence"], "medium");
        assert_eq!(medium["mappingPending"], false);

        for pending_id in [2000, 3000] {
            let pending = entries
                .iter()
                .find(|e| value_i64(e.get("id")) == pending_id)
                .expect("untouched id");
            assert_eq!(pending["mappingPending"], true);
            assert!(pending.get("mapping").is_none_or(Value::is_null));
        }
        // 墓碑：旧 anilist id 已写墓碑。
        assert!(value_i64(state["syncMetadata"]["followingDeletedAt"].get("1000")) > 0);
        assert!(value_i64(state["syncMetadata"]["followingDeletedAt"].get("5000")) > 0);
    }

    #[cfg(feature = "standard")]
    #[test]
    fn resolve_mapping_entry_reports_command_payload() {
        let map = hand_mapping_map();

        // 未找到条目 → unavailable。
        let missing = resolve_mapping_entry(&json!({"following": []}), &map, 12345);
        assert_eq!(missing["status"], "unavailable");
        assert!(missing["subjectId"].is_null());

        // 已映射条目（手动映射）→ mapped。
        let mapped_state = json!({"following": [{
            "id": 4001, "source": "bangumi", "bangumiId": 4001,
            "mapping": {"method": "manual", "confidence": "high", "updatedAt": 1},
            "displayTitle": "甲", "format": "TV", "coverImage": "", "seasonYear": 2020
        }]});
        let mapped = resolve_mapping_entry(&mapped_state, &map, 4001);
        assert_eq!(mapped["status"], "mapped");
        assert_eq!(mapped["subjectId"], 4001);
        assert_eq!(mapped["anime"]["displayTitle"], "甲");
        assert_eq!(mapped["candidates"].as_array().unwrap().len(), 0);

        // 多候选 → pending + candidates 投影。
        let pending_state = json!({"following": [
            merge(mapping_entry(2000, "Beta Show", json!({"year": 2021, "month": 4, "day": 4}), "TV"), json!({"displayTitle": "Beta"}))
        ]});
        fn merge(mut base: Value, patch: Value) -> Value {
            if let (Some(target), Some(patch)) = (base.as_object_mut(), patch.as_object()) {
                for (key, value) in patch {
                    target.insert(key.clone(), value.clone());
                }
            }
            base
        }
        let pending = resolve_mapping_entry(&pending_state, &map, 2000);
        assert_eq!(pending["status"], "pending");
        assert!(pending["subjectId"].is_null());
        let candidates = pending["candidates"].as_array().unwrap();
        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0]["subjectId"], 7001);
        assert_eq!(candidates[0]["nameCn"], "丙一");
        assert!(candidates[0].get("score").is_some());
    }

    #[cfg(feature = "standard")]
    #[test]
    fn toggle_follow_bangumi_source_uses_subject_key_shape() {
        let anime = json!({
            "id": 140001,
            "source": "bangumi",
            "anilistId": 21355,
            "nameCn": "Re：从零开始的异世界生活",
            "title": {"native": "Re:ゼロから始める異世界生活", "romaji": null, "english": null},
            "coverImage": {"medium": "https://lain.bgm.tv/pic/cover/m/1.jpg"},
            "format": "TV",
            "episodes": 25,
            "seasonYear": 2016
        });

        let entry = bangumi_following_entry(&anime, "auto", "zh-CN");

        assert_eq!(entry["id"], 140001);
        assert_eq!(entry["source"], "bangumi");
        assert_eq!(entry["anilistId"], 21355);
        assert_eq!(entry["bangumiId"], 140001);
        assert_eq!(entry["titleSource"], "bangumi");
        assert_eq!(entry["displayTitle"], "Re：从零开始的异世界生活");
        assert_eq!(entry["siteUrl"], "https://bgm.tv/subject/140001");
        assert_eq!(entry["mapping"]["method"], "manual");
        assert_eq!(entry["mapping"]["confidence"], "high");
        assert_eq!(entry["mappingPending"], false);
    }

    #[cfg(feature = "standard")]
    #[test]
    fn season_anime_annotation_is_additive_for_standard() {
        let annotated = annotate_anime_sources(
            vec![
                json!({"id": 7, "title": {}}),
                json!({"id": 8, "source": "bangumi", "bangumiSubjectId": 9, "anilistId": 7}),
            ],
            false,
        );
        assert_eq!(annotated[0]["source"], "anilist");
        assert!(annotated[0]["bangumiSubjectId"].is_null());
        assert_eq!(annotated[0]["anilistId"], 7);
        // 已带键的记录原样保留（季度链下批接入）。
        assert_eq!(annotated[1]["source"], "bangumi");
        assert_eq!(annotated[1]["bangumiSubjectId"], 9);
    }

    // -- Phase 1 Bangumi 命令 ---------------------------------------------------

    #[cfg(feature = "standard")]
    fn bangumi_test_base(url: &str) -> bangumi::BangumiBaseUrls {
        bangumi::BangumiBaseUrls {
            root: url.to_string(),
            v0: format!("{url}/v0"),
        }
    }

    #[cfg(feature = "standard")]
    #[test]
    fn bangumi_base_urls_prefers_block_then_settings_then_official() {
        // 决策 11：顶层 bangumi.apiBaseUrl 优先。
        assert_eq!(
            bangumi_base_urls(&json!({
                "bangumi": {"apiBaseUrl": "https://a.example.com/v0"},
                "settings": {"bangumiApiBaseUrl": "https://b.example.com/v0"}
            }))
            .v0,
            "https://a.example.com/v0"
        );
        // 顶层为空回落 settings.bangumiApiBaseUrl。
        assert_eq!(
            bangumi_base_urls(&json!({
                "bangumi": {"apiBaseUrl": ""},
                "settings": {"bangumiApiBaseUrl": "https://b.example.com/v0"}
            }))
            .v0,
            "https://b.example.com/v0"
        );
        // 再为空用官方。
        assert_eq!(
            bangumi_base_urls(&json!({"bangumi": {}, "settings": {}})).root,
            "https://api.bgm.tv"
        );
    }

    #[cfg(feature = "standard")]
    #[test]
    fn bangumi_api_base_url_mirror_writes_block_from_settings() {
        let mut state = default_state(false);
        state["settings"]["bangumiApiBaseUrl"] = json!("https://proxy.example.com/v0");
        state["bangumi"]["apiBaseUrl"] = json!("");

        sync_bangumi_api_base_url_into_block(&mut state);

        assert_eq!(
            state["bangumi"]["apiBaseUrl"],
            "https://proxy.example.com/v0"
        );
        assert_eq!(
            state["settings"]["bangumiApiBaseUrl"],
            "https://proxy.example.com/v0"
        );
    }

    #[cfg(feature = "standard")]
    #[test]
    fn bangumi_commands_round_trip_with_memory_store() {
        use crate::bangumi::test_support::MockBangumiServer;
        use std::sync::Arc;

        let profile = include_str!("../fixtures/bangumi/user-profile.json").to_string();
        let collections =
            include_str!("../fixtures/bangumi/user-collections-page.json").to_string();
        let server =
            MockBangumiServer::spawn(Arc::new(move |_method, target, headers, _request_body| {
                if target == "/v0/me" {
                    assert!(
                        headers
                            .get("authorization")
                            .map(String::as_str)
                            .unwrap_or_default()
                            .starts_with("Bearer ")
                    );
                    (200, vec![], profile.clone())
                } else if target.starts_with("/v0/users/anilog_dev/collections?") {
                    assert!(target.contains("subject_type=2"));
                    (200, vec![], collections.clone())
                } else {
                    (404, vec![], "{}".into())
                }
            }));
        let client =
            bangumi::HttpBangumiClient::with_base(bangumi_test_base(&server.url())).unwrap();
        let tokens = bangumi::MemoryTokenStore::new();
        let username_cache = std::sync::Mutex::new(None);
        let supported = bangumi_commands::token_store_supported();
        let empty_state = json!({"bangumi": {}, "settings": {}});

        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio test runtime")
            .block_on(async {
                // 初始 status：无 Token，apiBaseUrl 展示值回落 settings。
                let status = bangumi_commands::auth_status(
                    &json!({
                        "bangumi": {"apiBaseUrl": ""},
                        "settings": {"bangumiApiBaseUrl": "https://proxy.example.com/v0"}
                    }),
                    &tokens,
                );
                assert_eq!(status["supported"], supported);
                assert_eq!(status["hasToken"], false);
                assert_eq!(status["apiBaseUrl"], "https://proxy.example.com/v0");

                // 空 Token 拒绝（固定文案）。
                let empty = bangumi_commands::save_token(&tokens, "   ");
                assert_eq!(empty["ok"], false);
                assert_eq!(empty["message"], "Token 不能为空");

                // 保存（trim）→ hasToken=true；成功响应不回显 Token。
                let saved = bangumi_commands::save_token(&tokens, "  roundtrip-token  ");
                assert_eq!(saved["ok"], true);
                assert!(!saved.to_string().contains("roundtrip-token"));
                let status = bangumi_commands::auth_status(&empty_state, &tokens);
                assert_eq!(status["hasToken"], supported);

                // profile 走 /v0/me fixture，camelCase 投影。
                let profile = bangumi_commands::user_profile(&tokens, &client).await;
                assert_eq!(profile["username"], "anilog_dev");
                assert_eq!(profile["nickname"], "阿罗");
                assert_eq!(profile["userGroup"], 10);

                // collections 分页信封 + camelCase 投影（username 缓存自 /v0/me）。
                let collections = bangumi_commands::user_collections(
                    &tokens,
                    &client,
                    &username_cache,
                    Some(0),
                    Some(30),
                )
                .await;
                assert_eq!(collections["total"], 2);
                assert_eq!(collections["items"][0]["subjectId"], 45678);
                assert_eq!(collections["items"][0]["type"], 3);
                assert_eq!(collections["items"][0]["epStatus"], 3);
                assert_eq!(collections["items"][1]["subjectId"], 99999);

                // test_connection 成功回 username/nickname。
                let connection =
                    bangumi_commands::test_connection(&tokens, &client, &username_cache).await;
                assert_eq!(connection["ok"], true);
                assert_eq!(connection["username"], "anilog_dev");
                assert_eq!(connection["nickname"], "阿罗");

                // disconnect → hasToken=false，username 缓存清空。
                let disconnected = bangumi_commands::disconnect(&tokens, &username_cache);
                assert_eq!(disconnected["ok"], true);
                let status = bangumi_commands::auth_status(&empty_state, &tokens);
                assert_eq!(status["hasToken"], false);
                assert!(username_cache.lock().unwrap().is_none());
            });
        // Authorization 头始终形如 "Bearer <token>"（形制回归，不落盘不回显）。
        for request in server.requests() {
            let authorization = request
                .headers
                .get("authorization")
                .map(String::as_str)
                .unwrap_or_default();
            assert!(
                authorization.starts_with("Bearer "),
                "bad auth header: {authorization:?}"
            );
        }
    }

    #[cfg(all(feature = "standard", target_os = "windows"))]
    #[test]
    fn bangumi_original_rejection_messages_are_stable() {
        // original edition 的拒绝语义由固定文案函数承载（original 下单独测试，
        // 这里锁定 standard 构建中的同一实现）。
        assert_eq!(
            bangumi_command_rejected(),
            json!({"ok": false, "message": "Original 版不支持 Bangumi"})
        );
        assert_eq!(
            bangumi_auth_status_rejected(),
            json!({"supported": false, "hasToken": false, "apiBaseUrl": ""})
        );
    }

    #[cfg(feature = "original")]
    #[test]
    fn original_bangumi_commands_reject_with_fixed_messages() {
        // status 命令的 supported=false 语义核心函数。
        assert_eq!(
            bangumi_auth_status_rejected(),
            json!({"supported": false, "hasToken": false, "apiBaseUrl": ""})
        );
        // 其余命令统一 ok=false + 固定文案。
        assert_eq!(
            bangumi_command_rejected(),
            json!({"ok": false, "message": "Original 版不支持 Bangumi"})
        );
        // original 仍拒绝 bangumiApiBaseUrl 写入（默认状态强制为空、无 bangumi 块）。
        assert_eq!(default_state(true)["settings"]["bangumiApiBaseUrl"], "");
        assert!(default_state(true).get("bangumi").is_none());
    }

    // -- Phase 2 任务 5：季度主链 / 播出四级优先 / subject extras ------------

    #[cfg(feature = "standard")]
    fn bangumi_season_subject(id: i64, platform: &str) -> Value {
        json!({
            "id": id,
            "name": format!("サンプルアニメ {id}"),
            "name_cn": format!("示例动画 {id}"),
            "date": "2026-07-08",
            "platform": platform,
            "eps": 12,
            "summary": "合成季度条目。",
            "images": {
                "large": format!("https://lain.bgm.tv/pic/cover/l/00/00/{id}.jpg"),
                "common": format!("https://lain.bgm.tv/pic/cover/c/00/00/{id}.jpg"),
                "medium": format!("https://lain.bgm.tv/pic/cover/m/00/00/{id}.jpg")
            },
            "rating": {"score": 7.5, "total": 100, "rank": 900}
        })
    }

    /// parse RFC3339 → unix 秒（测试内固定时间基准）。
    #[cfg(feature = "standard")]
    fn at(instant: &str) -> i64 {
        chrono::DateTime::parse_from_rfc3339(instant)
            .expect("test instant")
            .timestamp()
    }

    #[cfg(feature = "standard")]
    fn offline_entry(anilist_id: i64, begin: Value, broadcast: Value, sites: Value) -> Value {
        let mut entry = json!({"b": 45678, "a": anilist_id, "c": "Re:从零开始的异世界生活 第3章", "t": "リ:ゼロから始める異世界生活 第3章"});
        if !begin.is_null() {
            entry["begin"] = begin;
        }
        if !broadcast.is_null() {
            entry["broadcast"] = broadcast;
        }
        if !sites.is_null() {
            entry["sites"] = sites;
        }
        entry
    }

    #[cfg(feature = "standard")]
    #[test]
    fn map_subjects_to_anime_maps_fixture_page_with_offline_airing() {
        let page: bangumi::Paged<bangumi::BangumiSubject> =
            serde_json::from_str(include_str!("../fixtures/bangumi/subjects-page.json"))
                .expect("subjects-page fixture");
        let map = json!({
            "version": 2,
            "bySubject": {
                "45678": offline_entry(
                    21355,
                    json!("2026-07-08T13:00:22Z"),
                    json!("R/2026-07-08T13:00:22.000Z/P7D"),
                    json!([{"s": "bangumi", "i": "45678"}, {"s": "anilist", "i": "21355"}])
                )
            },
            "anilistIndex": {"21355": 45678}
        });
        // 与 broadcast-vectors golden 向量 weekly-rfc5545-utc 同基准：
        // now=2026-07-19T16:00:00Z → 下一播 2026-07-22T13:00:22Z。
        let checked_at = at("2026-07-19T16:00:00+00:00");
        let preferred = vec!["bangumi".to_string()];

        let anime = map_subjects_to_anime(&page.data, &map, &preferred, "SUMMER", 2026, checked_at);

        assert_eq!(anime.len(), 2);
        let first = &anime[0];
        assert_eq!(first["id"], 45678);
        assert_eq!(first["source"], "bangumi");
        assert_eq!(first["bangumiSubjectId"], 45678);
        assert_eq!(first["anilistId"], 21355);
        // 中文优先：native=name_cn、romaji=name 原文。
        assert_eq!(first["title"]["native"], "Re:从零开始的异世界生活 第3章");
        assert_eq!(
            first["title"]["romaji"],
            "リ:ゼロから始める異世界生活 第3章"
        );
        assert_eq!(first["nameCn"], "Re:从零开始的异世界生活 第3章");
        assert!(first["title"]["english"].is_null());
        assert_eq!(
            first["coverImage"]["extraLarge"],
            "https://lain.bgm.tv/pic/cover/l/00/00/45678_pL3cR.jpg"
        );
        assert_eq!(
            first["coverImage"]["medium"],
            "https://lain.bgm.tv/pic/cover/m/00/00/45678_pL3cR.jpg"
        );
        assert_eq!(first["episodes"], 16);
        assert_eq!(first["season"], "SUMMER");
        assert_eq!(first["seasonYear"], 2026);
        assert_eq!(first["startDate"]["year"], 2026);
        assert_eq!(first["startDate"]["month"], 7);
        assert_eq!(first["startDate"]["day"], 8);
        assert_eq!(first["averageScore"], 8.2);
        assert_eq!(first["format"], "TV");
        assert_eq!(first["siteUrl"], "https://bgm.tv/subject/45678");
        // 播出四级优先第一级：floor((now-begin)/P7D)+1 = 2（11 天 3 小时 → 1 整周）。
        let next_airing = at("2026-07-22T13:00:22+00:00");
        assert_eq!(first["nextAiringEpisode"]["episode"], 2);
        assert_eq!(first["nextAiringEpisode"]["airingAt"], next_airing);
        assert_eq!(
            first["nextAiringEpisode"]["timeUntilAiring"],
            next_airing - checked_at
        );
        // 离线映射缺条目 → anilistId=null、无播出数据 → nextAiringEpisode=null。
        let second = &anime[1];
        assert_eq!(second["id"], 45682);
        assert!(second["anilistId"].is_null());
        assert!(second["nextAiringEpisode"].is_null());
    }

    #[cfg(feature = "standard")]
    #[test]
    fn map_subjects_to_anime_formats_titles_and_episode_clamps() {
        let parse = |value: Value| -> bangumi::BangumiSubject {
            serde_json::from_value(value).expect("subject value")
        };
        let checked_at = at("2026-07-19T16:00:00+00:00");
        let preferred = vec!["bangumi".to_string()];
        let empty_map = json!({"bySubject": {}, "anilistIndex": {}});

        // platform → format 映射契约。
        for (platform, expected) in [
            ("TV", json!("TV")),
            ("劇場版", json!("MOVIE")),
            ("剧场版", json!("MOVIE")),
            ("OVA", json!("OVA")),
            ("Web", json!("ONA")),
            ("WEB", json!("ONA")),
            ("PV", Value::Null),
        ] {
            let subject = parse(bangumi_season_subject(46000, platform));
            let anime = map_subjects_to_anime(
                std::slice::from_ref(&subject),
                &empty_map,
                &preferred,
                "SUMMER",
                2026,
                checked_at,
            );
            assert_eq!(anime[0]["format"], expected, "platform {platform}");
        }

        // name_cn 缺失 → native=name、romaji=null；date/images 缺失回落。
        let mut subject = parse(bangumi_season_subject(46001, "TV"));
        subject.name_cn = None;
        subject.date = None;
        subject.images = None;
        let anime = map_subjects_to_anime(
            std::slice::from_ref(&subject),
            &empty_map,
            &preferred,
            "SUMMER",
            2026,
            checked_at,
        );
        assert_eq!(anime[0]["title"]["native"], "サンプルアニメ 46001");
        assert!(anime[0]["title"]["romaji"].is_null());
        assert_eq!(anime[0]["coverImage"]["extraLarge"], "");
        assert_eq!(anime[0]["coverImage"]["medium"], "");
        assert!(anime[0]["startDate"].is_null());
        assert!(anime[0]["nextAiringEpisode"].is_null());

        // 一次性 begin（电影）：已播 → null；未播 → episode 1。
        let movie_entry = offline_entry(0, json!("2027-01-01T00:00:00Z"), Value::Null, Value::Null);
        let movie_map = json!({"bySubject": {"46002": movie_entry}, "anilistIndex": {}});
        let subject = parse(bangumi_season_subject(46002, "劇場版"));
        let anime = map_subjects_to_anime(
            std::slice::from_ref(&subject),
            &movie_map,
            &preferred,
            "SUMMER",
            2026,
            checked_at,
        );
        assert_eq!(anime[0]["format"], "MOVIE");
        assert_eq!(anime[0]["nextAiringEpisode"]["episode"], 1);
        assert_eq!(
            anime[0]["nextAiringEpisode"]["airingAt"],
            at("2027-01-01T00:00:00+00:00")
        );
        let past_movie_entry =
            offline_entry(0, json!("2026-01-01T00:00:00Z"), Value::Null, Value::Null);
        let past_movie_map = json!({"bySubject": {"46002": past_movie_entry}, "anilistIndex": {}});
        let anime = map_subjects_to_anime(
            std::slice::from_ref(&subject),
            &past_movie_map,
            &preferred,
            "SUMMER",
            2026,
            checked_at,
        );
        assert!(anime[0]["nextAiringEpisode"].is_null());

        // 完结钳制（问题 D）：eps=3、checked 已过 3 整周 → 推算第 4 期 > eps
        // → 全部播完，nextAiringEpisode=null（旧行为夹到 eps 会伪造"下周播出"）。
        let clamped_entry = offline_entry(
            0,
            json!("2026-07-08T13:00:22Z"),
            json!("R/2026-07-08T13:00:22.000Z/P7D"),
            Value::Null,
        );
        let clamped_map = json!({"bySubject": {"46003": clamped_entry}, "anilistIndex": {}});
        let mut subject = parse(bangumi_season_subject(46003, "TV"));
        subject.eps = Some(3);
        let late_checked = at("2026-07-29T20:00:00+00:00");
        let anime = map_subjects_to_anime(
            std::slice::from_ref(&subject),
            &clamped_map,
            &preferred,
            "SUMMER",
            2026,
            late_checked,
        );
        assert!(anime[0]["nextAiringEpisode"].is_null());
        // 未完结（下一期号 ≤ eps）：行为不变。checked 过 1 整周 → 下一期 2 ≤ 3。
        let early_checked = at("2026-07-15T20:00:00+00:00");
        let anime = map_subjects_to_anime(
            std::slice::from_ref(&subject),
            &clamped_map,
            &preferred,
            "SUMMER",
            2026,
            early_checked,
        );
        assert_eq!(anime[0]["nextAiringEpisode"]["episode"], 2);
        // eps 未知 → 不钳制：同一时点已过 3 整周 → 第 4 期。
        subject.eps = None;
        let anime = map_subjects_to_anime(
            std::slice::from_ref(&subject),
            &clamped_map,
            &preferred,
            "SUMMER",
            2026,
            late_checked,
        );
        assert_eq!(anime[0]["nextAiringEpisode"]["episode"], 4);
    }

    #[cfg(feature = "standard")]
    #[test]
    fn map_subjects_to_anime_prefers_site_broadcast_by_preference() {
        // broadcast-vectors golden 向量 fallback-to-site-ani-one：
        // 条目级无 begin/broadcast，ani_one 站点供给规则；
        // now=2026-07-18T16:00:00Z → 下一播 2026-07-22T13:00:00Z（21:00+08:00）。
        let subject: bangumi::BangumiSubject =
            serde_json::from_value(bangumi_season_subject(45678, "TV")).expect("subject");
        let entry = offline_entry(
            0,
            Value::Null,
            Value::Null,
            json!([
                {"s": "gamer", "i": "10551"},
                {"s": "ani_one", "i": "G8W68WD7Z",
                 "begin": "2026-07-08T21:00:00+08:00",
                 "broadcast": "R/2026-07-08T21:00:00.000+08:00/P7D"}
            ]),
        );
        let map = json!({"bySubject": {"45678": entry}, "anilistIndex": {}});
        let checked_at = at("2026-07-18T16:00:00+00:00");
        let preferred = vec!["bangumi".to_string(), "ani_one".to_string()];

        let anime = map_subjects_to_anime(
            std::slice::from_ref(&subject),
            &map,
            &preferred,
            "SUMMER",
            2026,
            checked_at,
        );

        assert_eq!(anime[0]["nextAiringEpisode"]["episode"], 2);
        assert_eq!(
            anime[0]["nextAiringEpisode"]["airingAt"],
            at("2026-07-22T13:00:00+00:00")
        );
    }

    #[cfg(feature = "standard")]
    fn season_chain_state(bangumi_api_base_url: &str) -> Value {
        let mut state = default_state(false);
        state["bangumi"]["apiBaseUrl"] = json!(bangumi_api_base_url);
        state
    }

    #[cfg(feature = "standard")]
    #[test]
    fn bangumi_season_chain_paginates_merges_and_caches() {
        use crate::bangumi::test_support::MockBangumiServer;

        // 每月分页：month 7/8 返回同一批 id（total=52：满页 50 条 + 次页 2 条）
        // 以验证分页与跨月去重；month 9 单页 2 条。
        let server = MockBangumiServer::spawn(Arc::new(|_method, target, _headers, _body| {
            let Some(query) = target.strip_prefix("/v0/subjects?") else {
                return (404, vec![], "{}".into());
            };
            let mut month = 0u32;
            let mut offset = 0u32;
            for pair in query.split('&') {
                let mut parts = pair.splitn(2, '=');
                match parts.next().unwrap_or_default() {
                    "month" => month = parts.next().unwrap_or("").parse().unwrap_or(0),
                    "offset" => offset = parts.next().unwrap_or("").parse().unwrap_or(0),
                    _ => {}
                }
            }
            if !(7..=9).contains(&month) {
                return (404, vec![], "{}".into());
            }
            let (total, data): (u32, Vec<Value>) = if month == 9 {
                (
                    2,
                    vec![
                        bangumi_season_subject(45900 + offset as i64, "TV"),
                        bangumi_season_subject(45901 + offset as i64, "TV"),
                    ],
                )
            } else if offset == 0 {
                (
                    52,
                    (0..50)
                        .map(|index| bangumi_season_subject(45700 + index as i64, "TV"))
                        .collect(),
                )
            } else {
                (
                    52,
                    (0..2)
                        .map(|index| bangumi_season_subject(45750 + index as i64, "TV"))
                        .collect(),
                )
            };
            let page = json!({"total": total, "limit": 50, "offset": offset, "data": data});
            (200, vec![], page.to_string())
        }));

        let directory = std::env::temp_dir().join(format!(
            "anilog-season-chain-test-{}-{}",
            std::process::id(),
            server.port()
        ));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).unwrap();

        let map = json!({
            "version": 2,
            "bySubject": {"45700": offline_entry(12345, Value::Null, Value::Null, Value::Null)},
            "anilistIndex": {}
        });
        let state = season_chain_state("https://unused.example.com/v0");
        let base = bangumi_test_base(&server.url());
        let client = reqwest::Client::new();

        let fetch = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio test runtime")
            .block_on(fetch_season_bangumi_chain(
                &client,
                base.clone(),
                &directory,
                &map,
                &state,
                "SUMMER",
                2026,
                None,
            ));

        let SeasonFetch::Bangumi {
            anime,
            fetched_at,
            stale,
        } = fetch
        else {
            panic!("expected bangumi fetch");
        };
        assert!(!stale);
        // month7 与 month8 返回同一批 id（各 52 条，跨月去重后 52 条）+
        // month9 2 条 = 54 条：验证分页（50+2）与跨月合并去重（subjectId）。
        assert_eq!(anime.len(), 54);
        assert!(anime.iter().all(|item| item["source"] == "bangumi"));
        assert!(
            anime
                .iter()
                .all(|item| item["season"] == "SUMMER" && item["seasonYear"] == 2026)
        );
        let ids: Vec<i64> = anime.iter().map(|item| value_i64(item.get("id"))).collect();
        let expected_ids: Vec<i64> = (45700..=45751).chain([45900, 45901]).collect();
        assert_eq!(ids, expected_ids);
        assert_eq!(anime[0]["anilistId"], 12345);
        assert_eq!(anime[1]["anilistId"], Value::Null);
        assert!(fetched_at > 0);

        // 缓存落盘：bangumi-cache/2026-SUMMER.json。
        let cache_path = directory.join("2026-SUMMER.json");
        let cached: Value =
            serde_json::from_str(&fs::read_to_string(&cache_path).unwrap()).unwrap();
        assert_eq!(cached["version"], CACHE_VERSION);
        assert_eq!(cached["source"], "bangumi");
        assert_eq!(cached["fetchedAt"], fetched_at);
        assert_eq!(cached["anime"].as_array().unwrap().len(), 54);

        // 请求形制：全部 GET /v0/subjects?type=2&year=2026&month=7..9&limit=50。
        // month7/8 各 2 页（50+2，total=52），month9 单页（total=2）→ 共 5 次。
        let requests = server.requests();
        assert_eq!(requests.len(), 5);
        assert!(requests.iter().all(|request| request.method == "GET"
            && request.target.starts_with("/v0/subjects?")
            && request.target.contains("type=2")
            && request.target.contains("year=2026")
            && request.target.contains("limit=50")));

        let _ = fs::remove_dir_all(&directory);
    }

    #[cfg(feature = "standard")]
    #[test]
    fn bangumi_season_chain_cache_stale_then_anilist_fallback() {
        use crate::bangumi::test_support::MockBangumiServer;

        let server = MockBangumiServer::spawn(Arc::new(|_method, target, _headers, _body| {
            if target.starts_with("/v0/subjects?") {
                let page = json!({
                    "total": 1, "limit": 50, "offset": 0,
                    "data": [bangumi_season_subject(45700, "TV")]
                });
                (200, vec![], page.to_string())
            } else {
                (404, vec![], "{}".into())
            }
        }));
        let directory = std::env::temp_dir().join(format!(
            "anilog-season-stale-test-{}-{}",
            std::process::id(),
            server.port()
        ));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).unwrap();

        let state = season_chain_state("https://unused.example.com/v0");
        let map = json!({"bySubject": {}, "anilistIndex": {}});
        let client = reqwest::Client::new();
        let dead_base = bangumi::BangumiBaseUrls {
            root: "http://127.0.0.1:9".into(),
            v0: "http://127.0.0.1:9/v0".into(),
        };

        let run = |base: bangumi::BangumiBaseUrls| {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("tokio test runtime")
                .block_on(fetch_season_bangumi_chain(
                    &client, base, &directory, &map, &state, "SUMMER", 2026, None,
                ))
        };

        // 1. 网络刷新成功并写缓存。
        let SeasonFetch::Bangumi { stale, .. } = run(bangumi_test_base(&server.url())) else {
            panic!("expected bangumi fetch");
        };
        assert!(!stale);
        let requests_after_refresh = server.requests().len();
        assert!(requests_after_refresh > 0);

        // 2. TTL 内缓存命中：网络失败（死端口）仍返回缓存且不发请求。
        let SeasonFetch::Bangumi { stale, .. } = run(dead_base.clone()) else {
            panic!("expected bangumi cache hit");
        };
        assert!(!stale);
        assert_eq!(server.requests().len(), requests_after_refresh);

        // 3. 过期缓存 → 网络失败 → stale 兜底（fetchedAt 标注数据时点）。
        let cache_path = directory.join("2026-SUMMER.json");
        let mut cached: Value =
            serde_json::from_str(&fs::read_to_string(&cache_path).unwrap()).unwrap();
        cached["fetchedAt"] = json!(now_millis() - 25 * 3_600_000);
        fs::write(&cache_path, serde_json::to_vec(&cached).unwrap()).unwrap();
        let SeasonFetch::Bangumi {
            anime,
            stale,
            fetched_at,
        } = run(dead_base.clone())
        else {
            panic!("expected stale cache fallback");
        };
        assert!(stale);
        assert_eq!(fetched_at, cached["fetchedAt"]);
        assert_eq!(anime.len(), 1);
        assert_eq!(server.requests().len(), requests_after_refresh);

        // 4. 连缓存都没有 → 回落现有 AniList 季度路径（标记）。
        fs::remove_file(&cache_path).unwrap();
        assert!(matches!(run(dead_base), SeasonFetch::AniListFallback));

        let _ = fs::remove_dir_all(&directory);
    }

    #[cfg(all(feature = "standard", not(target_os = "android")))]
    #[test]
    fn bangumi_offline_schedules_feed_airing_pipeline_with_dedup() {
        let mut state = default_state(false);
        state["following"] = json!([{
            "id": 45678,
            "source": "bangumi",
            "displayTitle": "Re:从零开始的异世界生活 第3章",
            "coverImage": "https://lain.bgm.tv/pic/cover/m/00/00/45678_pL3cR.jpg",
            "episodes": 16,
            "followedAt": 0,
            "syncUpdatedAt": 1
        }]);
        state["tasks"] = json!([]);
        state["seenAiringEvents"] = json!([]);
        let map = json!({
            "bySubject": {
                "45678": offline_entry(
                    21355,
                    json!("2026-07-08T13:00:22Z"),
                    json!("R/2026-07-08T13:00:22.000Z/P7D"),
                    Value::Null
                )
            }
        });
        let now = at("2026-07-19T16:00:00+00:00");

        let schedules = bangumi_offline_schedules(&state, &map, now);

        // 窗口 [begin, now+1] 内已播 2 期；下一期为第 3 期。
        assert_eq!(schedules.len(), 2);
        assert_eq!(schedules[0]["mediaId"], 45678);
        assert_eq!(schedules[0]["episode"], 1);
        assert_eq!(schedules[0]["airingAt"], at("2026-07-08T13:00:22+00:00"));
        assert_eq!(schedules[1]["episode"], 2);
        assert_eq!(schedules[1]["airingAt"], at("2026-07-15T13:00:22+00:00"));
        assert_eq!(schedules[1]["media"]["nextAiringEpisode"]["episode"], 3);

        // 灌入与 AniList 相同的 apply_airing_schedules 管道：任务 id
        // "{subjectId}-{episode}"、animeId=subjectId、nextAiringEpisode 更新。
        let outcome = apply_airing_schedules(&mut state, &schedules, now);
        assert_eq!(
            outcome,
            AiringOutcome {
                aired: 2,
                created: 2
            }
        );
        let task_ids: Vec<String> = state["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|task| value_string(task.get("id")))
            .collect();
        assert_eq!(task_ids, vec!["45678-1", "45678-2"]);
        assert_eq!(state["tasks"][0]["animeId"], 45678);
        assert_eq!(state["tasks"][0]["subjectId"], 45678);
        assert_eq!(state["following"][0]["nextAiringEpisode"]["episode"], 3);

        // 重复 sync（同一调度集重算重灌）：不产生重复任务。
        let schedules_again = bangumi_offline_schedules(&state, &map, now);
        let outcome = apply_airing_schedules(&mut state, &schedules_again, now);
        assert_eq!(
            outcome,
            AiringOutcome {
                aired: 0,
                created: 0
            }
        );
        assert_eq!(state["tasks"].as_array().unwrap().len(), 2);
    }

    #[cfg(all(feature = "standard", not(target_os = "android")))]
    #[test]
    fn bangumi_offline_schedules_truncate_eps_respect_preferences_and_skip_empty() {
        let map = json!({
            "bySubject": {
                // eps=2：第 2 期后全部播完 → 无下一期。
                "45678": offline_entry(
                    0,
                    json!("2026-07-08T13:00:22Z"),
                    json!("R/2026-07-08T13:00:22.000Z/P7D"),
                    Value::Null
                ),
                // 条目级无数据、站点级有（gamer 优先于 ani_one 之外的选择）。
                "45682": offline_entry(
                    0,
                    Value::Null,
                    Value::Null,
                    json!([
                        {"s": "ani_one", "i": "X",
                         "begin": "2026-07-08T21:00:00+08:00",
                         "broadcast": "R/2026-07-08T21:00:00.000+08:00/P7D"}
                    ])
                ),
                // 全无播出数据 → 第四级：无任务。
                "45690": offline_entry(0, Value::Null, Value::Null, Value::Null)
            }
        });
        let mut state = default_state(false);
        state["following"] = json!([
            {"id": 45678, "source": "bangumi", "episodes": 2, "followedAt": 0, "syncUpdatedAt": 1},
            {"id": 45682, "source": "bangumi", "episodes": 16, "followedAt": 0, "syncUpdatedAt": 1},
            {"id": 45690, "source": "bangumi", "episodes": 12, "followedAt": 0, "syncUpdatedAt": 1},
            {"id": 1, "source": "anilist", "episodes": 12, "followedAt": 0, "syncUpdatedAt": 1}
        ]);
        let now = at("2026-07-19T16:00:00+00:00");

        let schedules = bangumi_offline_schedules(&state, &map, now);

        // 45678：2 期全播完（episode 超过 eps 截断）→ nextAiringEpisode=null；
        // 45682：按站点规则生成（now 为 16:00Z，窗口内已播 1 期 07-08T13:00Z，
        // 下一期 07-15T13:00Z 也在窗口内 → 共 2 期，下一期 07-22）；
        // 45690 / anilist 条目：不生成。
        let episodes_for = |subject_id: i64| -> Vec<i64> {
            schedules
                .iter()
                .filter(|schedule| value_i64(schedule.get("mediaId")) == subject_id)
                .map(|schedule| value_i64(schedule.get("episode")))
                .collect()
        };
        assert_eq!(episodes_for(45678), vec![1, 2]);
        assert!(
            schedules
                .iter()
                .filter(|schedule| value_i64(schedule.get("mediaId")) == 45678)
                .all(|schedule| schedule["media"]["nextAiringEpisode"].is_null())
        );
        assert_eq!(episodes_for(45682), vec![1, 2]);
        assert_eq!(
            schedules
                .iter()
                .find(|schedule| value_i64(schedule.get("mediaId")) == 45682)
                .map(|schedule| value_i64(schedule["media"]["nextAiringEpisode"].get("episode"))),
            Some(3)
        );
        assert!(episodes_for(45690).is_empty());
        assert!(episodes_for(1).is_empty());
        assert_eq!(schedules.len(), 4);

        // createWatchTasks=false：只跳过任务创建，不跳过 nextAiringEpisode 更新。
        state["settings"]["createWatchTasks"] = json!(false);
        let outcome = apply_airing_schedules(&mut state, &schedules, now);
        assert_eq!(
            outcome,
            AiringOutcome {
                aired: 4,
                created: 0
            }
        );
        assert!(state["tasks"].as_array().unwrap().is_empty());
        assert_eq!(state["following"][0]["nextAiringEpisode"], Value::Null);
        assert_eq!(state["following"][1]["nextAiringEpisode"]["episode"], 3);
    }

    #[cfg(feature = "standard")]
    #[test]
    fn bangumi_subject_extras_caches_refreshes_and_falls_back() {
        use crate::bangumi::test_support::MockBangumiServer;
        use std::sync::Mutex;

        let detail = include_str!("../fixtures/bangumi/subject-detail.json").to_string();
        let characters = include_str!("../fixtures/bangumi/subject-characters.json").to_string();
        let related = include_str!("../fixtures/bangumi/subject-related.json").to_string();
        let failing = Arc::new(Mutex::new(false));
        let handler_failing = failing.clone();
        let server = MockBangumiServer::spawn(Arc::new(move |_method, target, _headers, _body| {
            if *handler_failing.lock().unwrap() {
                return (500, vec![], "{}".into());
            }
            match target {
                "/v0/subjects/45678" => (200, vec![], detail.clone()),
                "/v0/subjects/45678/characters" => (200, vec![], characters.clone()),
                "/v0/subjects/45678/subjects" => (200, vec![], related.clone()),
                _ => (404, vec![], "{}".into()),
            }
        }));
        let directory = std::env::temp_dir().join(format!(
            "anilog-extras-test-{}-{}",
            std::process::id(),
            server.port()
        ));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).unwrap();
        let cache_path = directory.join("subject-45678.json");
        let client =
            bangumi::HttpBangumiClient::with_base(bangumi_test_base(&server.url())).unwrap();
        let now = now_seconds();

        let run = |now: i64| {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("tokio test runtime")
                .block_on(bangumi_commands::subject_extras(
                    &client,
                    &cache_path.clone(),
                    45678,
                    now,
                ))
        };

        // 1. 首次拉取：三端点各一次，extras 形状符合前端契约。
        let extras = run(now);
        assert_eq!(server.requests().len(), 3);
        assert_eq!(extras["fetchedAt"], now);
        assert_eq!(extras["rating"]["score"], 8.2);
        assert_eq!(extras["rating"]["total"], 1234);
        assert_eq!(extras["rating"]["rank"], 210);
        assert_eq!(extras["tags"][0]["name"], "Re:Zero");
        assert_eq!(extras["tags"][0]["count"], 100);
        assert_eq!(extras["characters"][0]["id"], 12345);
        assert_eq!(extras["characters"][0]["nameCn"], "爱蜜莉雅");
        assert_eq!(extras["characters"][0]["relation"], "主角");
        assert_eq!(
            extras["characters"][0]["imageUrl"],
            "https://lain.bgm.tv/pic/crt/l/00/00/12345_crt_Ab12C.jpg"
        );
        assert_eq!(extras["related"][0]["relation"], "续集");
        assert_eq!(extras["staff"].as_array().unwrap().len(), 7);
        // infobox 数组 value 连接为字符串。
        assert_eq!(
            extras["staff"][1]["value"],
            "Re:ゼロから始める異世界生活 3期、Re:ZERO -Starting Life in Another World- 3rd Season"
        );
        assert_eq!(extras["siteUrl"], "https://bgm.tv/subject/45678");

        // 2. TTL 内缓存命中：不发请求。
        let cached = run(now + 60);
        assert_eq!(cached["fetchedAt"], now);
        assert_eq!(server.requests().len(), 3);

        // 3. 过期 + 刷新失败 → 阻塞刷新失败回落旧缓存（stale）。
        let mut stale_cache: Value =
            serde_json::from_str(&fs::read_to_string(&cache_path).unwrap()).unwrap();
        stale_cache["fetchedAt"] = json!(now - 25 * 3_600);
        fs::write(&cache_path, serde_json::to_vec(&stale_cache).unwrap()).unwrap();
        *failing.lock().unwrap() = true;
        let fallback = run(now);
        assert_eq!(fallback["fetchedAt"], now - 25 * 3_600);
        assert_eq!(fallback["rating"]["score"], 8.2);

        // 4. 无缓存 + 刷新失败 → null。
        fs::remove_file(&cache_path).unwrap();
        assert!(run(now).is_null());

        let _ = fs::remove_dir_all(&directory);
    }

    #[cfg(feature = "standard")]
    #[test]
    fn bangumi_platform_format_and_preferred_site_helpers() {
        assert_eq!(
            preferred_broadcast_sites(
                &json!({"bangumi": {"preferredBroadcastSites": ["ani_one"]}})
            ),
            vec!["ani_one".to_string()]
        );
        assert_eq!(
            preferred_broadcast_sites(&json!({"bangumi": {}})),
            bangumi::default_preferred_broadcast_sites()
        );
        assert_eq!(
            preferred_broadcast_sites(&json!({})),
            bangumi::default_preferred_broadcast_sites()
        );
    }

    #[cfg(feature = "original")]
    #[test]
    fn original_season_path_and_extras_surface_unchanged() {
        // 季度主链：original 无 bangumi 块 → 不进入 Bangumi 分支，AniList 原路径
        // 行为零变化（annotate_anime_sources 对 original 不追加任何新键）。
        let anime = vec![json!({"id": 1, "title": {"native": "x"}})];
        let annotated = annotate_anime_sources(anime.clone(), true);
        assert_eq!(annotated, anime);
        assert!(annotated[0].get("source").is_none());
        assert!(annotated[0].get("bangumiSubjectId").is_none());
        assert!(annotated[0].get("anilistId").is_none());
        // extras：original 命令守卫直接返回 null（context.original → Value::Null），
        // extras 缓存/客户端代码在 original 编译产物中不存在（cfg(feature) 级隔离）。
        assert!(default_state(true).get("bangumi").is_none());
    }

    // -- Phase 3：收藏/评分/进度 拉取合并 + hash 冲突 + 写回 ---------------------

    /// Phase 3 测试底座：standard 默认状态 + 全开关打开的 bangumi 块。
    #[cfg(feature = "standard")]
    fn phase3_state(bangumi_api: &str) -> Value {
        let mut state = default_state(false);
        state["bangumi"]["apiBaseUrl"] = json!(bangumi_api);
        state["bangumi"]["syncEnabled"] = json!(true);
        state["bangumi"]["pullCollections"] = json!(true);
        state["bangumi"]["pushLocalChanges"] = json!(true);
        state["bangumi"]["pushCompletedEpisodes"] = json!(true);
        state
    }

    /// Phase 3 测试底座：Bangumi 来源 following 条目（可补丁覆盖字段）。
    #[cfg(feature = "standard")]
    fn bangumi_following(id: i64, patch: Value) -> Value {
        let mut entry = json!({
            "id": id, "source": "bangumi", "bangumiId": id,
            "title": {"native": format!("サンプル {id}"), "romaji": null, "english": null},
            "displayTitle": format!("示例 {id}"),
            "coverImage": "", "followedAt": 1, "syncUpdatedAt": 1
        });
        if let (Some(base), Some(extra)) = (entry.as_object_mut(), patch.as_object()) {
            for (key, value) in extra {
                base.insert(key.clone(), value.clone());
            }
        }
        entry
    }

    #[cfg(feature = "standard")]
    fn collection_page(data: Value) -> String {
        json!({"total": data.as_array().map_or(0, |items| items.len()), "limit": 50, "offset": 0, "data": data}).to_string()
    }

    #[cfg(feature = "standard")]
    fn slim_subject(id: i64, name_cn: &str) -> Value {
        json!({
            "id": id, "name": format!("サンプル {id}"), "name_cn": name_cn,
            "date": "2026-07-08", "eps": 12,
            "images": {"medium": format!("https://lain.bgm.tv/pic/cover/m/{id}.jpg")}
        })
    }

    #[cfg(feature = "standard")]
    fn write_count(requests: &[crate::bangumi::test_support::RequestRecord]) -> usize {
        requests
            .iter()
            .filter(|request| request.method != "GET")
            .count()
    }

    /// 验收第 4 轮问题 2 测试辅助：每用例独立的集数缓存目录（防测试间缓存
    /// 串扰），返回前清空残留。
    #[cfg(feature = "standard")]
    fn episodes_cache_dir(tag: &str) -> std::path::PathBuf {
        let directory = std::env::temp_dir().join(format!(
            "anilog-test-episodes-cache-{}-{tag}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&directory);
        directory
    }

    #[cfg(feature = "standard")]
    #[test]
    fn bangumi_sync_hash_idempotent_second_run_zero_writes() {
        use crate::bangumi::test_support::MockBangumiServer;

        let profile = include_str!("../fixtures/bangumi/user-profile.json").to_string();
        let collections = collection_page(json!([{
            "subject_id": 45678, "subject_type": 2, "rate": 8, "type": 3,
            "tags": [], "ep_status": 3, "vol_status": 0,
            "updated_at": "2026-08-01T12:00:00+08:00", "private": false,
            "subject": slim_subject(45678, "示例 45678")
        }]));
        let server =
            MockBangumiServer::spawn(Arc::new(move |method, target, _headers, _request_body| {
                if target == "/v0/me" {
                    assert_eq!(method, "GET");
                    return (200, vec![], profile.clone());
                }
                if target.starts_with("/v0/users/anilog_dev/collections?") {
                    assert!(target.contains("subject_type=2") && target.contains("limit=50"));
                    return (200, vec![], collections.clone());
                }
                if target.starts_with("/v0/users/-/collections/") {
                    return (204, vec![], String::new());
                }
                (404, vec![], "{}".into())
            }));
        let tokens = bangumi::MemoryTokenStore::new();
        tokens.store("sync-token").unwrap();
        let username_cache = std::sync::Mutex::new(None);
        let client =
            bangumi::HttpBangumiClient::with_base(bangumi_test_base(&server.url())).unwrap();
        let offline = json!({"bySubject": {}, "anilistIndex": {}});
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio test runtime");

        // 前置短路：syncEnabled=false → 零请求 skipped。
        let disabled = {
            let mut state = phase3_state("https://unused.example.com/v0");
            state["bangumi"]["syncEnabled"] = json!(false);
            state["following"] = json!([bangumi_following(55555, json!({"rating": 9}))]);
            std::sync::Mutex::new(state)
        };
        let skipped = rt.block_on(bangumi_sync::run_bangumi_collection_sync(
            &client,
            &tokens,
            &username_cache,
            &disabled,
            &offline,
        ));
        assert_eq!(skipped, bangumi::BangumiSyncReport::default());
        assert_eq!(server.requests().len(), 0);

        // 本地已有追番（评分 9，本地修改）+ 远端 doing 45678 → 第一次同步：
        // 拉取新建 45678（lastChangedBy=bangumi）+ 写回 55555（POST type=3）。
        let state = {
            let mut state = phase3_state("https://unused.example.com/v0");
            state["following"] = json!([bangumi_following(55555, json!({"rating": 9}))]);
            std::sync::Mutex::new(state)
        };
        let first = rt.block_on(bangumi_sync::run_bangumi_collection_sync(
            &client,
            &tokens,
            &username_cache,
            &state,
            &offline,
        ));
        let episodes_cache = episodes_cache_dir("hash-idempotent");
        let push1 = rt.block_on(bangumi_sync::push_local_changes(
            &client,
            &tokens,
            &username_cache,
            &state,
            &episodes_cache,
        ));
        let writes_first = write_count(&server.requests());
        assert!(writes_first >= 1, "first sync must write the local change");

        assert_eq!(first.pulled, 1);
        assert_eq!(first.followed, 1);
        assert_eq!(push1.pushed, 1);
        {
            let guard = state.lock().unwrap();
            let created = guard["following"]
                .as_array()
                .unwrap()
                .iter()
                .find(|item| value_i64(item.get("id")) == 45678)
                .expect("45678 created");
            assert_eq!(created["bangumiStatus"], "doing");
            assert_eq!(created["rating"], 8);
            assert_eq!(created["watchedEpisode"], 3);
            assert_eq!(created["lastChangedBy"], "bangumi");
            assert!(
                created
                    .get("lastPulledPayloadHash")
                    .is_some_and(Value::is_string)
            );
            let local = guard["following"]
                .as_array()
                .unwrap()
                .iter()
                .find(|item| value_i64(item.get("id")) == 55555)
                .expect("55555 kept");
            assert_eq!(
                local["lastPushedPayloadHash"],
                json!(bangumi_sync::local_collection_hash(local))
            );
        }
        let post = server
            .requests()
            .into_iter()
            .find(|request| {
                request.method == "POST" && request.target == "/v0/users/-/collections/55555"
            })
            .expect("POST create for locally followed subject");
        let payload: Value = serde_json::from_str(&post.body).unwrap();
        assert_eq!(payload["type"], 3);
        assert_eq!(payload["rate"], 9);

        // 第二次同步：拉取 hash 命中跳过 + 写回 hash 命中跳过 → 零写请求。
        let second = rt.block_on(bangumi_sync::run_bangumi_collection_sync(
            &client,
            &tokens,
            &username_cache,
            &state,
            &offline,
        ));
        let push2 = rt.block_on(bangumi_sync::push_local_changes(
            &client,
            &tokens,
            &username_cache,
            &state,
            &episodes_cache,
        ));
        let writes_second = write_count(&server.requests());
        assert_eq!(writes_second, writes_first, "second sync must not write");
        assert_eq!(second.followed, 0);
        assert_eq!(second.pulled, 1);
        assert_eq!(push2.pushed, 0);
    }

    #[cfg(feature = "standard")]
    #[test]
    fn bangumi_pull_maps_all_collection_types() {
        use crate::bangumi::test_support::MockBangumiServer;

        let profile = include_str!("../fixtures/bangumi/user-profile.json").to_string();
        let collections = collection_page(json!([
            // 本地已有：doing → 状态/评分字段合并。
            {"subject_id": 45678, "subject_type": 2, "rate": 8, "type": 3,
             "tags": [], "ep_status": 3, "private": false},
            // 本地已有：dropped → 取消追番（只删未完成）。
            {"subject_id": 99999, "subject_type": 2, "rate": 0, "type": 5,
             "tags": [], "ep_status": 1, "private": false},
            // 本地已有：done → 补完成 episode<=ep_status 的 pending。
            {"subject_id": 11111, "subject_type": 2, "rate": 0, "type": 2,
             "tags": [], "ep_status": 2, "private": false},
            // 本地已有：wish / on_hold → 仅建议，追番状态不动。
            {"subject_id": 22222, "subject_type": 2, "rate": null, "type": 1,
             "tags": [], "ep_status": 0, "private": false, "subject": slim_subject(22222, " wishing 甲")},
            {"subject_id": 33333, "subject_type": 2, "rate": null, "type": 4,
             "tags": [], "ep_status": 0, "private": false, "subject": slim_subject(33333, "搁置 乙")},
            // 本地无 + 墓碑：doing 当前仍是远端在看，应恢复本地追番；
            // 墓碑只代表旧的本地取消意图。
            {"subject_id": 44444, "subject_type": 2, "rate": null, "type": 3,
             "tags": [], "ep_status": 0, "private": false, "subject": slim_subject(44444, "被删 丙")},
            // 本地无：doing → 创建 following（内嵌 SlimSubject）。
            {"subject_id": 55555, "subject_type": 2, "rate": 6, "type": 3,
             "tags": [], "ep_status": 4, "private": false, "subject": slim_subject(55555, "新增 丁")}
        ]));
        let server =
            MockBangumiServer::spawn(Arc::new(move |_method, target, _headers, _request_body| {
                if target == "/v0/me" {
                    return (200, vec![], profile.clone());
                }
                if target.starts_with("/v0/users/anilog_dev/collections?") {
                    return (200, vec![], collections.clone());
                }
                (404, vec![], "{}".into())
            }));
        let mut state = phase3_state("https://unused.example.com/v0");
        state["following"] = json!([
            bangumi_following(45678, json!({})),
            bangumi_following(99999, json!({})),
            bangumi_following(11111, json!({})),
            bangumi_following(22222, json!({})),
            bangumi_following(33333, json!({}))
        ]);
        state["tasks"] = json!([
            {"id": "99999-1", "animeId": 99999, "animeTitle": "示例 99999", "episode": 1,
             "airingAt": 10, "status": "pending", "createdAt": 10, "completedAt": null, "syncUpdatedAt": 1},
            {"id": "99999-2", "animeId": 99999, "animeTitle": "示例 99999", "episode": 2,
             "airingAt": 10, "status": "completed", "createdAt": 10, "completedAt": 20, "syncUpdatedAt": 1},
            {"id": "11111-1", "animeId": 11111, "animeTitle": "示例 11111", "episode": 1,
             "airingAt": 10, "status": "pending", "createdAt": 10, "completedAt": null, "syncUpdatedAt": 1},
            {"id": "11111-3", "animeId": 11111, "animeTitle": "示例 11111", "episode": 3,
             "airingAt": 10, "status": "pending", "createdAt": 10, "completedAt": null, "syncUpdatedAt": 1}
        ]);
        state["syncMetadata"]["followingDeletedAt"]["44444"] = json!(now_millis());
        let state = std::sync::Mutex::new(state);
        let tokens = bangumi::MemoryTokenStore::new();
        tokens.store("pull-token").unwrap();
        let username_cache = std::sync::Mutex::new(None);
        let client =
            bangumi::HttpBangumiClient::with_base(bangumi_test_base(&server.url())).unwrap();
        let offline = json!({"bySubject": {}, "anilistIndex": {}});

        let report = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio test runtime")
            .block_on(bangumi_sync::run_bangumi_collection_sync(
                &client,
                &tokens,
                &username_cache,
                &state,
                &offline,
            ));

        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(report.pulled, 7);
        assert_eq!(report.followed, 2);
        assert_eq!(report.unfollowed, 1);
        assert_eq!(report.completed_tasks, 1);
        assert_eq!(report.conflicts, 0);
        let mut suggestions = report.suggestions.clone();
        suggestions.sort_by_key(|suggestion| suggestion.subject_id);
        let rendered: Vec<(i64, u32)> = suggestions
            .iter()
            .map(|suggestion| (suggestion.subject_id, suggestion.collection_type))
            .collect();
        assert_eq!(rendered, vec![(22222, 1), (33333, 4)]);
        assert_eq!(suggestions[0].name_cn.as_deref(), Some(" wishing 甲"));

        let guard = state.lock().unwrap();
        let entry = |id: i64| {
            guard["following"]
                .as_array()
                .unwrap()
                .iter()
                .find(|item| value_i64(item.get("id")) == id)
                .cloned()
        };
        // doing：状态字段 + 评分合并，追番保持。
        let doing = entry(45678).expect("45678 kept");
        assert_eq!(doing["bangumiStatus"], "doing");
        assert_eq!(doing["rating"], 8);
        assert_eq!(doing["lastChangedBy"], "bangumi");
        assert!(doing.get("watchedEpisode").is_none_or(Value::is_null));
        // dropped：取消追番（条目删除、未完成删、已完成保留、墓碑写入）。
        assert!(entry(99999).is_none());
        let tasks = guard["tasks"].as_array().unwrap();
        assert!(
            !tasks
                .iter()
                .any(|task| value_string(task.get("id")) == "99999-1")
        );
        assert!(
            tasks
                .iter()
                .any(|task| value_string(task.get("id")) == "99999-2")
        );
        assert!(value_i64(guard["syncMetadata"]["followingDeletedAt"].get("99999")) > 0);
        // 远端驱动取消不进取消队列（防写回循环）。
        assert!(guard.get("pendingBangumiUnfollows").is_none_or(|queue| {
            queue.as_array().is_none_or(|items| {
                items
                    .iter()
                    .all(|item| value_i64(item.get("subjectId")) != 99999)
            })
        }));
        // done：episode<=ep_status 的 pending 补完成，超出范围保留 pending；
        // 不新建任务；watchedEpisode=ep_status；rate=0 → rating=0。
        let done = entry(11111).expect("11111 kept");
        assert_eq!(done["bangumiStatus"], "done");
        assert_eq!(done["watchedEpisode"], 2);
        assert_eq!(done["rating"], 0);
        let completed = tasks
            .iter()
            .find(|task| value_string(task.get("id")) == "11111-1")
            .expect("11111-1 completed");
        assert_eq!(completed["status"], "completed");
        assert_eq!(completed["lastChangedBy"], "bangumi");
        let beyond = tasks
            .iter()
            .find(|task| value_string(task.get("id")) == "11111-3")
            .expect("11111-3 stays pending");
        assert_eq!(beyond["status"], "pending");
        assert_eq!(tasks.len(), 3, "no new tasks created");
        // wish / on_hold：追番状态不动，仅 bangumiStatus 字段同步。
        assert!(entry(22222).is_some());
        assert_eq!(entry(22222).unwrap()["bangumiStatus"], "wish");
        assert!(entry(33333).is_some());
        assert_eq!(entry(33333).unwrap()["bangumiStatus"], "on_hold");
        // 远端 doing 会恢复本地追番，即使本地残留旧墓碑。
        let revived = entry(44444).expect("44444 revived from remote doing");
        assert_eq!(revived["bangumiStatus"], "doing");
        // doing 无墓碑 → 新建 following（复用 bangumi 构造 + 收藏字段）。
        let created = entry(55555).expect("55555 created");
        assert_eq!(created["source"], "bangumi");
        assert_eq!(created["bangumiStatus"], "doing");
        assert_eq!(created["rating"], 6);
        assert_eq!(created["watchedEpisode"], 4);
        assert_eq!(created["siteUrl"], "https://bgm.tv/subject/55555");
        assert_eq!(created["lastChangedBy"], "bangumi");
        assert!(
            created
                .get("lastPulledPayloadHash")
                .is_some_and(Value::is_string)
        );
    }

    #[cfg(feature = "standard")]
    #[test]
    fn bangumi_pull_changes_never_push_and_local_changes_do() {
        use crate::bangumi::test_support::MockBangumiServer;

        let profile = include_str!("../fixtures/bangumi/user-profile.json").to_string();
        let collections = collection_page(json!([{
            "subject_id": 45678, "subject_type": 2, "rate": 8, "type": 3,
            "tags": [], "ep_status": 3, "private": false,
            "subject": slim_subject(45678, "示例 45678")
        }]));
        // 有状态 mock：记录远端已创建的收藏，驱动单条读取探测（404/200）。
        let created: Arc<Mutex<HashSet<i64>>> = Arc::new(Mutex::new(HashSet::new()));
        let created_handler = created.clone();
        let server =
            MockBangumiServer::spawn(Arc::new(move |_method, target, _headers, _request_body| {
                if target == "/v0/me" {
                    return (200, vec![], profile.clone());
                }
                if target.starts_with("/v0/users/anilog_dev/collections?") {
                    return (200, vec![], collections.clone());
                }
                if let Some(rest) = target.strip_prefix("/v0/users/anilog_dev/collections/") {
                    let subject_id: i64 = rest.parse().unwrap_or(0);
                    return if created_handler.lock().unwrap().contains(&subject_id) {
                        (
                            200,
                            vec![],
                            json!({"subject_id": subject_id, "type": 3}).to_string(),
                        )
                    } else {
                        (404, vec![], "{}".into())
                    };
                }
                if target.starts_with("/v0/users/-/collections/") {
                    let rest = &target["/v0/users/-/collections/".len()..];
                    let subject_id: i64 = rest
                        .split('/')
                        .next()
                        .and_then(|value| value.parse().ok())
                        .unwrap_or(0);
                    created_handler.lock().unwrap().insert(subject_id);
                    return (204, vec![], String::new());
                }
                (404, vec![], "{}".into())
            }));
        let state = std::sync::Mutex::new(phase3_state("https://unused.example.com/v0"));
        let tokens = bangumi::MemoryTokenStore::new();
        tokens.store("loop-token").unwrap();
        let username_cache = std::sync::Mutex::new(None);
        let client =
            bangumi::HttpBangumiClient::with_base(bangumi_test_base(&server.url())).unwrap();
        let offline = json!({"bySubject": {}, "anilistIndex": {}});
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio test runtime");

        let episodes_cache = episodes_cache_dir("pull-push-loop");

        // 防循环：拉取来的变更（lastChangedBy=bangumi）在写回阶段零请求。
        let pull = rt.block_on(bangumi_sync::run_bangumi_collection_sync(
            &client,
            &tokens,
            &username_cache,
            &state,
            &offline,
        ));
        assert_eq!(pull.followed, 1);
        let push = rt.block_on(bangumi_sync::push_local_changes(
            &client,
            &tokens,
            &username_cache,
            &state,
            &episodes_cache,
        ));
        assert_eq!(push.pushed, 0, "pull-driven changes must not push back");
        assert_eq!(write_count(&server.requests()), 0);

        // 本地追番（lastChangedBy=local，无拉取基线）→ POST 创建 type=3。
        {
            let mut guard = state.lock().unwrap();
            guard["following"]
                .as_array_mut()
                .unwrap()
                .push(bangumi_following(77777, json!({})));
        }
        let push = rt.block_on(bangumi_sync::push_local_changes(
            &client,
            &tokens,
            &username_cache,
            &state,
            &episodes_cache,
        ));
        assert_eq!(push.pushed, 1);
        let post = server
            .requests()
            .into_iter()
            .find(|request| {
                request.method == "POST" && request.target == "/v0/users/-/collections/77777"
            })
            .expect("POST create");
        let payload: Value = serde_json::from_str(&post.body).unwrap();
        assert_eq!(payload["type"], 3);

        // 本地评分变化 → PATCH rate（hash 变化触发）。
        {
            let mut guard = state.lock().unwrap();
            let index = guard["following"]
                .as_array()
                .unwrap()
                .iter()
                .position(|item| value_i64(item.get("id")) == 77777)
                .unwrap();
            guard["following"][index]["rating"] = json!(7);
            guard["following"][index]["lastChangedBy"] = json!("local");
        }
        let push = rt.block_on(bangumi_sync::push_local_changes(
            &client,
            &tokens,
            &username_cache,
            &state,
            &episodes_cache,
        ));
        assert_eq!(push.pushed, 1);
        let patch = server
            .requests()
            .into_iter()
            .find(|request| {
                request.method == "PATCH" && request.target == "/v0/users/-/collections/77777"
            })
            .expect("PATCH rating");
        let payload: Value = serde_json::from_str(&patch.body).unwrap();
        assert_eq!(payload["type"], 3);
        assert_eq!(payload["rate"], 7);

        // 本地取消追番 → 队列入列 → PATCH type=5 → 推送成功清除队列。
        {
            let mut guard = state.lock().unwrap();
            assert!(remove_following(&mut guard, 77777));
            assert!(
                guard
                    .get("pendingBangumiUnfollows")
                    .and_then(Value::as_array)
                    .is_some_and(|items| items
                        .iter()
                        .any(|item| value_i64(item.get("subjectId")) == 77777))
            );
        }
        let push = rt.block_on(bangumi_sync::push_local_changes(
            &client,
            &tokens,
            &username_cache,
            &state,
            &episodes_cache,
        ));
        assert_eq!(push.pushed, 1);
        let dropped = server
            .requests()
            .into_iter()
            .find(|request| {
                request.method == "PATCH"
                    && request.target == "/v0/users/-/collections/77777"
                    && request.body.contains("\"type\":5")
            })
            .expect("PATCH type=5 for local unfollow");
        let payload: Value = serde_json::from_str(&dropped.body).unwrap();
        assert_eq!(payload["type"], 5);
        {
            let guard = state.lock().unwrap();
            assert!(
                guard
                    .get("pendingBangumiUnfollows")
                    .is_none_or(|queue| queue.as_array().is_none_or(|items| items.is_empty()))
            );
        }
    }

    #[cfg(feature = "standard")]
    #[test]
    fn bangumi_push_completed_episodes_binds_episode_ids_then_batch_is_idempotent() {
        use crate::bangumi::test_support::MockBangumiServer;

        // subject-episodes.json：id 98765(sort 4) / 98766(sort 25, SP) /
        // 98767(sort 5)——sort 25 用于锁定小数/特殊集不错配整数集数。
        let episodes = include_str!("../fixtures/bangumi/subject-episodes.json").to_string();
        let server =
            MockBangumiServer::spawn(Arc::new(move |_method, target, _headers, _request_body| {
                if target.starts_with("/v0/episodes?subject_id=45678") {
                    return (200, vec![], episodes.clone());
                }
                if target == "/v0/users/-/collections/45678/episodes" {
                    return (204, vec![], String::new());
                }
                (404, vec![], "{}".into())
            }));
        let mut state = phase3_state("https://unused.example.com/v0");
        state["bangumi"]["pushLocalChanges"] = json!(false);
        state["bangumi"]["pushCompletedEpisodes"] = json!(false);
        state["tasks"] = json!([
            // 本地完成、缺 episodeId → 经集数列表解析绑定（episode 5 → sort 5
            // → 98767），随批量 PATCH 上传。
            {"id": "45678-5", "animeId": 45678, "animeTitle": "示例", "episode": 5,
             "airingAt": 10, "status": "completed", "createdAt": 10, "completedAt": 20,
             "syncUpdatedAt": 1, "subjectId": 45678, "episodeId": null},
            // 已带 episodeId → 不覆盖、照常上传（98765）。
            {"id": "45678-4", "animeId": 45678, "animeTitle": "示例", "episode": 4,
             "airingAt": 10, "status": "completed", "createdAt": 10, "completedAt": 20,
             "syncUpdatedAt": 1, "subjectId": 45678, "episodeId": 98765},
            // 拉取来的完成（lastChangedBy=bangumi）→ 不绑定也不上传。
            {"id": "45678-3", "animeId": 45678, "animeTitle": "示例", "episode": 3,
             "airingAt": 10, "status": "completed", "createdAt": 10, "completedAt": 20,
             "syncUpdatedAt": 1, "subjectId": 45678, "episodeId": null,
             "lastChangedBy": "bangumi"}
        ]);
        let state = std::sync::Mutex::new(state);
        let tokens = bangumi::MemoryTokenStore::new();
        tokens.store("episodes-token").unwrap();
        let username_cache = std::sync::Mutex::new(None);
        let client =
            bangumi::HttpBangumiClient::with_base(bangumi_test_base(&server.url())).unwrap();
        let episodes_cache = episodes_cache_dir("episode-bind");
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio test runtime");

        // pushCompletedEpisodes=false：不解析、不上传，零请求。
        let report = rt.block_on(bangumi_sync::push_local_changes(
            &client,
            &tokens,
            &username_cache,
            &state,
            &episodes_cache,
        ));
        assert_eq!(report.pushed, 0);
        assert!(report.errors.is_empty());
        assert_eq!(server.requests().len(), 0);

        // 开启后：先经集数列表解析绑定 episode 5 → 98767，再聚合同 subject
        // 批量 PATCH（episode_id 数组 + type=2）。
        state.lock().unwrap()["bangumi"]["pushCompletedEpisodes"] = json!(true);
        let report = rt.block_on(bangumi_sync::push_local_changes(
            &client,
            &tokens,
            &username_cache,
            &state,
            &episodes_cache,
        ));
        assert_eq!(report.pushed, 1);
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        let episodes_gets = server
            .requests()
            .iter()
            .filter(|request| {
                request.method == "GET" && request.target.starts_with("/v0/episodes?")
            })
            .count();
        assert_eq!(episodes_gets, 1, "episodes list fetched exactly once");
        {
            let guard = state.lock().unwrap();
            let bound = guard["tasks"]
                .as_array()
                .unwrap()
                .iter()
                .find(|task| value_string(task.get("id")) == "45678-5")
                .unwrap();
            assert_eq!(bound["episodeId"], json!(98767), "episode 5 → sort 5");
            assert!(
                bound
                    .get("lastPushedToBangumiAt")
                    .is_some_and(Value::is_number)
            );
            // 拉取来源任务不绑定、不推送。
            let pulled = guard["tasks"]
                .as_array()
                .unwrap()
                .iter()
                .find(|task| value_string(task.get("id")) == "45678-3")
                .unwrap();
            assert!(pulled["episodeId"].is_null());
        }
        let request = server
            .requests()
            .into_iter()
            .find(|request| {
                request.method == "PATCH"
                    && request.target == "/v0/users/-/collections/45678/episodes"
            })
            .expect("batch PATCH");
        let payload: Value = serde_json::from_str(&request.body).unwrap();
        let mut pushed_ids = payload["episode_id"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_i64)
            .collect::<Vec<_>>();
        pushed_ids.sort_unstable();
        assert_eq!(pushed_ids, vec![98765, 98767]);
        assert_eq!(payload["type"], 2);

        // hash 幂等：再次推送零请求（无新候选任务）。
        let report = rt.block_on(bangumi_sync::push_local_changes(
            &client,
            &tokens,
            &username_cache,
            &state,
            &episodes_cache,
        ));
        assert_eq!(report.pushed, 0);
        assert_eq!(server.requests().len(), 2, "1 GET episodes + 1 PATCH");

        // 缓存命中：新完成集数 4（无 episodeId 的新任务）→ 解析走 24h 缓存，
        // 零新增 GET，仅新增一次批量 PATCH。
        {
            let mut guard = state.lock().unwrap();
            guard["tasks"].as_array_mut().unwrap().push(json!({
                "id": "45678-6", "animeId": 45678, "animeTitle": "示例", "episode": 4,
                "airingAt": 10, "status": "completed", "createdAt": 30, "completedAt": 40,
                "syncUpdatedAt": 1, "subjectId": 45678, "episodeId": null
            }));
        }
        let report = rt.block_on(bangumi_sync::push_local_changes(
            &client,
            &tokens,
            &username_cache,
            &state,
            &episodes_cache,
        ));
        assert_eq!(report.pushed, 1, "new completion pushed via cached mapping");
        assert_eq!(server.requests().len(), 3);
        let episodes_gets = server
            .requests()
            .iter()
            .filter(|request| {
                request.method == "GET" && request.target.starts_with("/v0/episodes?")
            })
            .count();
        assert_eq!(episodes_gets, 1, "second resolve must hit the 24h cache");
    }

    #[cfg(feature = "standard")]
    #[test]
    fn bangumi_push_completed_episodes_skips_subject_on_resolution_failure() {
        use crate::bangumi::test_support::MockBangumiServer;

        // 45678 集数列表拉取失败（500）→ 该条目本轮跳过进度写回（记 errors）；
        // 55555 正常解析（episode 4 → sort 4 → 98765）→ 不阻断其他条目。
        let episodes = include_str!("../fixtures/bangumi/subject-episodes.json").to_string();
        let server =
            MockBangumiServer::spawn(Arc::new(move |_method, target, _headers, _request_body| {
                if target.starts_with("/v0/episodes?subject_id=55555") {
                    return (200, vec![], episodes.clone());
                }
                if target.starts_with("/v0/episodes?subject_id=45678") {
                    return (500, vec![], "{}".into());
                }
                if target == "/v0/users/-/collections/55555/episodes" {
                    return (204, vec![], String::new());
                }
                (404, vec![], "{}".into())
            }));
        let mut state = phase3_state("https://unused.example.com/v0");
        state["bangumi"]["pushLocalChanges"] = json!(false);
        state["tasks"] = json!([
            {"id": "45678-2", "animeId": 45678, "animeTitle": "示例", "episode": 2,
             "airingAt": 10, "status": "completed", "createdAt": 10, "completedAt": 20,
             "syncUpdatedAt": 1, "subjectId": 45678, "episodeId": null},
            {"id": "55555-4", "animeId": 55555, "animeTitle": "示例", "episode": 4,
             "airingAt": 10, "status": "completed", "createdAt": 10, "completedAt": 20,
             "syncUpdatedAt": 1, "subjectId": 55555, "episodeId": null}
        ]);
        let state = std::sync::Mutex::new(state);
        let tokens = bangumi::MemoryTokenStore::new();
        tokens.store("resolve-fail-token").unwrap();
        let username_cache = std::sync::Mutex::new(None);
        let client =
            bangumi::HttpBangumiClient::with_base(bangumi_test_base(&server.url())).unwrap();
        let episodes_cache = episodes_cache_dir("resolve-failure");
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio test runtime");

        let report = rt.block_on(bangumi_sync::push_local_changes(
            &client,
            &tokens,
            &username_cache,
            &state,
            &episodes_cache,
        ));
        assert_eq!(report.pushed, 1, "55555 must still push");
        assert_eq!(report.errors.len(), 1, "{:?}", report.errors);
        assert!(report.errors[0].contains("45678"), "{:?}", report.errors);
        let guard = state.lock().unwrap();
        let skipped = guard["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|task| value_string(task.get("id")) == "45678-2")
            .unwrap();
        assert!(skipped["episodeId"].is_null(), "unresolved task untouched");
        let bound = guard["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|task| value_string(task.get("id")) == "55555-4")
            .unwrap();
        assert_eq!(bound["episodeId"], json!(98765));
        let patch = server
            .requests()
            .into_iter()
            .find(|request| request.method == "PATCH")
            .expect("PATCH for resolvable subject");
        assert_eq!(request_target_subject(&patch.target), 55555);
    }

    /// PATCH 目标 `/v0/users/-/collections/{sid}/episodes` 的 sid 提取（测试辅助）。
    #[cfg(feature = "standard")]
    fn request_target_subject(target: &str) -> i64 {
        target
            .trim_start_matches("/v0/users/-/collections/")
            .split('/')
            .next()
            .and_then(|value| value.parse().ok())
            .unwrap_or(0)
    }

    /// 验收第 4 轮问题 2：sort → 任务集数匹配规则。整数 sort 直接匹配；
    /// 小数 sort（SP "4.5"）不得错配相邻整数；同键冲突取更贴近整数的记录。
    #[cfg(feature = "standard")]
    #[test]
    fn episode_id_map_matches_integer_sorts_only() {
        let episodes = vec![
            bangumi::BangumiEpisode {
                id: 1,
                sort: Some(4.0),
                ..Default::default()
            },
            bangumi::BangumiEpisode {
                id: 2,
                sort: Some(4.5),
                ..Default::default()
            },
            bangumi::BangumiEpisode {
                id: 3,
                sort: Some(5.0),
                ..Default::default()
            },
            bangumi::BangumiEpisode {
                id: 4,
                sort: Some(25.0),
                ..Default::default()
            },
            bangumi::BangumiEpisode {
                id: 5,
                sort: Some(4.9),
                ..Default::default()
            },
            bangumi::BangumiEpisode {
                id: 6,
                sort: None,
                ..Default::default()
            },
            bangumi::BangumiEpisode {
                id: 7,
                sort: Some(0.0),
                ..Default::default()
            },
        ];
        let map = bangumi_sync::episode_id_map(&episodes);
        assert_eq!(map.get(&4), Some(&1), "整数 sort 直接匹配");
        assert_eq!(
            map.get(&5),
            Some(&3),
            "同键冲突取更贴近整数的记录（5.0 胜 4.9）"
        );
        assert_eq!(map.get(&25), Some(&4), "SP 25 只匹配 25");
        assert!(!map.contains_key(&2));
        assert_eq!(map.len(), 3, "4.5 / 无 sort / 0.0 不映射");
    }

    #[cfg(feature = "standard")]
    #[test]
    fn episode_number_prefers_local_ep_over_global_sort_for_split_subjects() {
        let episodes = vec![
            bangumi::BangumiEpisode {
                id: 10,
                ep: Some(5.0),
                sort: Some(82.0),
                ..Default::default()
            },
            bangumi::BangumiEpisode {
                id: 11,
                ep: None,
                sort: Some(83.0),
                ..Default::default()
            },
        ];
        let map = bangumi_sync::episode_id_map(&episodes);
        assert_eq!(map.get(&5), Some(&10));
        assert_eq!(map.get(&83), Some(&11));
        assert!(!map.contains_key(&82));

        let records = [bangumi::BangumiEpisode {
            id: 12,
            ep: Some(5.0),
            sort: Some(82.0),
            airdate: Some("2026-09-12".into()),
            ..Default::default()
        }];
        let precise = HashMap::from([(82, at("2026-09-12T13:30:00+00:00"))]);
        let schedule = bangumi_episode_schedule_with_precision(
            &records,
            at("2026-09-08T12:00:00+00:00"),
            &precise,
        );
        assert_eq!(
            schedule,
            vec![(5, 12, at("2026-09-12T13:30:00+00:00"), false)]
        );
    }

    /// 验收第 4 轮问题 1b：离线锚点与 AniList 权威冲突时，任务生成被钳制——
    /// 只生成 episode < AniList nextAiringEpisode.episode 的已播集；无 AniList
    /// 数据时维持离线推算（含 airingAt == now 边界、未来集不生成）。
    #[cfg(all(feature = "standard", not(target_os = "android")))]
    #[test]
    fn bangumi_offline_schedules_clamp_to_anilist_next_episode() {
        let mut state = default_state(false);
        // eps 未知 → 无 eps 钳制，仅权威边界生效。
        state["following"] = json!([{
            "id": 45678, "source": "bangumi", "displayTitle": "示例",
            "followedAt": 0, "syncUpdatedAt": 1
        }]);
        state["tasks"] = json!([]);
        state["seenAiringEvents"] = json!([]);
        let map = json!({"bySubject": {
            "45678": offline_entry(
                21355,
                json!("2026-04-01T13:00:22Z"),
                json!("R/2026-04-01T13:00:22.000Z/P7D"),
                Value::Null
            )
        }});
        // begin + 20 整周 → 第 21 期恰在 now（边界内）。
        let now = at("2026-08-19T13:00:22+00:00");

        // AniList 权威下一期 = 16（认为 16-21 未播）→ 只生成 1..15，
        // media.nextAiringEpisode 维持 AniList 权威值。
        let authoritative = json!({
            "episode": 16,
            "airingAt": at("2026-09-09T13:00:22+00:00"),
            "timeUntilAiring": 100
        });
        state["following"][0]["nextAiringEpisode"] = authoritative.clone();
        let schedules = bangumi_offline_schedules(&state, &map, now);
        let episodes: Vec<i64> = schedules
            .iter()
            .map(|schedule| value_i64(schedule.get("episode")))
            .collect();
        assert_eq!(episodes, (1..=15).collect::<Vec<_>>());
        assert_eq!(schedules[0]["media"]["nextAiringEpisode"], authoritative);

        // 无 AniList 数据（null）→ 离线推算不受钳制：1..=21 全部生成
        // （第 21 期 airingAt == now 边界内），未来集 22 只进 nextAiringEpisode。
        state["following"][0]["nextAiringEpisode"] = Value::Null;
        let schedules = bangumi_offline_schedules(&state, &map, now);
        assert_eq!(schedules.len(), 21);
        assert_eq!(schedules[20]["episode"], 21);
        assert_eq!(schedules[20]["airingAt"], now);
        assert_eq!(schedules[20]["media"]["nextAiringEpisode"]["episode"], 22);
    }

    /// 验收第 4 轮问题 1（存量清理）：reconcile_following_entries 删除
    /// pending 且 airingAt > now 的任务（从未播出的集不应是待看任务），
    /// 已完成任务与已播出任务保留，且幂等。
    #[cfg(feature = "standard")]
    #[test]
    fn reconcile_purges_unaired_pending_tasks_and_is_idempotent() {
        let mut state = default_state(false);
        state["following"] = json!([{
            "id": 45678, "source": "bangumi", "displayTitle": "示例",
            "followedAt": 0, "syncUpdatedAt": 1
        }]);
        let now = now_seconds();
        let future = now + 3_600;
        let past = now - 3_600;
        state["tasks"] = json!([
            // 未播出的 pending（污染形态）→ 清理。
            {"id": "45678-23", "animeId": 45678, "episode": 23, "airingAt": future,
             "status": "pending", "createdAt": 1, "completedAt": Value::Null, "syncUpdatedAt": 1},
            // 已播出的 pending → 保留。
            {"id": "45678-22", "animeId": 45678, "episode": 22, "airingAt": past,
             "status": "pending", "createdAt": 1, "completedAt": Value::Null, "syncUpdatedAt": 1},
            // 未来 airingAt 的 completed → 观看历史，永不删除。
            {"id": "45678-21", "animeId": 45678, "episode": 21, "airingAt": future,
             "status": "completed", "createdAt": 1, "completedAt": 2, "syncUpdatedAt": 1},
            // 无 airingAt 的 pending（无播出信息）→ 不误删。
            {"id": "45678-20", "animeId": 45678, "episode": 20,
             "status": "pending", "createdAt": 1, "completedAt": Value::Null, "syncUpdatedAt": 1}
        ]);
        let map = json!({"bySubject": {}, "anilistIndex": {}});

        assert!(reconcile_following_entries(&mut state, &map, false));
        let ids: Vec<String> = state["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|task| value_string(task.get("id")))
            .collect();
        assert_eq!(ids, vec!["45678-22", "45678-21", "45678-20"]);

        // 幂等：再次 reconcile 任务集不再变化。
        let before = state["tasks"].clone();
        reconcile_following_entries(&mut state, &map, false);
        assert_eq!(state["tasks"], before);
    }

    // -- 权威数据修复 1：AniList 权威化调度 -----------------------------------
    // 真实用户数据根因：黄泉的使者（bangumi 568572, anilistId=195600）的
    // nextAiringEpisode 与任务 airingAt 被 bangumi_offline_schedules 的离线
    // begin/broadcast 锚点（流媒体上线时段，周历错位 3-6 天）每轮覆写——
    // ep23 实测 9/6 CST，Bangumi 官方 /v0/episodes 为 2026-09-12；已完成集的
    // 正确时间全部来自 AniList AIRING_QUERY。修复后 anilistId 非空条目的播出
    // 数据完全由 AIRING_QUERY 提供。

    #[cfg(feature = "standard")]
    #[test]
    fn offline_schedules_skip_anilist_bound_entries_and_airing_query_rewrites_next() {
        let mut state = default_state(false);
        state["following"] = json!([{
            "id": 568572, "source": "bangumi", "anilistId": 195600, "bangumiId": 568572,
            "displayTitle": "黄泉的使者",
            "episodes": 13, "followedAt": 1, "syncUpdatedAt": 1,
            // 存量污染形态：离线锚点覆写的错误下一期（ep23@9/6 CST）。
            "nextAiringEpisode": {
                "episode": 23, "airingAt": at("2026-09-06T00:00:00+08:00")
            }
        }]);
        state["tasks"] = json!([]);
        state["seenAiringEvents"] = json!([]);
        // 离线映射存在完整 begin/broadcast 数据——修复前会为其逐期生成调度。
        let map = json!({
            "bySubject": {
                "568572": offline_entry(
                    195600,
                    json!("2026-08-16T15:00:00Z"),
                    json!("R/2026-08-16T15:00:00.000Z/P7D"),
                    Value::Null
                )
            },
            "anilistIndex": {"195600": 568572}
        });
        let now = at("2026-09-06T12:00:00+08:00");

        // a) anilistId 非空 → 离线调度零生成（不再覆写 nextAiringEpisode）。
        assert!(bangumi_offline_schedules(&state, &map, now).is_empty());

        // b) AIRING_QUERY 结果灌入：已播 ep22（官方 9/5 22:30 CST）建任务，
        //    media.nextAiringEpisode（AniList 权威 ep23@2026-09-12）写回条目，
        //    替换污染值 9/6。
        let schedules = json!({
            "mediaId": 195600,
            "episode": 22,
            "airingAt": at("2026-09-05T22:30:00+08:00"),
            "media": {
                "nextAiringEpisode": {
                    "episode": 23,
                    "airingAt": at("2026-09-12T00:00:00+08:00"),
                    "timeUntilAiring": 0
                }
            }
        });
        let outcome = apply_airing_schedules(&mut state, &[schedules], now);
        assert_eq!(
            outcome,
            AiringOutcome {
                aired: 1,
                created: 1
            }
        );
        assert_eq!(state["tasks"][0]["id"], "568572-22");
        assert_eq!(state["tasks"][0]["animeId"], 568572);
        assert_eq!(
            state["tasks"][0]["airingAt"],
            at("2026-09-05T22:30:00+08:00")
        );
        assert_eq!(state["following"][0]["nextAiringEpisode"]["episode"], 23);
        assert_eq!(
            state["following"][0]["nextAiringEpisode"]["airingAt"],
            at("2026-09-12T00:00:00+08:00")
        );
    }

    #[cfg(feature = "standard")]
    #[test]
    fn shared_anilist_id_assigns_schedule_to_primary_claimant_only() {
        // 丧失篇（bangumi 547888, eps=11）与夺还篇（bangumi 633836）共用
        // anilistId 189046（分季课程共占一个 AniList 条目）：本轮调度只分配给
        // 主条目（anilistIndex 指向者优先），非主条目零新任务、
        // nextAiringEpisode 不被写回。
        let mut state = default_state(false);
        state["following"] = json!([
            {"id": 547888, "source": "bangumi", "anilistId": 189046, "bangumiId": 547888,
             "displayTitle": "丧失篇", "episodes": 11, "followedAt": 1_000, "syncUpdatedAt": 1},
            {"id": 633836, "source": "bangumi", "anilistId": 189046, "bangumiId": 633836,
             "displayTitle": "夺还篇", "episodes": 12, "followedAt": 2_000, "syncUpdatedAt": 1,
             "nextAiringEpisode": {"episode": 15, "airingAt": 1}}
        ]);
        state["tasks"] = json!([]);
        state["seenAiringEvents"] = json!([]);
        let map = json!({
            "bySubject": {
                "547888": offline_entry(189046, Value::Null, Value::Null, Value::Null),
                "633836": offline_entry(189046, Value::Null, Value::Null, Value::Null)
            },
            "anilistIndex": {"189046": 547888}
        });

        let secondary = secondary_anilist_claimant_ids(&state, &map);
        assert_eq!(secondary, HashSet::from([633836]));

        let schedules = json!({
            "mediaId": 189046,
            "episode": 12,
            "airingAt": at("2026-09-06T21:00:00+08:00"),
            "media": {"nextAiringEpisode": {
                "episode": 13, "airingAt": at("2026-09-13T21:00:00+08:00")
            }}
        });
        let outcome = apply_airing_schedules_inner(
            &mut state,
            &[schedules],
            at("2026-09-06T22:00:00+08:00"),
            &secondary,
        );
        assert_eq!(
            outcome,
            AiringOutcome {
                aired: 1,
                created: 1
            }
        );
        let task_ids: Vec<String> = state["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|task| value_string(task.get("id")))
            .collect();
        assert_eq!(task_ids, vec!["547888-12"]);
        // 主条目获得 AniList 权威 nextAiringEpisode 写回。
        assert_eq!(state["following"][0]["nextAiringEpisode"]["episode"], 13);
        // 非主条目 nextAiringEpisode 不被写回（保持原值）。
        assert_eq!(state["following"][1]["nextAiringEpisode"]["episode"], 15);
    }

    #[cfg(feature = "standard")]
    #[test]
    fn reconcile_purges_eps_overflow_and_secondary_claimant_pending_tasks() {
        // 存量清理：a) 丧失篇 547888（eps=11）的 pending ep15（离线调度越过
        // eps 生成）删除；b) 共享 anilistId 189046 的非主条目 633836 的 pending
        // 删除（防 547888-12..15 与 633836-12..15 这类同集双份）；completed
        // 观看历史一律保留。幂等。
        let mut state = default_state(false);
        state["following"] = json!([
            {"id": 547888, "source": "bangumi", "anilistId": 189046, "bangumiId": 547888,
             "displayTitle": "丧失篇", "episodes": 11, "followedAt": 1_000, "syncUpdatedAt": 1},
            {"id": 633836, "source": "bangumi", "anilistId": 189046, "bangumiId": 633836,
             "displayTitle": "夺还篇", "episodes": 12, "followedAt": 2_000, "syncUpdatedAt": 1}
        ]);
        let past = now_seconds() - 3_600;
        state["tasks"] = json!([
            // a) episode > eps（15 > 11）的 pending → 删除。
            {"id": "547888-15", "animeId": 547888, "episode": 15, "airingAt": past,
             "status": "pending", "createdAt": 1, "completedAt": Value::Null, "syncUpdatedAt": 1},
            // 主条目、eps 内 pending → 保留。
            {"id": "547888-10", "animeId": 547888, "episode": 10, "airingAt": past,
             "status": "pending", "createdAt": 1, "completedAt": Value::Null, "syncUpdatedAt": 1},
            // b) 非主条目 pending → 删除（即使 episode <= eps）。
            {"id": "633836-12", "animeId": 633836, "episode": 12, "airingAt": past,
             "status": "pending", "createdAt": 1, "completedAt": Value::Null, "syncUpdatedAt": 1},
            // completed 历史 → 永不删除（两个条目各保留一条）。
            {"id": "633836-9", "animeId": 633836, "episode": 9, "airingAt": past,
             "status": "completed", "createdAt": 1, "completedAt": 2, "syncUpdatedAt": 1},
            {"id": "547888-9", "animeId": 547888, "episode": 9, "airingAt": past,
             "status": "completed", "createdAt": 1, "completedAt": 2, "syncUpdatedAt": 1}
        ]);
        let map = json!({"bySubject": {}, "anilistIndex": {"189046": 547888}});

        assert!(reconcile_following_entries(&mut state, &map, false));
        let ids: Vec<String> = state["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|task| value_string(task.get("id")))
            .collect();
        assert_eq!(ids, vec!["547888-10", "633836-9", "547888-9"]);

        // 幂等：再次 reconcile 任务集不再变化。
        let before = state["tasks"].clone();
        reconcile_following_entries(&mut state, &map, false);
        assert_eq!(state["tasks"], before);
    }

    // -- 权威数据修复 3：AniList 全量重写 + 任务纠偏（缺口 1/2）-----------------
    // 第 5 轮构建 2b4b672 后用户截图实测：AIRING_QUERY 只返回窗口内播出的集，
    // 窗口内零播出时无 media → next 污染（100女友 ep10@9/9、黄泉 ep24@9/13）
    // 永不纠正；purge 只删 airingAt > now，过去时间假任务（黄泉 ep23@9/6、
    // 描绘 ep10@9/4 23:35、无职 ep11@9/6 23:00）留存。

    /// ANILIST_AUTHORITY_QUERY 的 media 形状（id + next + 已播 schedule）。
    #[cfg(feature = "standard")]
    fn authority_media(id: i64, next: Value, schedule: Value) -> (i64, Value) {
        (
            id,
            json!({
                "id": id,
                "nextAiringEpisode": next,
                "airingSchedule": {"nodes": schedule}
            }),
        )
    }

    #[cfg(feature = "standard")]
    #[test]
    fn production_anilist_authority_never_writes_global_episode_numbers_to_bangumi_entries() {
        // Re:Zero 的两个 Bangumi subject 共用一个 AniList media。AniList 在该
        // media 中使用全集序号 82，而夺还篇必须展示/创建本分季的第 5 集。生产
        // 同步只能把该响应留作分钟级缓存，随后由 Bangumi episode 表按本地 ep
        // 归属，绝不能先写入 82 再等待下一步纠正。
        let mut state = default_state(false);
        state["following"] = json!([{
            "id": 633836, "source": "bangumi", "anilistId": 189046,
            "displayTitle": "夺还篇", "episodes": 8,
            "nextAiringEpisode": {"episode": 5, "airingAt": at("2026-09-12T21:00:00+08:00")}
        }]);
        state["tasks"] = json!([{
            "id": "633836-4", "animeId": 633836, "episode": 4,
            "airingAt": at("2026-09-05T21:00:00+08:00"), "status": "pending"
        }]);
        let before = state.clone();
        let map = json!({"bySubject": {}, "anilistIndex": {"189046": 547888}});
        let media = HashMap::from([authority_media(
            189046,
            json!({"episode": 82, "airingAt": at("2026-09-12T21:00:00+08:00")}),
            json!([{"episode": 81, "airingAt": at("2026-09-05T21:00:00+08:00")}]),
        )]);

        assert!(!apply_anilist_authority_media_inner(
            &mut state,
            &map,
            &media,
            at("2026-09-08T12:00:00+08:00"),
            true,
        ));
        assert_eq!(state, before);
    }

    #[cfg(feature = "standard")]
    #[test]
    fn anilist_authority_rewrites_polluted_next_and_drops_unaired_fake_task() {
        // 黄泉的使者（568572, anilistId=195600）：next 污染 ep24@9/13 + 假任务
        // ep23@9/6 00:00（ep23 真实 9/12 22:30 才播）。权威 media：next=ep23@9/12
        // + schedule [ep22@9/5, ep23@9/12] → next 无条件重写、ep23 假票删除
        // （未播出不该有票，9/12 播出时 AIRING_QUERY 按权威时间重建）、
        // ep22 completed 观看历史不动。
        let mut state = default_state(false);
        state["following"] = json!([{
            "id": 568572, "source": "bangumi", "anilistId": 195600, "bangumiId": 568572,
            "displayTitle": "黄泉的使者", "episodes": 13, "followedAt": 1, "syncUpdatedAt": 1,
            "nextAiringEpisode": {"episode": 24, "airingAt": at("2026-09-13T00:00:00+08:00")}
        }]);
        state["tasks"] = json!([
            {"id": "568572-23", "animeId": 568572, "episode": 23,
             "airingAt": at("2026-09-06T00:00:00+08:00"), "status": "pending",
             "createdAt": 1, "completedAt": Value::Null, "syncUpdatedAt": 1},
            {"id": "568572-22", "animeId": 568572, "episode": 22,
             "airingAt": at("2026-09-05T22:30:00+08:00"), "status": "completed",
             "createdAt": 1, "completedAt": 2, "syncUpdatedAt": 1}
        ]);
        let map = json!({"bySubject": {}, "anilistIndex": {"195600": 568572}});
        let now = at("2026-09-06T12:00:00+08:00");
        let media = HashMap::from([authority_media(
            195600,
            json!({"episode": 23, "airingAt": at("2026-09-12T22:30:00+08:00")}),
            json!([
                {"episode": 22, "airingAt": at("2026-09-05T22:30:00+08:00")},
                {"episode": 23, "airingAt": at("2026-09-12T22:30:00+08:00")}
            ]),
        )]);

        assert!(apply_anilist_authority_media(&mut state, &map, &media, now));

        // next 污染（ep24@9/13）被权威值无条件重写（ep23@9/12 22:30 + 剩余秒数）。
        assert_eq!(state["following"][0]["nextAiringEpisode"]["episode"], 23);
        assert_eq!(
            state["following"][0]["nextAiringEpisode"]["airingAt"],
            at("2026-09-12T22:30:00+08:00")
        );
        assert_eq!(
            state["following"][0]["nextAiringEpisode"]["timeUntilAiring"],
            at("2026-09-12T22:30:00+08:00") - now
        );
        // ep23 假票删除；ep22 已完成观看历史不动。
        let tasks = state["tasks"].as_array().unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0]["id"], "568572-22");
        assert_eq!(tasks[0]["status"], "completed");
        assert_eq!(tasks[0]["airingAt"], at("2026-09-05T22:30:00+08:00"));
    }

    #[cfg(feature = "standard")]
    #[test]
    fn anilist_authority_rewrites_aired_episode_time_from_schedule() {
        // 描绘直至生命尽头（545917, anilistId=163134）：pending ep10@9/4 23:35
        // （离线锚点错位）+ 权威 schedule ep10@9/4 22:30 → 时间纠偏 + 记录
        // syncUpdatedAt=now 毫秒；next 权威值 ep11@9/11 写回条目。
        let mut state = default_state(false);
        state["following"] = json!([{
            "id": 545917, "source": "bangumi", "anilistId": 163134, "bangumiId": 545917,
            "displayTitle": "描绘直至生命尽头", "episodes": 13, "followedAt": 1, "syncUpdatedAt": 1,
            "nextAiringEpisode": {"episode": 10, "airingAt": at("2026-09-04T23:35:00+08:00")}
        }]);
        state["tasks"] = json!([
            {"id": "545917-10", "animeId": 545917, "episode": 10,
             "airingAt": at("2026-09-04T23:35:00+08:00"), "status": "pending",
             "createdAt": 1, "completedAt": Value::Null, "syncUpdatedAt": 1}
        ]);
        let map = json!({"bySubject": {}, "anilistIndex": {"163134": 545917}});
        let now = at("2026-09-04T23:00:00+08:00");
        let media = HashMap::from([authority_media(
            163134,
            json!({"episode": 11, "airingAt": at("2026-09-11T22:30:00+08:00")}),
            json!([{"episode": 10, "airingAt": at("2026-09-04T22:30:00+08:00")}]),
        )]);

        assert!(apply_anilist_authority_media(&mut state, &map, &media, now));

        let task = &state["tasks"][0];
        assert_eq!(task["id"], "545917-10");
        assert_eq!(task["airingAt"], at("2026-09-04T22:30:00+08:00"));
        assert!(value_i64(task.get("syncUpdatedAt")) > 1);
        // 条目 next 同步到 AniList 权威值 ep11@9/11。
        assert_eq!(state["following"][0]["nextAiringEpisode"]["episode"], 11);
        assert_eq!(
            state["following"][0]["nextAiringEpisode"]["airingAt"],
            at("2026-09-11T22:30:00+08:00")
        );
    }

    #[cfg(feature = "standard")]
    #[test]
    fn anilist_authority_drops_unaired_task_and_accepts_legacy_anilist_key() {
        // 无职转生（501963, anilistId=178789）：pending ep11@9/6 23:00 还没播
        // （AniList next=ep11）→ 删除；残留旧键任务（animeId=anilistId，主键
        // 迁移前形态）同样被纠偏口径命中；completed 历史不动。
        let mut state = default_state(false);
        state["following"] = json!([{
            "id": 501963, "source": "bangumi", "anilistId": 178789, "bangumiId": 501963,
            "displayTitle": "无职转生 III", "episodes": 12, "followedAt": 1, "syncUpdatedAt": 1,
            "nextAiringEpisode": {"episode": 12, "airingAt": at("2026-09-13T23:00:00+08:00")}
        }]);
        state["tasks"] = json!([
            {"id": "178789-11", "animeId": 178789, "episode": 11,
             "airingAt": at("2026-09-06T23:00:00+08:00"), "status": "pending",
             "createdAt": 1, "completedAt": Value::Null, "syncUpdatedAt": 1},
            {"id": "501963-10", "animeId": 501963, "episode": 10,
             "airingAt": at("2026-08-30T23:00:00+08:00"), "status": "completed",
             "createdAt": 1, "completedAt": 2, "syncUpdatedAt": 1}
        ]);
        let map = json!({"bySubject": {}, "anilistIndex": {"178789": 501963}});
        let now = at("2026-09-06T12:00:00+08:00");
        let media = HashMap::from([authority_media(
            178789,
            json!({"episode": 11, "airingAt": at("2026-09-06T23:00:00+08:00")}),
            json!([{"episode": 10, "airingAt": at("2026-08-30T23:00:00+08:00")}]),
        )]);

        assert!(apply_anilist_authority_media(&mut state, &map, &media, now));

        // 未播假票（旧键 pending ep11 >= next.episode 11）删除。
        let tasks = state["tasks"].as_array().unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0]["id"], "501963-10");
        assert_eq!(tasks[0]["status"], "completed");
        // next 由污染值 ep12@9/13 重写为权威值 ep11@9/6 23:00。
        assert_eq!(state["following"][0]["nextAiringEpisode"]["episode"], 11);
        assert_eq!(
            state["following"][0]["nextAiringEpisode"]["airingAt"],
            at("2026-09-06T23:00:00+08:00")
        );
    }

    #[cfg(feature = "standard")]
    #[test]
    fn anilist_authority_finished_media_clears_next_and_keeps_backlog() {
        // 完结 media（next=null）：条目 next 重写为 null（治愈已完结番残留
        // 下一期）；已播集 pending 的错误时间按 schedule 纠偏；已播待看积压
        // （schedule 内/外）与 completed 观看历史全部保留——完结番的 pending
        // 是合法观看积压，不属于"未播假票"（next.episode 未知时删除规则不
        // 生效）；越 eps 的残留由 reconcile_anilist_authority_tasks 另行清理。
        let mut state = default_state(false);
        state["following"] = json!([{
            "id": 35760, "source": "bangumi", "anilistId": 16498, "bangumiId": 35760,
            "displayTitle": "SHIROBAKO", "episodes": 24, "followedAt": 1, "syncUpdatedAt": 1,
            "nextAiringEpisode": {"episode": 25, "airingAt": at("2026-09-20T22:30:00+08:00")}
        }]);
        state["tasks"] = json!([
            {"id": "35760-24", "animeId": 35760, "episode": 24,
             "airingAt": at("2026-03-26T23:00:00+08:00"), "status": "pending",
             "createdAt": 1, "completedAt": Value::Null, "syncUpdatedAt": 1},
            {"id": "35760-23", "animeId": 35760, "episode": 23,
             "airingAt": at("2026-09-01T00:00:00+08:00"), "status": "pending",
             "createdAt": 1, "completedAt": Value::Null, "syncUpdatedAt": 1},
            {"id": "35760-22", "animeId": 35760, "episode": 22,
             "airingAt": at("2026-08-30T00:00:00+08:00"), "status": "completed",
             "createdAt": 1, "completedAt": 2, "syncUpdatedAt": 1}
        ]);
        let map = json!({"bySubject": {}, "anilistIndex": {"16498": 35760}});
        let now = at("2026-09-06T12:00:00+08:00");
        let media = HashMap::from([authority_media(
            16498,
            Value::Null,
            json!([
                {"episode": 24, "airingAt": at("2026-03-26T22:30:00+08:00")},
                {"episode": 23, "airingAt": at("2026-03-19T22:30:00+08:00")}
            ]),
        )]);

        assert!(apply_anilist_authority_media(&mut state, &map, &media, now));

        assert!(state["following"][0]["nextAiringEpisode"].is_null());
        let tasks = state["tasks"].as_array().unwrap();
        assert_eq!(tasks.len(), 3);
        // 已播集时间纠偏。
        assert_eq!(tasks[0]["id"], "35760-24");
        assert_eq!(tasks[0]["airingAt"], at("2026-03-26T22:30:00+08:00"));
        // 已播待看积压与 completed 全部保留。
        assert_eq!(tasks[1]["id"], "35760-23");
        assert_eq!(tasks[1]["status"], "pending");
        assert_eq!(tasks[2]["id"], "35760-22");
        assert_eq!(tasks[2]["status"], "completed");
    }

    #[cfg(feature = "standard")]
    #[test]
    fn anilist_authority_apply_is_idempotent() {
        // 幂等：同一权威 media 应用两次，第二次零变更（next 重写只比较
        // (episode, airingAt)，timeUntilAiring 随 now 漂移不参与）。
        let mut state = default_state(false);
        state["following"] = json!([{
            "id": 568572, "source": "bangumi", "anilistId": 195600, "bangumiId": 568572,
            "displayTitle": "黄泉的使者", "episodes": 13, "followedAt": 1, "syncUpdatedAt": 1,
            "nextAiringEpisode": {"episode": 24, "airingAt": at("2026-09-13T00:00:00+08:00")}
        }]);
        state["tasks"] = json!([
            {"id": "568572-23", "animeId": 568572, "episode": 23,
             "airingAt": at("2026-09-06T00:00:00+08:00"), "status": "pending",
             "createdAt": 1, "completedAt": Value::Null, "syncUpdatedAt": 1}
        ]);
        let map = json!({"bySubject": {}, "anilistIndex": {"195600": 568572}});
        let now = at("2026-09-06T12:00:00+08:00");
        let media = HashMap::from([authority_media(
            195600,
            json!({"episode": 23, "airingAt": at("2026-09-12T22:30:00+08:00")}),
            json!([{"episode": 22, "airingAt": at("2026-09-05T22:30:00+08:00")}]),
        )]);

        assert!(apply_anilist_authority_media(&mut state, &map, &media, now));
        let after = state.clone();
        // 第二轮 now 不同（timeUntilAiring 变化）仍必须零变更。
        assert!(!apply_anilist_authority_media(
            &mut state,
            &map,
            &media,
            now + 600
        ));
        assert_eq!(state, after);
    }

    #[cfg(feature = "standard")]
    #[test]
    fn anilist_authority_keeps_aired_task_under_next_and_rewrites_time() {
        // 第 6 轮误删事故回归（防误删半场）：100女友（598058, anilistId=200637）
        // 合法已播 ep10 任务（9/6 21:30 真实播出，离线锚点错写成 23:30）在
        // 权威 refresh（next=ep11@9/13 21:30 + schedule 含 ep10@9/6 21:30）
        // 下必须保留且时间被改写——episode < next.episode 的已播任务不删除、
        // 不重复回填。
        let mut state = default_state(false);
        state["following"] = json!([{
            "id": 598058, "source": "bangumi", "anilistId": 200637, "bangumiId": 598058,
            "displayTitle": "100 女友 S3", "episodes": 12, "followedAt": 1, "syncUpdatedAt": 1,
            "nextAiringEpisode": {"episode": 10, "airingAt": at("2026-09-09T21:30:00+08:00")}
        }]);
        state["tasks"] = json!([
            {"id": "598058-10", "animeId": 598058, "episode": 10,
             "airingAt": at("2026-09-06T23:30:00+08:00"), "status": "pending",
             "createdAt": 1, "completedAt": Value::Null, "syncUpdatedAt": 1}
        ]);
        let map = json!({"bySubject": {}, "anilistIndex": {"200637": 598058}});
        let now = at("2026-09-06T22:00:00+08:00");
        let media = HashMap::from([authority_media(
            200637,
            json!({"episode": 11, "airingAt": at("2026-09-13T21:30:00+08:00")}),
            json!([{"episode": 10, "airingAt": at("2026-09-06T21:30:00+08:00")}]),
        )]);

        assert!(apply_anilist_authority_media(&mut state, &map, &media, now));

        // ep10 合法已播任务保留（唯一一条，无回填重复），时间按权威 schedule 改写。
        let tasks = state["tasks"].as_array().unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0]["id"], "598058-10");
        assert_eq!(tasks[0]["status"], "pending");
        assert_eq!(tasks[0]["airingAt"], at("2026-09-06T21:30:00+08:00"));
        // 条目 next 由污染值 ep10@9/9 重写为权威值 ep11@9/13 21:30。
        assert_eq!(state["following"][0]["nextAiringEpisode"]["episode"], 11);

        // 幂等：再应用零变更。
        assert!(!apply_anilist_authority_media(
            &mut state,
            &map,
            &media,
            now + 600
        ));
    }

    #[cfg(feature = "standard")]
    #[test]
    fn anilist_authority_backfills_missing_aired_episodes_once() {
        // 权威回填主场景（100女友 事故复刻）：ep10 任务已被误删（tasks 空），
        // 权威 schedule 含 ep9@8/30、ep10@9/6 21:30、next=ep11 → 逐集回填
        // pending（字段与调度管道同形）；未播的 next 本集绝不回填；幂等。
        let mut state = default_state(false);
        state["following"] = json!([{
            "id": 598058, "source": "bangumi", "anilistId": 200637, "bangumiId": 598058,
            "displayTitle": "100 女友 S3", "episodes": 12, "followedAt": 1, "syncUpdatedAt": 1,
            "nextAiringEpisode": {"episode": 10, "airingAt": at("2026-09-09T21:30:00+08:00")}
        }]);
        state["tasks"] = json!([]);
        let map = json!({"bySubject": {}, "anilistIndex": {"200637": 598058}});
        let now = at("2026-09-06T22:00:00+08:00");
        let media = HashMap::from([authority_media(
            200637,
            json!({"episode": 11, "airingAt": at("2026-09-13T21:30:00+08:00")}),
            json!([
                {"episode": 9, "airingAt": at("2026-08-30T21:30:00+08:00")},
                {"episode": 10, "airingAt": at("2026-09-06T21:30:00+08:00")}
            ]),
        )]);

        assert!(apply_anilist_authority_media(&mut state, &map, &media, now));

        let tasks = state["tasks"].as_array().unwrap();
        assert_eq!(tasks.len(), 2);
        let ep10 = tasks
            .iter()
            .find(|task| value_string(task.get("id")) == "598058-10")
            .expect("backfilled episode 10");
        assert_eq!(ep10["animeId"], 598058);
        assert_eq!(ep10["subjectId"], 598058);
        assert_eq!(ep10["episode"], 10);
        assert_eq!(ep10["airingAt"], at("2026-09-06T21:30:00+08:00"));
        assert_eq!(ep10["status"], "pending");
        assert_eq!(ep10["createdAt"], now);
        assert!(ep10["completedAt"].is_null());
        assert!(value_i64(ep10.get("syncUpdatedAt")) > 0);
        assert_eq!(ep10["animeTitle"], "100 女友 S3");
        assert_eq!(ep10["episodeSortKey"], "10");
        assert_eq!(ep10["episodeType"], "regular");
        let ep9 = tasks
            .iter()
            .find(|task| value_string(task.get("id")) == "598058-9")
            .expect("backfilled episode 9");
        assert_eq!(ep9["airingAt"], at("2026-08-30T21:30:00+08:00"));
        // 未播集（next=ep11，含其补充时间）不回填。
        assert!(
            !tasks
                .iter()
                .any(|task| value_i64(task.get("episode")) >= 11)
        );

        // 幂等：再次应用零变更（已回填任务不重复、时间一致）。
        let after = state.clone();
        assert!(!apply_anilist_authority_media(
            &mut state,
            &map,
            &media,
            now + 600
        ));
        assert_eq!(state, after);
    }

    #[cfg(feature = "standard")]
    #[test]
    fn anilist_authority_backfill_skips_when_any_task_exists_including_legacy_key() {
        // a) 已有 completed ep10（subjectId 键）→ 观看历史在，不回填 pending；
        // b) 已有旧键任务（animeId=anilistId 200637，主键迁移前形态）→ 不重复
        //    回填（canonicalize_cross_key_tasks 负责归一，回填不动它）。
        let map = json!({"bySubject": {}, "anilistIndex": {"200637": 598058}});
        let now = at("2026-09-06T22:00:00+08:00");
        let media = HashMap::from([authority_media(
            200637,
            json!({"episode": 11, "airingAt": at("2026-09-13T21:30:00+08:00")}),
            json!([{"episode": 10, "airingAt": at("2026-09-06T21:30:00+08:00")}]),
        )]);

        // a) completed ep10 → 不回填（next 已是权威值 → 整轮零变更）。
        let mut state = default_state(false);
        state["following"] = json!([{
            "id": 598058, "source": "bangumi", "anilistId": 200637, "bangumiId": 598058,
            "displayTitle": "100 女友 S3", "episodes": 12, "followedAt": 1, "syncUpdatedAt": 1,
            "nextAiringEpisode": {"episode": 11, "airingAt": at("2026-09-13T21:30:00+08:00")}
        }]);
        state["tasks"] = json!([
            {"id": "598058-10", "animeId": 598058, "subjectId": 598058, "episode": 10,
             "airingAt": at("2026-09-06T21:30:00+08:00"), "status": "completed",
             "createdAt": 1, "completedAt": 2, "syncUpdatedAt": 1}
        ]);
        assert!(!apply_anilist_authority_media(
            &mut state, &map, &media, now
        ));
        let tasks = state["tasks"].as_array().unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0]["status"], "completed");

        // b) 旧键 pending ep10 → 不重复回填，原记录原样保留。
        let mut state = default_state(false);
        state["following"] = json!([{
            "id": 598058, "source": "bangumi", "anilistId": 200637, "bangumiId": 598058,
            "displayTitle": "100 女友 S3", "episodes": 12, "followedAt": 1, "syncUpdatedAt": 1,
            "nextAiringEpisode": {"episode": 11, "airingAt": at("2026-09-13T21:30:00+08:00")}
        }]);
        state["tasks"] = json!([
            {"id": "200637-10", "animeId": 200637, "episode": 10,
             "airingAt": at("2026-09-06T21:30:00+08:00"), "status": "pending",
             "createdAt": 1, "completedAt": Value::Null, "syncUpdatedAt": 1}
        ]);
        assert!(!apply_anilist_authority_media(
            &mut state, &map, &media, now
        ));
        let tasks = state["tasks"].as_array().unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0]["id"], "200637-10");
        assert_eq!(tasks[0]["animeId"], 200637);
    }

    #[cfg(feature = "standard")]
    #[test]
    fn anilist_authority_backfill_gated_by_tracking_status_and_create_tasks() {
        // 门控：wish/on_hold/done（收录不追踪）不回填；createWatchTasks=false
        // 不回填（回填即自动建任务，与调度管道同契约）。
        let map = json!({"bySubject": {}, "anilistIndex": {"200637": 598058}});
        let now = at("2026-09-06T22:00:00+08:00");
        let media = HashMap::from([authority_media(
            200637,
            json!({"episode": 11, "airingAt": at("2026-09-13T21:30:00+08:00")}),
            json!([{"episode": 10, "airingAt": at("2026-09-06T21:30:00+08:00")}]),
        )]);
        let entry = |status: &str| {
            json!([{
                "id": 598058, "source": "bangumi", "anilistId": 200637, "bangumiId": 598058,
                "displayTitle": "100 女友 S3", "episodes": 12, "followedAt": 1,
                "syncUpdatedAt": 1, "bangumiStatus": status,
                "nextAiringEpisode": {"episode": 11, "airingAt": at("2026-09-13T21:30:00+08:00")}
            }])
        };

        // a) 非追踪状态（wish/on_hold/done）→ 零回填。
        for status in ["wish", "on_hold", "done"] {
            let mut state = default_state(false);
            state["following"] = entry(status);
            state["tasks"] = json!([]);
            assert!(!apply_anilist_authority_media(
                &mut state, &map, &media, now
            ));
            assert!(state["tasks"].as_array().unwrap().is_empty());
        }

        // b) doing（追踪中）但 createWatchTasks=false → 零回填。
        let mut state = default_state(false);
        state["following"] = entry("doing");
        state["tasks"] = json!([]);
        state["settings"]["createWatchTasks"] = json!(false);
        assert!(!apply_anilist_authority_media(
            &mut state, &map, &media, now
        ));
        assert!(state["tasks"].as_array().unwrap().is_empty());
    }

    #[cfg(feature = "standard")]
    #[test]
    fn anilist_authority_backfill_skips_pre_follow_history_and_watched_episodes() {
        // 第 8 轮问题 3 回填门槛（回填洪流治理）：
        // a) followedAt=9/6 时 9/4 播的集不回填（追番前的历史不是待办），
        //    9/8 播的集照常回填；
        // b) watchedEpisode=3 时 ep1-3 不回填、追番后播且未看的 ep4 照常回填。
        let map = json!({"bySubject": {}, "anilistIndex": {"200637": 598058}});
        let now = at("2026-09-08T22:00:00+08:00");

        // a) airingAt < followedAt 不回填。
        let mut state = default_state(false);
        state["following"] = json!([{
            "id": 598058, "source": "bangumi", "anilistId": 200637, "bangumiId": 598058,
            "displayTitle": "100 女友 S3", "episodes": 12,
            "followedAt": at("2026-09-06T00:00:00+08:00"), "syncUpdatedAt": 1,
            "nextAiringEpisode": {"episode": 5, "airingAt": at("2026-09-13T21:30:00+08:00")}
        }]);
        state["tasks"] = json!([]);
        let media = HashMap::from([authority_media(
            200637,
            json!({"episode": 5, "airingAt": at("2026-09-13T21:30:00+08:00")}),
            json!([
                {"episode": 3, "airingAt": at("2026-09-04T21:30:00+08:00")},
                {"episode": 4, "airingAt": at("2026-09-08T21:30:00+08:00")}
            ]),
        )]);

        assert!(apply_anilist_authority_media(&mut state, &map, &media, now));

        let episodes: Vec<i64> = state["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|task| value_i64(task.get("episode")))
            .collect();
        assert_eq!(episodes, vec![4], "只回填追番后播出的 ep4");

        // b) watchedEpisode=3：ep1-3 已看不回填。
        let mut state = default_state(false);
        state["following"] = json!([{
            "id": 598058, "source": "bangumi", "anilistId": 200637, "bangumiId": 598058,
            "displayTitle": "100 女友 S3", "episodes": 12,
            "followedAt": at("2026-08-01T00:00:00+08:00"), "syncUpdatedAt": 1,
            "watchedEpisode": 3,
            "nextAiringEpisode": {"episode": 5, "airingAt": at("2026-09-13T21:30:00+08:00")}
        }]);
        state["tasks"] = json!([]);
        let media = HashMap::from([authority_media(
            200637,
            json!({"episode": 5, "airingAt": at("2026-09-13T21:30:00+08:00")}),
            json!([
                {"episode": 1, "airingAt": at("2026-08-02T21:30:00+08:00")},
                {"episode": 2, "airingAt": at("2026-08-09T21:30:00+08:00")},
                {"episode": 3, "airingAt": at("2026-08-16T21:30:00+08:00")},
                {"episode": 4, "airingAt": at("2026-08-23T21:30:00+08:00")}
            ]),
        )]);

        assert!(apply_anilist_authority_media(&mut state, &map, &media, now));

        let episodes: Vec<i64> = state["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|task| value_i64(task.get("episode")))
            .collect();
        assert_eq!(
            episodes,
            vec![4],
            "ep1-3 已看不回填，追番后播的 ep4 照常回填"
        );
    }

    #[cfg(feature = "standard")]
    #[test]
    fn reconcile_purges_pre_follow_flood_pending_and_is_idempotent() {
        // 第 8 轮问题 3 存量洪水清理：anilistId 条目（含旧键 animeId=anilistId）
        // 的 pending 且 airingAt < followedAt → 删除（黄泉 1..16 洪水形态）；
        // completed 历史保留；纯 Bangumi 条目（无 anilistId）与 followedAt=0
        // 的条目不判；幂等。
        let followed_at = at("2026-09-06T00:00:00+08:00");
        let mut state = default_state(false);
        state["following"] = json!([
            {"id": 568572, "source": "bangumi", "anilistId": 195600, "bangumiId": 568572,
             "displayTitle": "黄泉的使者",
             "followedAt": followed_at, "syncUpdatedAt": 1},
            {"id": 140001, "source": "bangumi", "anilistId": null,
             "displayTitle": "纯Bangumi条目",
             "followedAt": followed_at, "syncUpdatedAt": 1},
            {"id": 200001, "source": "bangumi", "anilistId": 999001,
             "displayTitle": "未知追番时间", "followedAt": 0, "syncUpdatedAt": 1}
        ]);
        state["tasks"] = json!([
            // 洪水：追番前已播的历史 pending → 删除。
            {"id": "568572-1", "animeId": 568572, "episode": 1,
             "airingAt": followed_at - 86_400, "status": "pending",
             "createdAt": 1, "completedAt": Value::Null, "syncUpdatedAt": 1},
            // 旧键（animeId=anilistId 195600）洪水 → 同样删除。
            {"id": "195600-16", "animeId": 195600, "episode": 16,
             "airingAt": followed_at - 3_600, "status": "pending",
             "createdAt": 1, "completedAt": Value::Null, "syncUpdatedAt": 1},
            // 追番后已播 pending → 保留。
            {"id": "568572-3", "animeId": 568572, "episode": 3,
             "airingAt": followed_at + 3_600, "status": "pending",
             "createdAt": 1, "completedAt": Value::Null, "syncUpdatedAt": 1},
            // 追番前已播的 completed → 观看历史，保留。
            {"id": "568572-2", "animeId": 568572, "episode": 2,
             "airingAt": followed_at - 3_600, "status": "completed",
             "createdAt": 1, "completedAt": 2, "syncUpdatedAt": 1},
            // 纯 Bangumi 条目（无 anilistId）：airingAt 非权威来源，不判。
            {"id": "140001-1", "animeId": 140001, "episode": 1,
             "airingAt": followed_at - 3_600, "status": "pending",
             "createdAt": 1, "completedAt": Value::Null, "syncUpdatedAt": 1},
            // followedAt=0（未知追番时间）：不判。
            {"id": "200001-1", "animeId": 200001, "episode": 1, "airingAt": 50,
             "status": "pending", "createdAt": 1, "completedAt": Value::Null,
             "syncUpdatedAt": 1}
        ]);
        let map = json!({"bySubject": {}, "anilistIndex": {}});

        assert!(reconcile_following_entries(&mut state, &map, false));
        let ids: Vec<String> = state["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|task| value_string(task.get("id")))
            .collect();
        assert_eq!(ids, vec!["568572-3", "568572-2", "140001-1", "200001-1"]);

        // 幂等：再次 reconcile 任务集不再变化。
        let before = state["tasks"].clone();
        assert!(!reconcile_following_entries(&mut state, &map, false));
        assert_eq!(state["tasks"], before);
    }

    #[cfg(feature = "standard")]
    #[test]
    fn anilist_authority_refresh_silently_fails_on_network_errors() {
        use crate::bangumi::test_support::MockBangumiServer;

        let map = json!({"bySubject": {}, "anilistIndex": {"195600": 568572}});
        let state = Mutex::new(json!({
            "following": [{"id": 568572, "source": "bangumi", "anilistId": 195600,
                           "followedAt": 1, "syncUpdatedAt": 1}],
            "tasks": []
        }));
        let client = reqwest::Client::new();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio test runtime");
        let cache_dir = authority_cache_dir("refresh-network-errors");

        // a) 连接不可达（127.0.0.1:9 discard 端口）→ 静默 false，状态不动。
        let refreshed = runtime.block_on(anilist_authority_refresh(
            &state,
            &map,
            "http://127.0.0.1:9/",
            &client,
            &[195600],
            &cache_dir,
            at("2026-09-06T12:00:00+08:00"),
        ));
        assert!(!refreshed);
        assert!(state.lock().unwrap()["following"][0]["nextAiringEpisode"].is_null());

        // b) GraphQL errors 响应 → 静默 false。
        let erroring =
            MockBangumiServer::spawn(std::sync::Arc::new(|_method, _target, _headers, _body| {
                (
                    200,
                    vec![],
                    json!({"errors": [{"message": "boom"}], "data": Value::Null}).to_string(),
                )
            }));
        let refreshed = runtime.block_on(anilist_authority_refresh(
            &state,
            &map,
            &erroring.url(),
            &client,
            &[195600],
            &cache_dir,
            at("2026-09-06T12:00:00+08:00"),
        ));
        assert!(!refreshed);
        assert!(state.lock().unwrap()["following"][0]["nextAiringEpisode"].is_null());
    }

    /// 第 8 轮测试辅助：每用例独立的权威缓存目录（防缓存串扰），返回前清空
    /// 残留并建目录。
    #[cfg(all(feature = "standard", not(target_os = "android")))]
    fn authority_cache_dir(tag: &str) -> std::path::PathBuf {
        let directory = std::env::temp_dir().join(format!(
            "anilog-test-authority-cache-{}-{tag}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&directory);
        let _ = fs::create_dir_all(&directory);
        directory
    }

    #[cfg(all(feature = "standard", not(target_os = "android")))]
    #[test]
    fn anilist_authority_cache_roundtrip_ttl_and_webdav_resurrection_heal() {
        // 第 8 轮问题 1（桌面止血）+ 问题 2 测试：
        // 1) 抓取缓存落盘后可免网络重放（apply_cached_anilist_authority 无
        //    HTTP 客户端参数，结构性不触网）；TTL 内应用成功，过期返回 false
        //    不阻塞；缓存目录为空/损坏 JSON 同样 false。
        // 2) webdav 复活愈合序列：本地已删 ep23 假票 → 云端文档 merge 加回 →
        //    apply cached（fresh）→ 再删 → 上传文档无 ep23、无 nextAiringEpisode。
        let map = json!({"bySubject": {}, "anilistIndex": {"195600": 568572}});
        let now = at("2026-09-06T12:00:00+08:00");
        let media = authority_media(
            195600,
            json!({"episode": 23, "airingAt": at("2026-09-12T22:30:00+08:00")}),
            json!([
                {"episode": 22, "airingAt": at("2026-09-05T22:30:00+08:00")},
                {"episode": 23, "airingAt": at("2026-09-12T22:30:00+08:00")}
            ]),
        )
        .1;
        // 本地形态：黄泉 next 仍为污染值 ep24@9/13；ep23 假票已被本地删除
        // （只剩 ep22 completed 观看历史）。
        let local_state = || {
            let mut state = default_state(false);
            state["following"] = json!([{
                "id": 568572, "source": "bangumi", "anilistId": 195600, "bangumiId": 568572,
                "title": {"native": "黄泉の使者", "english": null, "romaji": null},
                "displayTitle": "黄泉的使者",
                "episodes": 13, "followedAt": at("2026-08-20T00:00:00+08:00"), "syncUpdatedAt": 1,
                "nextAiringEpisode": {"episode": 24, "airingAt": at("2026-09-13T00:00:00+08:00")}
            }]);
            state["tasks"] = json!([
                {"id": "568572-22", "animeId": 568572, "episode": 22,
                 "airingAt": at("2026-09-05T22:30:00+08:00"), "status": "completed",
                 "createdAt": 1, "completedAt": 2, "syncUpdatedAt": 1}
            ]);
            state
        };
        // 云端文档：携带复活假票 ep23@9/6（任务无墓碑，LWW 会加回）与污染 next。
        let remote = json!({
            "version": SYNC_VERSION,
            "following": [{
                "id": 568572, "source": "bangumi", "anilistId": 195600, "bangumiId": 568572,
                "title": {"native": "黄泉の使者", "english": null, "romaji": null},
                "displayTitle": "黄泉的使者",
                "episodes": 13, "followedAt": at("2026-08-20T00:00:00+08:00"),
                "syncUpdatedAt": 9_000,
                "nextAiringEpisode": {"episode": 24, "airingAt": at("2026-09-13T00:00:00+08:00")}
            }],
            "tasks": [{
                "id": "568572-23", "animeId": 568572, "animeTitle": "黄泉的使者",
                "episode": 23, "airingAt": at("2026-09-06T00:00:00+08:00"),
                "status": "pending", "createdAt": 1, "completedAt": null,
                "syncUpdatedAt": 9_000
            }],
            "followingDeletedAt": {}
        });
        let cache_dir = authority_cache_dir("roundtrip-heal");

        // 1a) 缓存目录为空 → false 不阻塞。
        let mut state = local_state();
        assert!(!apply_cached_anilist_authority(
            &mut state, &map, &cache_dir, now
        ));
        assert_eq!(state["tasks"].as_array().unwrap().len(), 1);

        // 1b) 抓取缓存写入 → TTL 内（真实时钟 fetchedAt=now 毫秒）免网络应用：
        //     next 重写为权威值 ep23@9/12、复活的 ep23 假票删除；再次应用零变更。
        write_anilist_authority_cache(&cache_dir, &HashMap::from([(195600, media.clone())]));
        let mut state = local_state();
        assert!(apply_cached_anilist_authority(
            &mut state,
            &map,
            &cache_dir,
            now_seconds()
        ));
        assert_eq!(state["following"][0]["nextAiringEpisode"]["episode"], 23);
        assert!(
            !state["tasks"]
                .as_array()
                .unwrap()
                .iter()
                .any(|task| value_string(task.get("id")) == "568572-23")
        );
        assert!(!apply_cached_anilist_authority(
            &mut state,
            &map,
            &cache_dir,
            now_seconds()
        ));

        // 1c) 过期缓存（fetchedAt = now - 1 小时）→ false 不阻塞、状态不动。
        let stale = json!({
            "fetchedAt": (now - 3_600) * 1_000,
            "media": [media.clone()]
        });
        fs::write(
            cache_dir.join(ANILIST_AUTHORITY_CACHE_FILE),
            stale.to_string(),
        )
        .unwrap();
        let mut state = local_state();
        assert!(!apply_cached_anilist_authority(
            &mut state, &map, &cache_dir, now
        ));
        assert_eq!(state["following"][0]["nextAiringEpisode"]["episode"], 24);

        // 1d) 损坏 JSON → false 不阻塞。
        fs::write(cache_dir.join(ANILIST_AUTHORITY_CACHE_FILE), "not-json").unwrap();
        let mut state = local_state();
        assert!(!apply_cached_anilist_authority(
            &mut state, &map, &cache_dir, now
        ));

        // 2) webdav 复活愈合序列（perform_webdav_sync 合并段复刻）。缓存改用
        //    测试 now 手写 fresh 时间戳，断言与真实时钟解耦。
        let fresh = json!({"fetchedAt": now * 1_000, "media": [media]});
        fs::write(
            cache_dir.join(ANILIST_AUTHORITY_CACHE_FILE),
            fresh.to_string(),
        )
        .unwrap();
        let mut state = local_state();
        let (changed, _, _) = merge_document_into_state(&mut state, &remote).unwrap();
        assert!(changed, "云端假票经 LWW 加回本地");
        assert!(
            state["tasks"]
                .as_array()
                .unwrap()
                .iter()
                .any(|task| value_string(task.get("id")) == "568572-23")
        );
        // 合并后套用缓存权威数据 → 假票当次再死一次、污染 next 治愈。
        assert!(apply_cached_anilist_authority(
            &mut state, &map, &cache_dir, now
        ));
        let tasks = state["tasks"].as_array().unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0]["id"], "568572-22");
        assert_eq!(tasks[0]["status"], "completed");
        assert_eq!(state["following"][0]["nextAiringEpisode"]["episode"], 23);
        // 上传文档自愈：无 ep23 假票、无 nextAiringEpisode。
        let document = document_from_state(&mut state);
        assert!(
            !document["tasks"]
                .as_array()
                .unwrap()
                .iter()
                .any(|task| value_string(task.get("id")) == "568572-23")
        );
        assert!(document["following"][0].get("nextAiringEpisode").is_none());
    }

    #[cfg(feature = "standard")]
    #[test]
    fn anilist_authority_fetch_paginates_graphql_pages() {
        use crate::bangumi::test_support::MockBangumiServer;

        // 两页 media（lastPage=2）：验证分页抓取与请求形制（query 名、
        // variables.ids 原样透传、页码递增）。
        let requested: Mutex<Vec<Value>> = Mutex::new(Vec::new());
        let capture = std::sync::Arc::new(requested);
        let writer = capture.clone();
        let server = MockBangumiServer::spawn(std::sync::Arc::new(
            move |_method, _target, _headers, body| {
                let payload: Value = serde_json::from_str(body).unwrap_or(Value::Null);
                let page = value_i64(payload["variables"].get("page"));
                writer.lock().unwrap().push(payload);
                let media = if page == 1 {
                    json!([authority_media(
                        195600,
                        json!({"episode": 23, "airingAt": at("2026-09-12T22:30:00+08:00")}),
                        json!([])
                    )
                    .1])
                } else {
                    json!([authority_media(
                        163134,
                        json!({"episode": 11, "airingAt": at("2026-09-11T22:30:00+08:00")}),
                        json!([])
                    )
                    .1])
                };
                (
                    200,
                    vec![],
                    json!({"data": {"Page": {
                        "pageInfo": {"lastPage": 2}, "media": media
                    }}})
                    .to_string(),
                )
            },
        ));

        let client = reqwest::Client::new();
        let media = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio test runtime")
            .block_on(fetch_anilist_authority_media(
                &client,
                &server.url(),
                &[195600, 163134],
            ))
            .expect("authority media");

        assert_eq!(media.len(), 2);
        assert!(media.contains_key(&195600));
        assert!(media.contains_key(&163134));
        let requests = capture.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(requests.iter().all(|request| {
            request["query"]
                .as_str()
                .unwrap_or_default()
                .contains("AniListAuthority")
        }));
        assert_eq!(requests[0]["variables"]["ids"], json!([195600, 163134]));
        assert_eq!(requests[0]["variables"]["page"], 1);
        assert_eq!(requests[1]["variables"]["page"], 2);
    }

    #[cfg(feature = "standard")]
    #[test]
    fn desktop_reconcile_keeps_aired_task_under_polluted_next_android_fn_still_drops() {
        // 第 6 轮误删事故回归（100女友 598058, anilistId=200637）：桌面条目的
        // nextAiringEpisode 曾被离线锚点污染（next=ep10@9/9），而 ep10 已于
        // 9/6 21:30 真实播出——旧版 reconcile_following_entries 按
        // episode >= next 误删了合法已播任务，且其 id 已在 seenAiringEvents、
        // AIRING_QUERY 窗口永不重建。修复后：
        // a) 桌面 reconcile（覆盖加载/WebDAV 合并路径）不再做此无网络清理，
        //    已播任务保留且零变更；
        // b) Android 语义（纯函数层，mobile merge_status 同口径）：next=ep10
        //    时 pending ep10 仍按契约删除（Java 提供的 next 与条目同源，无
        //    离线锚点污染路径），completed 不动，幂等。
        let mut state = default_state(false);
        state["following"] = json!([{
            "id": 598058, "source": "bangumi", "anilistId": 200637, "bangumiId": 598058,
            "displayTitle": "100 女友 S3", "episodes": 12, "followedAt": 1, "syncUpdatedAt": 1,
            "nextAiringEpisode": {"episode": 10, "airingAt": at("2026-09-09T21:30:00+08:00")}
        }]);
        let aired = now_seconds() - 3_600; // 真实已播（相对 now，purge 不误伤）。
        // 任务已是 canonicalize 后的规范形态（subjectId/episodeSortKey/
        // episodeType 齐备）→ a) 步的零变更断言只反映"误删清理已撤销"。
        state["tasks"] = json!([
            {"id": "598058-10", "animeId": 598058, "subjectId": 598058,
             "episode": 10, "episodeSortKey": "10", "episodeType": "regular",
             "airingAt": aired, "status": "pending", "createdAt": 1,
             "completedAt": Value::Null, "syncUpdatedAt": 1},
            {"id": "598058-9", "animeId": 598058, "subjectId": 598058,
             "episode": 9, "episodeSortKey": "9", "episodeType": "regular",
             "airingAt": aired - 604_800, "status": "completed", "createdAt": 1,
             "completedAt": 2, "syncUpdatedAt": 1}
        ]);
        let map = json!({"bySubject": {}, "anilistIndex": {"200637": 598058}});

        // a) 桌面 reconcile：污染 next 不再触发删除，任务集原样保留。
        assert!(!reconcile_following_entries(&mut state, &map, false));
        let ids: Vec<String> = state["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|task| value_string(task.get("id")))
            .collect();
        assert_eq!(ids, vec!["598058-10", "598058-9"]);

        // b) Android 语义（纯函数层）：pending >= next 删除，completed 保留。
        assert!(reconcile_unaired_anilist_next_tasks(&mut state));
        let ids: Vec<String> = state["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|task| value_string(task.get("id")))
            .collect();
        assert_eq!(ids, vec!["598058-9"]);
        // 幂等。
        assert!(!reconcile_unaired_anilist_next_tasks(&mut state));
    }

    // -- 权威数据修复 2：跨键身份按 subject 锚定（夺还篇重追）------------------

    #[cfg(feature = "standard")]
    #[test]
    fn refollow_dakkan_card_stays_independent_from_shared_anilist_entry() {
        // 夺还篇（bangumi 633836）与丧失篇（547888）共用 anilistId 189046：
        // 重追夺还篇卡片绝不合并进 547888（anilistId 撞车不算同一作品），
        // 走独立新增路径 + 清 633836 墓碑/取消队列。
        let map = json!({
            "bySubject": {
                "547888": {"b": 547888, "a": 189046, "c": "丧失篇", "t": "Soushou Hen"},
                "633836": {"b": 633836, "a": 189046, "c": "夺还篇", "t": "Dakkan Hen"}
            },
            "anilistIndex": {"189046": 547888}
        });
        let mut state = default_state(false);
        state["following"] = json!([bangumi_following(
            547888,
            json!({"anilistId": 189046, "episodes": 11})
        )]);
        // 夺还篇曾以 633836 键追过并取消：墓碑 + 取消队列残留。
        state["following"]
            .as_array_mut()
            .unwrap()
            .push(bangumi_following(
                633836,
                json!({"anilistId": 189046, "episodes": 12}),
            ));
        assert!(remove_following(&mut state, 633836));
        assert!(following_tombstone_exists(&state, 633836));
        assert!(queue_contains_subject(&state, 633836));

        let card = json!({
            "id": 633836, "source": "bangumi", "anilistId": 189046, "bangumiSubjectId": 633836,
            "nameCn": "夺还篇",
            "title": {"native": "奪還篇", "romaji": "Dakkan Hen", "english": null},
            "coverImage": {"medium": "https://lain.bgm.tv/pic/cover/m/633836.jpg"},
            "format": "TV", "episodes": 12, "seasonYear": 2026
        });
        add_following_entry_standard(&mut state, &card, &map);

        let following = state["following"].as_array().unwrap();
        assert_eq!(following.len(), 2, "独立新增，不合并进 547888");
        let dakkan = following
            .iter()
            .find(|item| value_i64(item.get("id")) == 633836)
            .expect("新增独立 633836 条目");
        assert_eq!(dakkan["source"], "bangumi");
        assert_eq!(dakkan["anilistId"], 189046);
        assert_eq!(dakkan["lastChangedBy"], "local");
        assert_eq!(dakkan["displayTitle"], "夺还篇");
        // 547888 未被吞并（displayTitle 保持自身）。
        let soushou = following
            .iter()
            .find(|item| value_i64(item.get("id")) == 547888)
            .expect("547888 保留");
        assert_eq!(soushou["displayTitle"], "示例 547888");
        // 墓碑与取消队列清理。
        assert!(!following_tombstone_exists(&state, 633836));
        assert!(!queue_contains_subject(&state, 633836));
    }

    #[cfg(feature = "standard")]
    #[test]
    fn bangumi_conflict_policy_three_ways() {
        use crate::bangumi::test_support::MockBangumiServer;

        // 远端与本地内容完全一致（H_local==H_remote），但本地与远端各自的
        // 基线 hash 都已过期 → 方向不明，按 conflictPolicy 分派。
        let remote = json!({
            "subject_id": 45678, "subject_type": 2, "rate": 8, "type": 3,
            "tags": [], "ep_status": 3, "private": false
        });
        let collection: bangumi::BangumiCollection =
            serde_json::from_value(remote.clone()).unwrap();
        let h_remote = bangumi::collection_payload_hash(&collection);
        let stale_pulled =
            bangumi::collection_payload_hash_parts(3, Some(7), Some(1), None, &[], None);
        let stale_pushed =
            bangumi::collection_payload_hash_parts(3, Some(9), Some(1), None, &[], None);

        let run = |policy: &str| -> (
            bangumi::BangumiSyncReport,
            MockBangumiServer,
            std::sync::Mutex<Value>,
        ) {
            let profile = include_str!("../fixtures/bangumi/user-profile.json").to_string();
            let collections = collection_page(json!([remote.clone()]));
            let server = MockBangumiServer::spawn(Arc::new(
                move |_method, target, _headers, _request_body| {
                    if target == "/v0/me" {
                        return (200, vec![], profile.clone());
                    }
                    if target.starts_with("/v0/users/anilog_dev/collections?") {
                        return (200, vec![], collections.clone());
                    }
                    if target.starts_with("/v0/users/-/collections/") {
                        return (204, vec![], String::new());
                    }
                    (404, vec![], "{}".into())
                },
            ));
            let mut state = phase3_state("https://unused.example.com/v0");
            state["bangumi"]["conflictPolicy"] = json!(policy);
            state["following"] = json!([bangumi_following(
                45678,
                json!({
                    "rating": 8, "watchedEpisode": 3,
                    "lastPulledPayloadHash": stale_pulled,
                    "lastPushedPayloadHash": stale_pushed
                })
            )]);
            let state = std::sync::Mutex::new(state);
            let tokens = bangumi::MemoryTokenStore::new();
            tokens.store("conflict-token").unwrap();
            let username_cache = std::sync::Mutex::new(None);
            let client =
                bangumi::HttpBangumiClient::with_base(bangumi_test_base(&server.url())).unwrap();
            let offline = json!({"bySubject": {}, "anilistIndex": {}});
            let report = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("tokio test runtime")
                .block_on(bangumi_sync::run_bangumi_collection_sync(
                    &client,
                    &tokens,
                    &username_cache,
                    &state,
                    &offline,
                ));
            (report, server, state)
        };

        // latest：不动本地 + 记冲突，零写请求。
        let (report, server, state) = run("latest");
        assert_eq!(report.conflicts, 1);
        assert_eq!(report.pushed, 0);
        assert_eq!(write_count(&server.requests()), 0);
        {
            let guard = state.lock().unwrap();
            let entry = &guard["following"][0];
            assert_eq!(entry["lastPulledPayloadHash"], json!(stale_pulled));
            assert_eq!(entry["lastPushedPayloadHash"], json!(stale_pushed));
        }

        // local-first：推远端（本地 payload PATCH）+ 记录推送基线。
        let (report, server, state) = run("local-first");
        assert_eq!(report.conflicts, 0);
        assert_eq!(report.pushed, 1);
        let patch = server
            .requests()
            .into_iter()
            .find(|request| {
                request.method == "PATCH" && request.target == "/v0/users/-/collections/45678"
            })
            .expect("local-first pushes local payload");
        let payload: Value = serde_json::from_str(&patch.body).unwrap();
        assert_eq!(payload["type"], 3);
        assert_eq!(payload["rate"], 8);
        {
            let guard = state.lock().unwrap();
            let entry = &guard["following"][0];
            let expected_local =
                bangumi::collection_payload_hash_parts(3, Some(8), Some(3), None, &[], None);
            assert_eq!(entry["lastPushedPayloadHash"], json!(expected_local));
        }

        // bangumi-first：改本地（合并远端），零写请求。
        let (report, server, state) = run("bangumi-first");
        assert_eq!(report.conflicts, 0);
        assert_eq!(report.pushed, 0);
        assert_eq!(write_count(&server.requests()), 0);
        {
            let guard = state.lock().unwrap();
            let entry = &guard["following"][0];
            assert_eq!(entry["lastPulledPayloadHash"], json!(h_remote));
            assert_eq!(entry["lastChangedBy"], "bangumi");
        }
    }

    // 回归锁定（schema §9 + Phase 3）：bangumiSyncStatus 五字段与
    // pendingBangumiUnfollows 只进本地状态，绝不进坚果云文档。
    #[cfg(feature = "standard")]
    #[test]
    fn document_from_state_phase3_local_only_keys_never_sync() {
        let mut state = default_state(false);
        state["bangumiSyncStatus"] = json!({
            "lastFullSyncAt": 1, "lastWebDavSyncAt": 2, "lastBangumiSyncAt": 3,
            "lastScheduleSyncAt": 4, "lastSyncError": "boom"
        });
        state["pendingBangumiUnfollows"] = json!([{"subjectId": 9, "at": 5}]);
        state["bangumi"]["syncEnabled"] = json!(true);
        state["following"] = json!([{
            "id": 1, "title": {"native": "n", "romaji": null, "english": null},
            "displayTitle": "x", "followedAt": 1, "syncUpdatedAt": 1,
            "bangumiStatus": "doing", "rating": 8, "watchedEpisode": 3,
            "lastPulledPayloadHash": "aa", "lastPushedPayloadHash": "bb",
            "lastChangedBy": "bangumi"
        }]);

        let document = document_from_state(&mut state);

        let mut keys: Vec<&str> = document
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "following",
                "followingDeletedAt",
                "tasks",
                "updatedAt",
                "version"
            ]
        );
        // 记录体允许携带（属于 following 数组记录，随业务字段同步）。
        assert_eq!(document["following"][0]["bangumiStatus"], "doing");

        // merge_defaults 为旧状态补齐 bangumiSyncStatus 五字段与 following 镜像键。
        let merged = merge_defaults(legacy_v2_state(), false);
        let status = merged["bangumiSyncStatus"]
            .as_object()
            .expect("status block");
        let mut status_keys: Vec<&str> = status.keys().map(String::as_str).collect();
        status_keys.sort_unstable();
        assert_eq!(
            status_keys,
            [
                "lastBangumiSyncAt",
                "lastFullSyncAt",
                "lastScheduleSyncAt",
                "lastSyncError",
                "lastWebDavSyncAt"
            ]
        );
        assert!(merged["following"][0].get("bangumiStatus").is_some());
        assert!(merged["following"][0].get("rating").is_some());
        assert!(merged["following"][0].get("watchedEpisode").is_some());

        // original 不补任何 Phase 3 键。
        let original = merge_defaults(legacy_v2_state(), true);
        assert!(original.get("bangumiSyncStatus").is_none());
        assert!(original.get("pendingBangumiUnfollows").is_none());
        assert!(original["following"][0].get("bangumiStatus").is_none());
        assert!(original["following"][0].get("rating").is_none());
        assert!(original["following"][0].get("watchedEpisode").is_none());
    }

    #[cfg(feature = "standard")]
    #[test]
    fn webdav_merge_keeps_local_next_and_upload_doc_strips_next() {
        // 第 8 轮问题 1（治本）：nextAiringEpisode 转设备本地数据。
        // a) 云端文档携带旧污染 next（黄泉 ep24@9/13）且 LWW 由远端记录胜出 →
        //    合并后本地 next 保持本地值（合并前剥远端键 + 合并后恢复本地快照）；
        // b) 上传文档（document_from_state 输出）不含 nextAiringEpisode；
        // c) 远端复活记录（本地无该条目）不带 next 进入本地状态。
        let local_next = json!({"episode": 23, "airingAt": at("2026-09-12T22:30:00+08:00")});
        let remote = json!({
            "version": SYNC_VERSION,
            "following": [{
                "id": 568572, "source": "bangumi", "anilistId": 195600, "bangumiId": 568572,
                "title": {"native": "黄泉の使者", "english": null, "romaji": null},
                "displayTitle": "黄泉的使者",
                "followedAt": 1_000, "syncUpdatedAt": 9_000,
                "nextAiringEpisode": {"episode": 24, "airingAt": at("2026-09-13T00:00:00+08:00")}
            }],
            "tasks": [],
            "followingDeletedAt": {}
        });

        // a) 本地已有条目（syncUpdatedAt 较旧 → LWW 远端胜出）。
        let mut state = default_state(false);
        state["following"] = json!([{
            "id": 568572, "source": "bangumi", "anilistId": 195600, "bangumiId": 568572,
            "title": {"native": "黄泉の使者", "english": null, "romaji": null},
            "displayTitle": "黄泉的使者",
            "followedAt": 1_000, "syncUpdatedAt": 1_000,
            "nextAiringEpisode": local_next
        }]);
        let (_, merged, _) = merge_document_into_state(&mut state, &remote).unwrap();
        // 本地 next 保持本地值，不被远端污染值覆盖。
        assert_eq!(state["following"][0]["nextAiringEpisode"], local_next);
        // 上传文档不含 nextAiringEpisode。
        assert!(merged["following"][0].get("nextAiringEpisode").is_none());

        // c) 本地无该条目 → 云端记录复活进本地，但不携带 next（由本端调度/
        //    权威路径重建）。
        let mut state = default_state(false);
        let (_, merged, _) = merge_document_into_state(&mut state, &remote).unwrap();
        assert_eq!(state["following"].as_array().unwrap().len(), 1);
        assert!(state["following"][0].get("nextAiringEpisode").is_none());
        assert!(merged["following"][0].get("nextAiringEpisode").is_none());
    }

    #[cfg(feature = "standard")]
    #[test]
    fn remove_following_queues_bangumi_unfollows_only() {
        let mut state = default_state(false);
        state["following"] = json!([bangumi_following(100, json!({})), followed(200, 1_000)]);

        assert!(remove_following(&mut state, 100));
        assert!(remove_following(&mut state, 200));

        let queue = state["pendingBangumiUnfollows"].as_array().unwrap();
        assert_eq!(queue.len(), 1);
        assert_eq!(queue[0]["subjectId"], 100);
        assert!(value_i64(queue[0].get("at")) > 0);
        // 幂等：已不存在的条目不再入队。
        assert!(!remove_following(&mut state, 100));
        assert_eq!(
            state["pendingBangumiUnfollows"].as_array().unwrap().len(),
            1
        );
    }

    // ------------------------------------------------------------------
    // Phase 4：Android 前台过期同步补偿（foreground_sync 纯逻辑 +
    // run_full_bangumi_sync skipped 语义回归锁定）。Windows 测试直接覆盖
    // standard 门控的纯函数/核心编排，证明桌面路径未被 cfg 侵入。
    // ------------------------------------------------------------------

    /// 任务 4.1：过期判定三态（缺失/过期/新鲜，注入 now；边界恰好 900 秒
    /// 为"未超"，严格大于才算过期）。
    #[cfg(feature = "standard")]
    #[test]
    fn foreground_sync_staleness_three_states_with_injected_now() {
        use foreground_sync::{STALE_AFTER_SECS, SyncStaleness, staleness};
        let now = 1_760_000_000;
        // 缺失：无 lastFullSyncAt 视为过期（需补偿）。
        assert_eq!(staleness(None, now), SyncStaleness::Missing);
        // 过期：距 now 严格大于 900 秒。
        assert_eq!(
            staleness(Some(now - STALE_AFTER_SECS - 1), now),
            SyncStaleness::Stale
        );
        assert_eq!(staleness(Some(now - 30 * 60), now), SyncStaleness::Stale);
        // 新鲜：15 分钟内，含恰好等于阈值的边界。
        assert_eq!(
            staleness(Some(now - STALE_AFTER_SECS), now),
            SyncStaleness::Fresh
        );
        assert_eq!(staleness(Some(now - 60), now), SyncStaleness::Fresh);
        assert_eq!(staleness(Some(now), now), SyncStaleness::Fresh);
        // 未来时间戳（时钟偏移）不 panic、按新鲜处理。
        assert_eq!(staleness(Some(now + 120), now), SyncStaleness::Fresh);
    }

    /// 任务 4.1 补充：错误重试判定（lastSyncError 非空 + 距上次尝试超 30 分钟）。
    #[cfg(feature = "standard")]
    #[test]
    fn foreground_sync_error_retry_throttled_to_thirty_minutes() {
        use foreground_sync::{ERROR_RETRY_AFTER_SECS, error_retry_due};
        let now = 1_760_000_000;
        // 无错误（None / 空串 / 空白）不重试。
        assert_eq!(error_retry_due(None, Some(now - 4 * 3600), now), false);
        assert_eq!(error_retry_due(Some(""), Some(now - 4 * 3600), now), false);
        assert_eq!(
            error_retry_due(Some("  "), Some(now - 4 * 3600), now),
            false
        );
        // 有错误但从未同步过（无上次尝试时间）不重试。
        assert_eq!(error_retry_due(Some("网络错误"), None, now), false);
        // 有错误且距上次尝试超过 30 分钟 → 重试。
        assert_eq!(
            error_retry_due(
                Some("网络错误"),
                Some(now - ERROR_RETRY_AFTER_SECS - 1),
                now
            ),
            true
        );
        // 边界：恰好 30 分钟为"未超"，不重试。
        assert_eq!(
            error_retry_due(Some("网络错误"), Some(now - ERROR_RETRY_AFTER_SECS), now),
            false
        );
        // 刚失败不久不重试。
        assert_eq!(
            error_retry_due(Some("网络错误"), Some(now - 60), now),
            false
        );
    }

    /// 任务 4.2：single-flight 并发两次只跑一次（8 线程竞争只有 1 个赢家；
    /// finish 后可再次占用，串行重入被拒绝）。
    #[cfg(feature = "standard")]
    #[test]
    fn single_flight_gate_only_one_winner_under_concurrency() {
        use std::sync::Barrier;
        let gate = Arc::new(foreground_sync::SingleFlightGate::new());
        let barrier = Arc::new(Barrier::new(8));
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let gate = Arc::clone(&gate);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    gate.try_begin()
                })
            })
            .collect();
        let winners = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .filter(|won| *won)
            .count();
        assert_eq!(winners, 1);
        assert!(gate.is_running());
        // 持有期间重入被拒绝。
        assert!(!gate.try_begin());
        gate.finish();
        // 释放后可再次占用（任务 3 的 30 分钟错误重试依赖此语义）。
        assert!(gate.try_begin());
        gate.finish();
        assert!(gate.try_begin());
    }

    /// 任务 4.3：skipped 语义判定——开关关闭 / 无 Token 都落到 LocalOnly
    /// （= 坚果云与播出数据步骤仍执行、仅 Bangumi 网络段跳过）。
    #[cfg(feature = "standard")]
    #[test]
    fn bangumi_sync_scope_local_only_when_disabled_or_no_token() {
        assert_eq!(
            bangumi_sync_scope(false, true),
            BangumiSyncScope::LocalOnly {
                reason: "Bangumi 同步未启用"
            }
        );
        // 无 Token：LocalOnly（回归锁定——不再整体早退，坚果云步骤仍执行）。
        assert_eq!(
            bangumi_sync_scope(true, false),
            BangumiSyncScope::LocalOnly {
                reason: "尚未保存 Bangumi Token"
            }
        );
        assert_eq!(
            bangumi_sync_scope(false, false),
            BangumiSyncScope::LocalOnly {
                reason: "Bangumi 同步未启用"
            }
        );
        assert_eq!(bangumi_sync_scope(true, true), BangumiSyncScope::Full);
    }

    /// 任务 4.3：核心编排回归——LocalOnly（无 Token）作用域下坚果云与播出
    /// 数据步骤仍执行、Bangumi 网络段跳过（report 零值）、lastFullSyncAt 落
    /// 状态（Android 前台补偿跨进程节流依据）；webdav=None（本机未启用坚果
    /// 云）时静默跳过、不写 lastSyncError。
    #[cfg(feature = "standard")]
    #[test]
    fn full_sync_core_without_token_still_runs_webdav_and_schedule() {
        let data_dir = std::env::temp_dir().join(format!(
            "anilog-phase4-core-{}-{}",
            std::process::id(),
            now_millis()
        ));
        fs::create_dir_all(&data_dir).unwrap();
        let context = AppContext {
            state: Arc::new(Mutex::new(default_state(false))),
            runtime: Arc::new(Mutex::new(json!({}))),
            data_dir: data_dir.clone(),
            cache_dir: data_dir.join("cache"),
            client: reqwest::Client::new(),
            original: false,
            sync_wakeup: Arc::new(tokio::sync::Notify::new()),
            webdav_wakeup: Arc::new(tokio::sync::Notify::new()),
            webdav_sync_lock: Arc::new(tokio::sync::Mutex::new(())),
            #[cfg(desktop)]
            main_window_opening: Arc::new(AtomicBool::new(false)),
            bangumi_lookup_lock: Arc::new(tokio::sync::Mutex::new(())),
            bangumi_unavailable_until: Arc::new(AtomicI64::new(0)),
            offline_bangumi: Arc::new(json!({})),
            // MemoryTokenStore：无 Token → LocalOnly 作用域。
            bangumi_tokens: Arc::new(bangumi::MemoryTokenStore::new()),
            bangumi_username_cache: Arc::new(Mutex::new(None)),
        };
        let webdav_calls = Arc::new(AtomicI64::new(0));
        let schedule_calls = Arc::new(AtomicI64::new(0));

        let payload = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio test runtime")
            .block_on(async {
                let webdav_future = {
                    let calls = Arc::clone(&webdav_calls);
                    async move {
                        calls.fetch_add(1, Ordering::AcqRel);
                        Ok::<Value, anyhow::Error>(json!({"ok": true}))
                    }
                };
                let schedule_future = {
                    let calls = Arc::clone(&schedule_calls);
                    async move {
                        calls.fetch_add(1, Ordering::AcqRel);
                        Ok::<Value, String>(json!({}))
                    }
                };
                run_full_bangumi_sync_core(
                    &context,
                    bangumi_sync_scope(false, false),
                    Some(webdav_future),
                    schedule_future,
                )
                .await
                .expect("core sync succeeds")
            });

        // skipped 消息 + 零值 report；两步骤均执行。
        assert_eq!(payload["ok"], true);
        assert_eq!(
            payload["message"],
            "Bangumi 同步未启用；坚果云与播出数据已按需刷新"
        );
        assert_eq!(payload["report"]["pulled"], 0);
        assert_eq!(payload["report"]["pushed"], 0);
        assert_eq!(webdav_calls.load(Ordering::Acquire), 1);
        assert_eq!(schedule_calls.load(Ordering::Acquire), 1);
        // lastFullSyncAt / lastWebDavSyncAt 已落状态（前台补偿节流依据），
        // 无错误（Bangumi 段跳过不算错误）。
        let state = context.state.lock().unwrap();
        let status: bangumi::BangumiSyncStatus =
            serde_json::from_value(state["bangumiSyncStatus"].clone()).unwrap();
        assert!(status.last_full_sync_at.is_some());
        assert!(status.last_web_dav_sync_at.is_some());
        assert!(status.last_bangumi_sync_at.is_some());
        assert_eq!(status.last_sync_error.as_deref(), Some(""));
        drop(state);

        // webdav=None（LocalOnly 且本机未启用坚果云）：静默跳过、无错误、
        // 不更新 lastWebDavSyncAt。
        let payload = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio test runtime")
            .block_on(async {
                let no_webdav: Option<std::future::Ready<anyhow::Result<Value>>> = None;
                run_full_bangumi_sync_core(
                    &context,
                    BangumiSyncScope::LocalOnly {
                        reason: "尚未保存 Bangumi Token",
                    },
                    no_webdav,
                    async { Ok::<Value, String>(json!({})) },
                )
                .await
                .expect("core sync succeeds")
            });
        assert_eq!(
            payload["message"],
            "尚未保存 Bangumi Token；播出数据已按需刷新"
        );
        let state = context.state.lock().unwrap();
        let status: bangumi::BangumiSyncStatus =
            serde_json::from_value(state["bangumiSyncStatus"].clone()).unwrap();
        assert_eq!(status.last_sync_error.as_deref(), Some(""));
        drop(state);
        let _ = fs::remove_dir_all(&data_dir);
    }

    #[cfg(feature = "original")]
    #[test]
    fn original_bangumi_phase3_surfaces_unchanged() {
        // 三命令统一拒绝（sync_now 携带零值 report 的固定文案）。
        let rejected = bangumi_sync_now_rejected();
        assert_eq!(rejected["ok"], false);
        assert_eq!(rejected["message"], "Original 版不支持 Bangumi");
        assert_eq!(rejected["report"]["pulled"], 0);
        assert_eq!(rejected["report"]["suggestions"], json!([]));
        // original 无 bangumiSyncStatus 默认键、merge 不补。
        assert!(default_state(true).get("bangumiSyncStatus").is_none());
        // 取消追番不写取消队列（该机制不存在于 original 编译产物）。
        let mut state = default_state(true);
        state["following"] = json!([followed(1, 1_000)]);
        assert!(remove_following(&mut state, 1));
        assert!(state.get("pendingBangumiUnfollows").is_none());
    }

    // -----------------------------------------------------------------------
    // PC 验收修复（问题 A-E）：public_state 别名 / 跨键合并 / 追番判重 /
    // AniList 播出覆盖与完结钳制 / 墓碑回归。
    // -----------------------------------------------------------------------

    // -- 问题 A：public_state 注入 bangumiSyncSettings -----------------------

    #[cfg(feature = "standard")]
    #[test]
    fn public_state_injects_bangumi_sync_settings_alias() {
        let mut state = default_state(false);
        state["bangumi"]["syncEnabled"] = json!(true);
        state["bangumi"]["apiBaseUrl"] = json!("https://proxy.example.com/v0");

        inject_public_state_aliases(&mut state, false);

        // 前端读取的 bangumiSyncSettings 与顶层 bangumi 块一致。
        assert_eq!(state["bangumiSyncSettings"], state["bangumi"]);
        assert_eq!(state["bangumiSyncSettings"]["syncEnabled"], true);
        assert_eq!(
            state["bangumiSyncSettings"]["apiBaseUrl"],
            "https://proxy.example.com/v0"
        );
        // bangumiSyncStatus 顶层同名透传不受影响。
        assert!(state["bangumiSyncStatus"].is_object());
        // original 运行旗标（original edition 等价路径）：不注入。
        let mut state = default_state(true);
        inject_public_state_aliases(&mut state, true);
        assert!(state.get("bangumiSyncSettings").is_none());
    }

    #[cfg(feature = "original")]
    #[test]
    fn public_state_original_never_injects_bangumi_sync_settings() {
        let mut state = default_state(true);
        inject_public_state_aliases(&mut state, true);
        assert!(state.get("bangumiSyncSettings").is_none());
        assert!(state.get("bangumi").is_none());
    }

    #[cfg(all(feature = "standard", desktop))]
    #[test]
    fn public_state_on_context_carries_bangumi_sync_settings() {
        let data_dir = std::env::temp_dir().join(format!(
            "anilog-public-state-{}-{}",
            std::process::id(),
            now_millis()
        ));
        fs::create_dir_all(&data_dir).unwrap();
        let context = AppContext {
            state: Arc::new(Mutex::new(default_state(false))),
            runtime: Arc::new(Mutex::new(json!({}))),
            data_dir: data_dir.clone(),
            cache_dir: data_dir.join("cache"),
            client: reqwest::Client::new(),
            original: false,
            sync_wakeup: Arc::new(tokio::sync::Notify::new()),
            webdav_wakeup: Arc::new(tokio::sync::Notify::new()),
            webdav_sync_lock: Arc::new(tokio::sync::Mutex::new(())),
            main_window_opening: Arc::new(AtomicBool::new(false)),
            bangumi_lookup_lock: Arc::new(tokio::sync::Mutex::new(())),
            bangumi_unavailable_until: Arc::new(AtomicI64::new(0)),
            offline_bangumi: Arc::new(json!({})),
            bangumi_tokens: Arc::new(bangumi::MemoryTokenStore::new()),
            bangumi_username_cache: Arc::new(Mutex::new(None)),
        };
        let public = context.public_state();
        assert_eq!(public["bangumiSyncSettings"], public["bangumi"]);
        assert!(public["bangumiSyncStatus"].is_object());
        let _ = fs::remove_dir_all(&data_dir);
    }

    // -- 问题 B / E：跨键去重合并与墓碑回归 ----------------------------------

    #[cfg(feature = "standard")]
    fn cross_key_state() -> Value {
        let mut state = default_state(false);
        state["following"] = json!([
            {
                "id": 21355, "source": "anilist", "anilistId": null, "mapping": null,
                "mappingPending": false,
                "title": {"romaji": "Mushoku Tensei III", "english": null, "native": null},
                "displayTitle": "Mushoku Tensei III",
                "coverImage": "https://anilist.example/cover.jpg",
                "episodes": 12, "format": "TV", "seasonYear": 2026,
                "followedAt": 0, "syncUpdatedAt": 5_000
            },
            {
                "id": 45678, "source": "bangumi", "anilistId": 21355, "bangumiId": 45678,
                "title": {"native": "無職転生 III", "english": null, "romaji": null},
                "displayTitle": "无职转生 III",
                "coverImage": "",
                "episodes": null, "format": "TV", "seasonYear": 2026,
                "followedAt": 0, "syncUpdatedAt": 6_000
            }
        ]);
        state["tasks"] = json!([
            {"id": "21355-1", "animeId": 21355, "animeTitle": "Mushoku Tensei III", "episode": 1, "airingAt": 10, "status": "completed", "createdAt": 10, "completedAt": 20, "syncUpdatedAt": 4_000},
            {"id": "21355-2", "animeId": 21355, "animeTitle": "Mushoku Tensei III", "episode": 2, "airingAt": 20, "status": "pending", "createdAt": 20, "completedAt": null, "syncUpdatedAt": 4_500},
            {"id": "45678-2", "animeId": 45678, "animeTitle": "无职转生 III", "episode": 2, "airingAt": 20, "status": "pending", "createdAt": 20, "completedAt": null, "syncUpdatedAt": 5_000},
            {"id": "45678-3", "animeId": 45678, "animeTitle": "无职转生 III", "episode": 3, "airingAt": 30, "status": "completed", "createdAt": 30, "completedAt": 40, "syncUpdatedAt": 5_500},
            {"id": "21355-4", "animeId": 21355, "animeTitle": "Mushoku Tensei III", "episode": 4, "airingAt": 40, "status": "completed", "createdAt": 40, "completedAt": 50, "syncUpdatedAt": 9_000},
            {"id": "45678-4", "animeId": 45678, "animeTitle": "无职转生 III", "episode": 4, "airingAt": 40, "status": "completed", "createdAt": 40, "completedAt": 50, "syncUpdatedAt": 8_000},
            {"id": "21355-5", "animeId": 21355, "animeTitle": "Mushoku Tensei III", "episode": 5, "airingAt": 50, "status": "pending", "createdAt": 50, "completedAt": null, "syncUpdatedAt": 9_500},
            {"id": "45678-5", "animeId": 45678, "animeTitle": "无职转生 III", "episode": 5, "airingAt": 50, "status": "pending", "createdAt": 50, "completedAt": null, "syncUpdatedAt": 5_000}
        ]);
        state
    }

    #[cfg(feature = "standard")]
    fn cross_key_map() -> Value {
        json!({
            "version": 2,
            "bySubject": {
                "45678": {"b": 45678, "a": 21355, "c": "无职转生 III", "t": "Mushoku Tensei III", "d": "2026-07-08", "f": "tv"}
            },
            "anilistIndex": {"21355": 45678}
        })
    }

    /// 离线映射空底座（跨键守卫测试用：无映射 → 行为与旧版一致）。
    #[cfg(feature = "standard")]
    fn empty_offline_map() -> Value {
        json!({"bySubject": {}, "anilistIndex": {}})
    }

    #[cfg(feature = "standard")]
    #[test]
    fn reconcile_merges_cross_key_duplicate_into_bangumi_entry() {
        let map = cross_key_map();
        let mut state = cross_key_state();

        assert!(reconcile_following_entries(&mut state, &map, false));

        // 只剩 subjectId 键条目；旧 AniList 键条目删除并写墓碑。
        let following = state["following"].as_array().unwrap();
        assert_eq!(following.len(), 1);
        assert_eq!(following[0]["id"], 45678);
        assert_eq!(following[0]["source"], "bangumi");
        assert!(value_i64(state["syncMetadata"]["followingDeletedAt"].get("21355")) > 0);
        // 展示字段补齐：coverImage 来自旧条目、episodes 补缺。
        assert_eq!(
            following[0]["coverImage"],
            "https://anilist.example/cover.jpg"
        );
        assert_eq!(following[0]["episodes"], 12);

        // 任务裁决（canonicalize_cross_key_tasks 规范化语义：同集唯一权威
        // 记录，一律归一到 subjectId 键）：
        // - ep1：仅旧键 completed → 历史原样保留但重键 "45678-1"（不丢
        //   completedAt）；
        // - ep2：双 pending，新键较新（5000 > 4500）→ 保留 45678-2；
        // - ep3：仅新键 completed → 保留；
        // - ep4：双 completed，旧键较新（9000 > 8000）→ 内容取 21355-4，
        //   规范化为 45678-4；
        // - ep5：双 pending，旧键较新（9500 > 5000）→ 旧记录胜出并重键 45678-5。
        let tasks = state["tasks"].as_array().unwrap();
        let ids: Vec<String> = tasks
            .iter()
            .map(|task| value_string(task.get("id")))
            .collect();
        assert!(ids.contains(&"45678-1".to_string()));
        assert!(ids.contains(&"45678-2".to_string()));
        assert!(ids.contains(&"45678-3".to_string()));
        assert!(ids.contains(&"45678-4".to_string()));
        assert!(ids.contains(&"45678-5".to_string()));
        assert_eq!(ids.len(), 5);
        assert!(
            tasks
                .iter()
                .all(|task| value_i64(task.get("animeId")) == 45678)
        );
        let episode1 = tasks
            .iter()
            .find(|task| value_string(task.get("id")) == "45678-1")
            .expect("normalized episode 1 task");
        assert_eq!(episode1["status"], "completed");
        assert_eq!(episode1["completedAt"], 20);
        let episode4 = tasks
            .iter()
            .find(|task| value_string(task.get("id")) == "45678-4")
            .expect("normalized episode 4 task");
        assert_eq!(episode4["status"], "completed");
        assert_eq!(episode4["completedAt"], 50);
        let episode5 = tasks
            .iter()
            .find(|task| value_string(task.get("id")) == "45678-5")
            .expect("rekeyed episode 5 task");
        assert_eq!(episode5["animeId"], 45678);
        assert_eq!(episode5["subjectId"], 45678);
        assert_eq!(episode5["animeTitle"], "无职转生 III");

        // 幂等：再次 reconcile 无变更。
        assert!(!reconcile_following_entries(&mut state, &map, false));
    }

    #[cfg(feature = "standard")]
    #[test]
    fn reconcile_applies_single_entry_mapping_for_unbound_entries() {
        let map = cross_key_map();
        let mut state = default_state(false);
        state["following"] = json!([
            {
                "id": 21355, "source": "anilist", "mapping": null, "mappingPending": false,
                "title": {"romaji": "Mushoku Tensei III", "english": null, "native": "無職転生 III"},
                "displayTitle": "Mushoku Tensei III",
                "format": "TV", "seasonYear": 2026,
                "startDate": {"year": 2026, "month": 7, "day": 8},
                "followedAt": 1_000, "syncUpdatedAt": 1_000
            }
        ]);

        assert!(reconcile_following_entries(&mut state, &map, false));

        // 无并存 bangumi 条目 → 走单条自动映射（离线表精确命中 → high 绑定）。
        let following = state["following"].as_array().unwrap();
        assert_eq!(following.len(), 1);
        assert_eq!(following[0]["id"], 45678);
        assert_eq!(following[0]["source"], "bangumi");
        assert_eq!(following[0]["mapping"]["confidence"], "high");
        assert_eq!(following[0]["mappingPending"], false);
        assert!(value_i64(state["syncMetadata"]["followingDeletedAt"].get("21355")) > 0);

        // original：不执行任何 reconcile。
        let mut state = default_state(false);
        state["following"] = json!([{"id": 21355, "followedAt": 1, "syncUpdatedAt": 1}]);
        assert!(!reconcile_following_entries(&mut state, &map, true));
        assert_eq!(state["following"][0]["id"], 21355);
    }

    #[cfg(feature = "standard")]
    #[test]
    fn reconcile_tombstone_blocks_later_webdav_resurrection() {
        // 问题 E 回归：跨键合并写墓碑后，坚果云对端文档仍带旧键记录（syncUpdatedAt
        // 新于墓碑）→ merge 先复活 → reconcile 立即再合并；墓碑时间 ≥ 记录时间
        // 保证后续合并不再复活（不冲突）。
        let map = cross_key_map();
        let mut state = cross_key_state();
        assert!(reconcile_following_entries(&mut state, &map, false));
        let tombstone = value_i64(state["syncMetadata"]["followingDeletedAt"].get("21355"));

        let remote = json!({
            "version": SYNC_VERSION,
            "following": [{
                "id": 21355, "source": "anilist",
                "title": {"romaji": "Mushoku Tensei III", "english": null, "native": null},
                "displayTitle": "Mushoku Tensei III",
                "coverImage": "https://anilist.example/cover.jpg",
                "followedAt": 1_000, "syncUpdatedAt": tombstone + 1_000
            }],
            "tasks": [{
                "id": "21355-6", "animeId": 21355, "animeTitle": "Mushoku Tensei III",
                "episode": 6, "airingAt": 60, "status": "pending", "createdAt": 60,
                "completedAt": null, "syncUpdatedAt": tombstone + 1_000
            }],
            "followingDeletedAt": {}
        });
        let (changed, _, _) = merge_document_into_state(&mut state, &remote).unwrap();
        assert!(changed, "远端记录时间戳新于墓碑 → 旧键记录先被合并复活");

        // WebDAV 合并后的 reconcile（perform_webdav_sync / mobile sync_webdav
        // 挂载序列）：跨键合并掉复活记录，任务重键保留。
        assert!(reconcile_following_entries(&mut state, &map, false));
        let following = state["following"].as_array().unwrap();
        assert_eq!(following.len(), 1);
        assert_eq!(following[0]["id"], 45678);
        let new_tombstone = value_i64(state["syncMetadata"]["followingDeletedAt"].get("21355"));
        assert!(new_tombstone >= tombstone + 1_000);
        let tasks = state["tasks"].as_array().unwrap();
        assert!(
            tasks
                .iter()
                .any(|task| value_string(task.get("id")) == "45678-6"
                    && value_string(task.get("status")) == "pending")
        );

        // 再次合并同一远端文档：墓碑 ≥ 记录时间 → 不再复活。
        merge_document_into_state(&mut state, &remote).unwrap();
        assert_eq!(state["following"].as_array().unwrap().len(), 1);
        assert!(
            state["tasks"]
                .as_array()
                .unwrap()
                .iter()
                .all(|task| value_i64(task.get("animeId")) != 21355
                    || value_string(task.get("status")) == "completed")
        );
    }

    // -- 问题 C：toggle_follow 跨键守卫 --------------------------------------

    #[cfg(feature = "standard")]
    #[test]
    fn follow_bangumi_card_merges_existing_anilist_key_entry() {
        // 两侧记录并存时 follow Bangumi 卡片：合并而非新增第二条。
        let mut state = cross_key_state();
        let anime = json!({
            "id": 45678, "source": "bangumi", "anilistId": 21355, "bangumiSubjectId": 45678,
            "nameCn": "无职转生 III",
            "title": {"native": "無職転生 III", "romaji": "Mushoku Tensei III", "english": null},
            "coverImage": {"medium": "https://lain.bgm.tv/pic/cover/m/45678.jpg"},
            "format": "TV", "episodes": 12, "seasonYear": 2026
        });

        add_following_entry_standard(&mut state, &anime, &empty_offline_map());

        let following = state["following"].as_array().unwrap();
        assert_eq!(following.len(), 1);
        assert_eq!(following[0]["id"], 45678);
        assert_eq!(following[0]["source"], "bangumi");
        assert_eq!(following[0]["mapping"]["method"], "manual");
        assert_eq!(following[0]["mapping"]["confidence"], "high");
        assert_eq!(following[0]["displayTitle"], "无职转生 III");
        assert_eq!(
            following[0]["coverImage"],
            "https://lain.bgm.tv/pic/cover/m/45678.jpg"
        );
        assert!(value_i64(state["syncMetadata"]["followingDeletedAt"].get("21355")) > 0);
        // 旧键条目不再有 pending 任务（重键/去重到 subjectId）。
        assert!(state["tasks"].as_array().unwrap().iter().all(|task| {
            value_i64(task.get("animeId")) != 21355
                || value_string(task.get("status")) == "completed"
        }));
    }

    #[cfg(feature = "standard")]
    #[test]
    fn follow_bangumi_card_promotes_lone_anilist_entry() {
        // 仅旧 AniList 键记录时 follow Bangumi 卡片：转正（apply_mapping），
        // 绝不新增第二条。
        let mut state = default_state(false);
        state["following"] = json!([{
            "id": 21355, "source": "anilist", "mapping": null, "mappingPending": false,
            "title": {"romaji": "Mushoku Tensei III", "english": null, "native": null},
            "displayTitle": "Mushoku Tensei III",
            "coverImage": "https://anilist.example/cover.jpg",
            "followedAt": 1_000, "syncUpdatedAt": 1_000
        }]);
        state["tasks"] = json!([{
            "id": "21355-1", "animeId": 21355, "animeTitle": "Mushoku Tensei III",
            "episode": 1, "airingAt": 10, "status": "pending", "createdAt": 10,
            "completedAt": null, "syncUpdatedAt": 1_000
        }]);
        let anime = json!({
            "id": 45678, "source": "bangumi", "anilistId": 21355, "bangumiSubjectId": 45678,
            "nameCn": "无职转生 III",
            "title": {"native": "無職転生 III", "romaji": null, "english": null},
            "coverImage": {"medium": "https://lain.bgm.tv/pic/cover/m/45678.jpg"},
            "format": "TV", "episodes": 12, "seasonYear": 2026
        });

        add_following_entry_standard(&mut state, &anime, &empty_offline_map());

        let following = state["following"].as_array().unwrap();
        assert_eq!(following.len(), 1);
        assert_eq!(following[0]["id"], 45678);
        assert_eq!(following[0]["anilistId"], 21355);
        assert_eq!(following[0]["mapping"]["method"], "manual");
        assert_eq!(following[0]["displayTitle"], "无职转生 III");
        // pending 任务重键到 subjectId。
        assert_eq!(state["tasks"][0]["id"], "45678-1");
        assert_eq!(state["tasks"][0]["animeId"], 45678);
        assert!(value_i64(state["syncMetadata"]["followingDeletedAt"].get("21355")) > 0);
    }

    #[cfg(feature = "standard")]
    #[test]
    fn follow_anilist_card_binds_existing_bangumi_entry() {
        // 反向：follow AniList 卡片（id 为 anilistId）而存在 anilistId 匹配的
        // bangumi 条目 → 绑定 manual/high，不新增第二条。
        let mut state = default_state(false);
        state["following"] = json!([{
            "id": 45678, "source": "bangumi", "anilistId": 21355, "bangumiId": 45678,
            "title": {"native": "無職転生 III", "english": null, "romaji": null},
            "displayTitle": "无职转生 III",
            "mapping": {"method": "local", "confidence": "low", "updatedAt": 1},
            "mappingPending": false,
            "followedAt": 1_000, "syncUpdatedAt": 1_000
        }]);
        let anime = json!({
            "id": 21355,
            "title": {"native": "Mushoku Tensei III", "romaji": "Mushoku Tensei III", "english": null},
            "coverImage": {"medium": "https://anilist.example/cover.jpg"},
            "format": "TV", "episodes": 12, "seasonYear": 2026
        });

        add_following_entry_standard(&mut state, &anime, &empty_offline_map());

        let following = state["following"].as_array().unwrap();
        assert_eq!(following.len(), 1);
        assert_eq!(following[0]["id"], 45678);
        assert_eq!(following[0]["mapping"]["method"], "manual");
        assert_eq!(following[0]["mapping"]["confidence"], "high");
        assert!(value_i64(following[0].get("syncUpdatedAt")) > 1_000);
    }

    #[cfg(feature = "standard")]
    #[test]
    fn follow_without_cross_key_still_pushes_single_entry() {
        // 无跨键重复：普通 follow 行为不变（新增单条）。
        let mut state = default_state(false);
        let anime = json!({
            "id": 45678, "source": "bangumi", "anilistId": null, "bangumiSubjectId": 45678,
            "nameCn": "示例动画",
            "title": {"native": "サンプル", "romaji": null, "english": null},
            "coverImage": {"medium": "https://lain.bgm.tv/pic/cover/m/45678.jpg"},
            "format": "TV", "episodes": 12, "seasonYear": 2026
        });

        add_following_entry_standard(&mut state, &anime, &empty_offline_map());

        let following = state["following"].as_array().unwrap();
        assert_eq!(following.len(), 1);
        assert_eq!(following[0]["id"], 45678);
        assert_eq!(following[0]["displayTitle"], "示例动画");
    }

    // -- 跨键重追复活（取消后从另一侧卡片重追，墓碑/取消队列不得残留）--------

    #[cfg(feature = "standard")]
    #[test]
    fn refollow_via_anilist_card_revives_tombstoned_bangumi_entry() {
        // 主场景：bangumi S 取消（墓碑+取消队列）→ 从 AniList 卡片 A 重追
        // → 复活 S 键条目、清墓碑、撤销取消队列；绝不新增 A 键第二条。
        let map = cross_key_map();
        let mut state = default_state(false);
        state["following"] = json!([bangumi_following(45678, json!({"anilistId": 21355}))]);
        assert!(remove_following(&mut state, 45678));
        assert!(following_tombstone_exists(&state, 45678));
        assert!(queue_contains_subject(&state, 45678));

        let anilist_card = json!({
            "id": 21355,
            "title": {"native": "無職転生 III", "romaji": "Mushoku Tensei III", "english": null},
            "coverImage": {"medium": "https://anilist.example/cover.jpg"},
            "format": "TV", "episodes": 12, "seasonYear": 2026
        });
        add_following_entry_standard(&mut state, &anilist_card, &map);

        let following = state["following"].as_array().unwrap();
        assert_eq!(following.len(), 1, "复活为单条 S 键条目");
        assert_eq!(following[0]["id"], 45678);
        assert_eq!(following[0]["source"], "bangumi");
        assert_eq!(following[0]["anilistId"], 21355);
        assert_eq!(following[0]["mapping"]["method"], "manual");
        assert_eq!(following[0]["mapping"]["confidence"], "high");
        assert_eq!(following[0]["lastChangedBy"], "local");
        assert_eq!(following[0]["displayTitle"], "无职转生 III");
        // S 墓碑清除（复活语义），取消队列撤销。
        assert!(!following_tombstone_exists(&state, 45678));
        assert!(!queue_contains_subject(&state, 45678));

        // 幂等：同一卡片再次重追（条目已存在）不新增、不走复活。
        add_following_entry_standard(&mut state, &anilist_card, &map);
        assert_eq!(state["following"].as_array().unwrap().len(), 1);
        assert_eq!(state["following"][0]["id"], 45678);
    }

    #[cfg(feature = "standard")]
    #[test]
    fn refollow_via_bangumi_card_revives_cancelled_anilist_entry() {
        // 对称场景：anilist A 取消（A 墓碑）→ 从 bangumi 卡片 S 重追 → 单条
        // S 键复活，A 墓碑一并清除（重追即撤销删除意图）。
        let map = cross_key_map();
        let mut state = default_state(false);
        state["following"] = json!([{
            "id": 21355, "source": "anilist", "anilistId": null, "mapping": null,
            "mappingPending": false,
            "title": {"romaji": "Mushoku Tensei III", "english": null, "native": null},
            "displayTitle": "Mushoku Tensei III",
            "followedAt": 1_000, "syncUpdatedAt": 1_000
        }]);
        assert!(remove_following(&mut state, 21355));
        assert!(following_tombstone_exists(&state, 21355));

        let bangumi_card = json!({
            "id": 45678, "source": "bangumi", "anilistId": 21355, "bangumiSubjectId": 45678,
            "nameCn": "无职转生 III",
            "title": {"native": "無職転生 III", "romaji": "Mushoku Tensei III", "english": null},
            "coverImage": {"medium": "https://lain.bgm.tv/pic/cover/m/45678.jpg"},
            "format": "TV", "episodes": 12, "seasonYear": 2026
        });
        add_following_entry_standard(&mut state, &bangumi_card, &map);

        let following = state["following"].as_array().unwrap();
        assert_eq!(following.len(), 1);
        assert_eq!(following[0]["id"], 45678);
        assert_eq!(following[0]["source"], "bangumi");
        assert_eq!(following[0]["lastChangedBy"], "local");
        assert!(!following_tombstone_exists(&state, 21355), "A 墓碑清除");
        assert!(!following_tombstone_exists(&state, 45678));
    }

    #[cfg(feature = "standard")]
    #[test]
    fn refollow_same_card_keeps_single_entry_and_revokes_queue() {
        // 无墓碑残留的普通重追回归（先取消后立刻重追同卡）：单条、墓碑清除、
        // 取消队列即时撤销；S 从未存在时 AniList 卡片重追仍是普通 A 键新增。
        let map = cross_key_map();
        // 1) bangumi 卡片同卡重追。
        let mut state = default_state(false);
        state["following"] = json!([bangumi_following(45678, json!({}))]);
        assert!(remove_following(&mut state, 45678));
        let bangumi_card = json!({
            "id": 45678, "source": "bangumi", "anilistId": null, "bangumiSubjectId": 45678,
            "nameCn": "无职转生 III",
            "title": {"native": "無職転生 III", "romaji": "Mushoku Tensei III", "english": null},
            "coverImage": {"medium": "https://lain.bgm.tv/pic/cover/m/45678.jpg"},
            "format": "TV", "episodes": 12, "seasonYear": 2026
        });
        add_following_entry_standard(&mut state, &bangumi_card, &map);
        assert_eq!(state["following"].as_array().unwrap().len(), 1);
        assert_eq!(state["following"][0]["id"], 45678);
        assert!(!following_tombstone_exists(&state, 45678));
        assert!(!queue_contains_subject(&state, 45678));

        // 2) AniList 卡片重追（S 有映射但从未被追过、无墓碑）→ 普通 A 键新增。
        let mut state = default_state(false);
        let anilist_card = json!({
            "id": 21355,
            "title": {"native": "無職転生 III", "romaji": "Mushoku Tensei III", "english": null},
            "coverImage": {"medium": "https://anilist.example/cover.jpg"},
            "format": "TV", "episodes": 12, "seasonYear": 2026
        });
        add_following_entry_standard(&mut state, &anilist_card, &map);
        let following = state["following"].as_array().unwrap();
        assert_eq!(following.len(), 1);
        assert_eq!(following[0]["id"], 21355);
        assert_eq!(following[0]["source"], "anilist");
    }

    #[cfg(feature = "standard")]
    fn queue_contains_subject(state: &Value, subject_id: i64) -> bool {
        state
            .get("pendingBangumiUnfollows")
            .and_then(Value::as_array)
            .is_some_and(|items| {
                items
                    .iter()
                    .any(|item| value_i64(item.get("subjectId")) == subject_id)
            })
    }

    #[cfg(feature = "standard")]
    #[test]
    fn push_after_cross_key_refollow_writes_type3_not_type5() {
        // 写回门禁：S 取消（墓碑+队列）→ AniList 卡片 A 重追复活 S 条目后，
        // push_local_changes 对 S 走 type=3（doing）而非 type=5（抛弃）。
        use crate::bangumi::test_support::MockBangumiServer;

        let profile = include_str!("../fixtures/bangumi/user-profile.json").to_string();
        let server = MockBangumiServer::spawn(Arc::new(move |method, target, _headers, _body| {
            if target == "/v0/me" {
                return (200, vec![], profile.clone());
            }
            if target.starts_with("/v0/users/anilog_dev/collections/45678") {
                // 远端无收藏记录 → 探测 404 → POST 创建。
                return (404, vec![], "{}".into());
            }
            if method == "POST" && target == "/v0/users/-/collections/45678" {
                return (204, vec![], String::new());
            }
            (404, vec![], "{}".into())
        }));
        let mut state = phase3_state("https://unused.example.com/v0");
        state["following"] = json!([bangumi_following(45678, json!({"anilistId": 21355}))]);
        assert!(remove_following(&mut state, 45678));
        let anilist_card = json!({
            "id": 21355,
            "title": {"native": "無職転生 III", "romaji": "Mushoku Tensei III", "english": null},
            "coverImage": {"medium": "https://anilist.example/cover.jpg"},
            "format": "TV", "episodes": 12, "seasonYear": 2026
        });
        add_following_entry_standard(&mut state, &anilist_card, &cross_key_map());
        assert_eq!(state["following"][0]["id"], 45678);
        assert!(!queue_contains_subject(&state, 45678));

        let state = std::sync::Mutex::new(state);
        let tokens = bangumi::MemoryTokenStore::new();
        tokens.store("refollow-token").unwrap();
        let username_cache = std::sync::Mutex::new(None);
        let client =
            bangumi::HttpBangumiClient::with_base(bangumi_test_base(&server.url())).unwrap();
        let episodes_cache = episodes_cache_dir("cross-key-refollow");
        let push = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio test runtime")
            .block_on(bangumi_sync::push_local_changes(
                &client,
                &tokens,
                &username_cache,
                &state,
                &episodes_cache,
            ));
        assert!(push.errors.is_empty(), "{:?}", push.errors);
        assert_eq!(push.pushed, 1);
        let requests = server.requests();
        // 写回必须是 type=3（复活条目按 doing 处理），绝不允许残留队列的 type=5。
        assert!(
            requests
                .iter()
                .all(|request| !request.body.contains("\"type\":5"))
        );
        let created = requests
            .into_iter()
            .find(|request| {
                request.method == "POST" && request.target == "/v0/users/-/collections/45678"
            })
            .expect("POST create for revived entry");
        let payload: Value = serde_json::from_str(&created.body).unwrap();
        assert_eq!(payload["type"], 3);
    }

    // -- 问题 D：AniList 补充覆盖 / 完结钳制 / 月度并行 -----------------------

    #[cfg(feature = "standard")]
    #[test]
    fn bangumi_season_chain_enriches_from_anilist_override() {
        use crate::bangumi::test_support::MockBangumiServer;

        let server = MockBangumiServer::spawn(Arc::new(|method, target, _headers, body| {
            if target.starts_with("/v0/subjects?") {
                // 两个条目：45700（AniList 12345，RELEASING）、45701（AniList 999，
                // FINISHED——已完结番仍显示下集的回归）。
                let page = json!({
                    "total": 2, "limit": 50, "offset": 0,
                    "data": [bangumi_season_subject(45700, "TV"), bangumi_season_subject(45701, "TV")]
                });
                (200, vec![], page.to_string())
            } else if method == "POST" {
                // AniList GraphQL 补充覆盖请求（id_in 批量）。
                assert!(body.contains("SeasonAniListEnrich"));
                assert!(body.contains("12345") && body.contains("999"));
                let data = json!({"data": {"Page": {"pageInfo": {"lastPage": 1}, "media": [
                    {"id": 12345, "status": "RELEASING", "episodes": 13, "duration": 24,
                     "genres": ["Action", "Fantasy"], "averageScore": 85,
                     "bannerImage": "https://img.anilist.co/banner.jpg",
                     "studios": {"nodes": [{"name": "Studio Bind"}]},
                     "nextAiringEpisode": {"episode": 4, "airingAt": 1_800_000_000, "timeUntilAiring": 86_400},
                     "airingSchedule": {"nodes": [
                        {"episode": 1, "airingAt": 1_768_400_000},
                        {"episode": 2, "airingAt": 1_768_900_000},
                        {"episode": 3, "airingAt": 1_769_000_000}]}},
                    {"id": 999, "status": "FINISHED", "episodes": 12,
                     "nextAiringEpisode": {"episode": 13, "airingAt": 1_800_000_000, "timeUntilAiring": 86_400}}
                ]}}});
                (200, vec![], data.to_string())
            } else {
                (404, vec![], "{}".into())
            }
        }));
        let directory = std::env::temp_dir().join(format!(
            "anilog-season-enrich-{}-{}",
            std::process::id(),
            server.port()
        ));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).unwrap();

        let map = json!({
            "version": 2,
            "bySubject": {
                // 45700：离线映射带 anilistId 关联、无播出数据 → nextAiringEpisode
                // 由 AniList 覆盖供给。
                "45700": {"b": 45700, "a": 12345, "c": "示例动画 45700", "t": "サンプルアニメ 45700", "d": "2026-07-08", "f": "tv"},
                // 45701：有播出数据（推算非空）但 AniList 已完结 → 钳制为 null。
                "45701": offline_entry(999, json!("2026-07-08T13:00:22Z"), json!("R/2026-07-08T13:00:22.000Z/P7D"), Value::Null)
            },
            "anilistIndex": {"12345": 45700, "999": 45701}
        });
        let state = season_chain_state("https://unused.example.com/v0");
        let client = reqwest::Client::new();
        let anilist_source = AniListSeasonSource {
            client: &client,
            endpoint: &server.url(),
        };

        let fetch = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio test runtime")
            .block_on(fetch_season_bangumi_chain(
                &client,
                bangumi_test_base(&server.url()),
                &directory,
                &map,
                &state,
                "SUMMER",
                2026,
                Some(&anilist_source),
            ));

        let SeasonFetch::Bangumi { anime, .. } = fetch else {
            panic!("expected bangumi fetch");
        };
        assert_eq!(anime.len(), 2);
        let releasing = anime
            .iter()
            .find(|item| value_i64(item.get("id")) == 45700)
            .expect("releasing entry");
        // nextAiringEpisode 用 AniList 值（权威，修正 bangumi-data 平台首播星期）。
        assert_eq!(releasing["nextAiringEpisode"]["episode"], 4);
        assert_eq!(releasing["nextAiringEpisode"]["airingAt"], 1_800_000_000);
        // airingSchedule 填入（前端星期分组依赖）。
        assert_eq!(
            releasing["airingSchedule"]["nodes"]
                .as_array()
                .unwrap()
                .len(),
            3
        );
        // 补充字段只补缺：duration/status/genres/bannerImage/studios 填入；
        // episodes/averageScore 保留 bangumi 已有值（13/85 不覆盖 12/7.5）。
        assert_eq!(releasing["duration"], 24);
        assert_eq!(releasing["status"], "RELEASING");
        assert_eq!(releasing["genres"].as_array().unwrap().len(), 2);
        assert_eq!(
            releasing["bannerImage"],
            "https://img.anilist.co/banner.jpg"
        );
        assert_eq!(releasing["studios"]["nodes"][0]["name"], "Studio Bind");
        assert_eq!(releasing["episodes"], 12);
        assert_eq!(releasing["averageScore"], 7.5);

        let finished = anime
            .iter()
            .find(|item| value_i64(item.get("id")) == 45701)
            .expect("finished entry");
        // AniList status FINISHED → nextAiringEpisode=null（完结钳制）。
        assert!(finished["nextAiringEpisode"].is_null());
        assert_eq!(finished["status"], "FINISHED");

        // 覆盖结果随缓存落盘。
        let cached: Value =
            serde_json::from_str(&fs::read_to_string(directory.join("2026-SUMMER.json")).unwrap())
                .unwrap();
        assert_eq!(cached["anime"][0]["nextAiringEpisode"]["episode"], 4);

        let _ = fs::remove_dir_all(&directory);
    }

    #[cfg(feature = "standard")]
    #[test]
    fn bangumi_season_chain_anilist_failure_keeps_bangumi_values() {
        use crate::bangumi::test_support::MockBangumiServer;

        let server = MockBangumiServer::spawn(Arc::new(|method, target, _headers, _body| {
            if target.starts_with("/v0/subjects?") {
                // eps 拉高保证 begin 推算的期号在任意运行日期都非空、未被完结钳制。
                let mut subject = bangumi_season_subject(45700, "TV");
                subject["eps"] = json!(100_000);
                let page = json!({
                    "total": 1, "limit": 50, "offset": 0,
                    "data": [subject]
                });
                (200, vec![], page.to_string())
            } else if method == "POST" {
                (500, vec![], "{}".into())
            } else {
                (404, vec![], "{}".into())
            }
        }));
        let directory = std::env::temp_dir().join(format!(
            "anilog-season-enrich-fail-{}-{}",
            std::process::id(),
            server.port()
        ));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).unwrap();

        let map = json!({
            "version": 2,
            "bySubject": {
                "45700": offline_entry(12345, json!("2026-07-08T13:00:22Z"), json!("R/2026-07-08T13:00:22.000Z/P7D"), Value::Null)
            },
            "anilistIndex": {}
        });
        let state = season_chain_state("https://unused.example.com/v0");
        let client = reqwest::Client::new();
        let anilist_source = AniListSeasonSource {
            client: &client,
            endpoint: &server.url(),
        };

        let fetch = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio test runtime")
            .block_on(fetch_season_bangumi_chain(
                &client,
                bangumi_test_base(&server.url()),
                &directory,
                &map,
                &state,
                "SUMMER",
                2026,
                Some(&anilist_source),
            ));

        // AniList 失败：静默保留 bangumi-data 计算值，季度链整体不受影响。
        let SeasonFetch::Bangumi { anime, .. } = fetch else {
            panic!("expected bangumi fetch");
        };
        assert_eq!(anime.len(), 1);
        assert!(value_i64(anime[0]["nextAiringEpisode"].get("episode")) > 0);
        assert!(anime[0].get("airingSchedule").is_none());

        let _ = fs::remove_dir_all(&directory);
    }

    #[cfg(feature = "standard")]
    #[test]
    fn bangumi_season_fetch_concurrency_bounded_by_two() {
        use crate::bangumi::test_support::MockBangumiServer;

        let in_flight = Arc::new(std::sync::atomic::AtomicI64::new(0));
        let max_in_flight = Arc::new(std::sync::atomic::AtomicI64::new(0));
        let server = MockBangumiServer::spawn({
            let in_flight = Arc::clone(&in_flight);
            let max_in_flight = Arc::clone(&max_in_flight);
            Arc::new(move |_method, target, _headers, _body| {
                let current = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                max_in_flight.fetch_max(current, Ordering::SeqCst);
                // 阻塞一段时间制造重叠窗口，使并发可观测。
                std::thread::sleep(std::time::Duration::from_millis(150));
                in_flight.fetch_sub(1, Ordering::SeqCst);
                let mut month = 0u32;
                for pair in target
                    .strip_prefix("/v0/subjects?")
                    .unwrap_or_default()
                    .split('&')
                {
                    let mut parts = pair.splitn(2, '=');
                    if parts.next() == Some("month") {
                        month = parts.next().unwrap_or("").parse().unwrap_or(0);
                    }
                }
                let page = json!({
                    "total": 1, "limit": 50, "offset": 0,
                    "data": [bangumi_season_subject(45900 + i64::from(month), "TV")]
                });
                (200, vec![], page.to_string())
            })
        });

        let http = bangumi::HttpBangumiClient::with_base(bangumi_test_base(&server.url())).unwrap();
        let subjects = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio test runtime")
            .block_on(fetch_season_bangumi_subjects(&http, "SUMMER", 2026))
            .expect("season subjects");
        // 三个月各 1 条 → 合并 3 条；并发不超过 HttpBangumiClient 的
        // Semaphore(2)，且确实发生了并行（≥2）。
        assert_eq!(subjects.len(), 3);
        let max = max_in_flight.load(Ordering::SeqCst);
        assert!(max >= 2, "三月应并行拉取（观察最大并发 {max}）");
        assert!(max <= 2, "并发不得超过 Semaphore(2)（观察 {max}）");
    }

    // -- 验收第 2 轮修复回归测试 ------------------------------------------------

    #[cfg(feature = "standard")]
    #[test]
    fn resolve_sync_enabled_implicit_enable_and_explicit_false_wins() {
        // 问题 1 回归：patch 只含 push_local_changes=true → syncEnabled 隐式 true。
        assert_eq!(
            resolve_sync_enabled(None, [None, Some(true), None, None]),
            Some(true)
        );
        // 四个子开关任一为 true 都触发隐式启用。
        assert_eq!(
            resolve_sync_enabled(None, [Some(true), None, None, None]),
            Some(true)
        );
        assert_eq!(
            resolve_sync_enabled(None, [None, None, Some(true), None]),
            Some(true)
        );
        assert_eq!(
            resolve_sync_enabled(None, [None, None, None, Some(true)]),
            Some(true)
        );
        // 显式 sync_enabled=false 不被子开关覆盖。
        assert_eq!(
            resolve_sync_enabled(
                Some(false),
                [Some(true), Some(true), Some(true), Some(true)]
            ),
            Some(false)
        );
        // 显式 true 保持；全无提供时不动现状。
        assert_eq!(
            resolve_sync_enabled(Some(true), [None, None, None, None]),
            Some(true)
        );
        assert_eq!(resolve_sync_enabled(None, [None, None, None, None]), None);
        // 子开关显式 false 不触发隐式启用。
        assert_eq!(
            resolve_sync_enabled(None, [Some(false), Some(false), Some(false), Some(false)]),
            None
        );
    }

    #[cfg(feature = "standard")]
    #[test]
    fn toggle_task_status_marks_bangumi_completion_local() {
        // 问题 2a 回归：subjectId 齐备的 bangumi 任务完成 → lastChangedBy=local。
        let mut task = json!({
            "id": "140001-5", "animeId": 140001, "subjectId": 140001,
            "episodeId": 987654, "episode": 5, "status": "pending"
        });
        assert!(toggle_task_status(&mut task));
        assert_eq!(task["status"], "completed");
        assert_eq!(task["lastChangedBy"], "local");
        assert!(task["completedAt"].is_number());

        // anilist 键任务（无 subjectId）：行为不变，不写 lastChangedBy。
        let mut anilist_task = json!({"id": "21355-5", "animeId": 21355, "status": "pending"});
        assert!(toggle_task_status(&mut anilist_task));
        assert_eq!(anilist_task["status"], "completed");
        assert!(anilist_task.get("lastChangedBy").is_none());

        // subjectId 为 null 的任务：不写 lastChangedBy。
        let mut null_subject =
            json!({"id": "1-1", "animeId": 1, "subjectId": null, "status": "pending"});
        assert!(toggle_task_status(&mut null_subject));
        assert!(null_subject.get("lastChangedBy").is_none());

        // 取消完成（completed → pending）：不置 local。
        let mut uncomplete = json!({
            "id": "140001-5", "subjectId": 140001, "status": "completed",
            "lastChangedBy": "bangumi"
        });
        assert!(!toggle_task_status(&mut uncomplete));
        assert_eq!(uncomplete["status"], "pending");
        assert_eq!(uncomplete["lastChangedBy"], "bangumi");
        assert!(uncomplete["completedAt"].is_null());
    }

    #[cfg(feature = "standard")]
    #[test]
    fn toggle_follow_standard_marks_last_changed_by_local() {
        // 问题 2a 回归：本地追番的三条路径（新增 / AniList 卡片转正 / 跨键
        // 转正）产出的 bangumi 条目都必须带 lastChangedBy=local，否则写回
        // 引擎永远不会推送该收藏。
        // 1) 新增 bangumi 卡片。
        let mut state = default_state(false);
        let anime = json!({
            "id": 140001, "source": "bangumi", "anilistId": 21355,
            "nameCn": "Re：从零开始的异世界生活",
            "title": {"native": "Re:ゼロから始める異世界生活"},
            "coverImage": {"medium": "https://lain.bgm.tv/pic/cover/m/1.jpg"},
            "episodes": 25
        });
        add_following_entry_standard(&mut state, &anime, &empty_offline_map());
        assert_eq!(state["following"][0]["lastChangedBy"], "local");

        // 2) AniList 卡片 follow，但同作品 bangumi 键条目已存在 → 转正。
        let mut state = default_state(false);
        state["following"] = json!([{
            "id": 140001, "source": "bangumi", "anilistId": 21355,
            "displayTitle": "旧标题", "followedAt": 1, "syncUpdatedAt": 1
        }]);
        let anilist_card = json!({"id": 21355, "title": {"english": "Re:Zero"}, "episodes": 25});
        add_following_entry_standard(&mut state, &anilist_card, &empty_offline_map());
        assert_eq!(state["following"].as_array().unwrap().len(), 1);
        assert_eq!(state["following"][0]["lastChangedBy"], "local");

        // 3) bangumi 卡片 follow，旧 AniList 键记录存在 → 转正为 subjectId 键。
        let mut state = default_state(false);
        state["following"] = json!([{
            "id": 21355, "source": "anilist", "displayTitle": "旧键",
            "followedAt": 1, "syncUpdatedAt": 1
        }]);
        add_following_entry_standard(&mut state, &anime, &empty_offline_map());
        assert_eq!(state["following"].as_array().unwrap().len(), 1);
        assert_eq!(state["following"][0]["id"], 140001);
        assert_eq!(state["following"][0]["lastChangedBy"], "local");
    }

    #[cfg(all(feature = "standard", not(target_os = "android")))]
    #[test]
    fn apply_airing_schedules_skip_episodes_with_completed_history() {
        // 问题 3 回归：旧版完成任务挂在 anilistId 键（"21355-5"），新版按
        // subjectId（"140001-5"）生成——按 id 查重查不到；有观看历史的集
        // 不得重建 pending，无历史的集正常建。
        let mut state = default_state(false);
        state["following"] = json!([{
            "id": 140001, "source": "bangumi", "anilistId": 21355,
            "displayTitle": "黄泉的使者", "followedAt": 0, "syncUpdatedAt": 1
        }]);
        state["tasks"] = json!([{
            "id": "21355-5", "animeId": 21355, "episode": 5,
            "status": "completed", "completedAt": 100, "syncUpdatedAt": 1
        }]);
        state["seenAiringEvents"] = json!([]);
        let schedules_value = json!([
            {"mediaId": 21355, "episode": 5, "airingAt": 50,
             "media": {"nextAiringEpisode": {"episode": 6, "airingAt": 60}}},
            {"mediaId": 21355, "episode": 6, "airingAt": 60,
             "media": {"nextAiringEpisode": {"episode": 7, "airingAt": 70}}}
        ]);
        let schedules = schedules_value.as_array().unwrap();

        let outcome = apply_airing_schedules(&mut state, schedules, 60);

        // ep5 有观看历史 → 不建；ep6 无历史 → 正常建。
        assert_eq!(outcome.aired, 2);
        assert_eq!(outcome.created, 1);
        let ids: Vec<String> = state["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|task| value_string(task.get("id")))
            .collect();
        assert_eq!(ids, vec!["21355-5", "140001-6"]);
        assert_eq!(state["tasks"][1]["subjectId"], 140001);

        // 幂等：重复灌入同一调度集不新增。
        let outcome = apply_airing_schedules(&mut state, &schedules, 60);
        assert_eq!(outcome.created, 0);
    }

    #[cfg(feature = "standard")]
    #[test]
    fn mobile_event_task_guard_skips_episodes_with_completed_history() {
        // 问题 3（Android 原生 aired 事件建任务）内核：mobile::merge_status
        // 依赖 AppHandle 不可直接测，抽取的"事件→应建任务"判定内核按与
        // apply_airing_schedules 相同口径覆盖。旧版 completed 挂 anilistId
        // 键（"21355-5"），bangumi 条目新事件按 subjectId（"140001-5"）→
        // 仅按任务 id 查重查不到，需按已完成集合拦截。
        let tasks = json!([
            {"id": "21355-5", "animeId": 21355, "episode": 5, "status": "completed", "completedAt": 100, "syncUpdatedAt": 1},
            {"id": "140001-7", "animeId": 140001, "subjectId": 140001, "episode": 7, "status": "completed", "completedAt": 200, "syncUpdatedAt": 1},
            // pending 不入已完成集合：ep6 未看过 → 仍应建任务。
            {"id": "140001-6", "animeId": 140001, "subjectId": 140001, "episode": 6, "status": "pending", "syncUpdatedAt": 2}
        ]);
        let history = completed_episode_history(&tasks);
        assert!(history.contains(&(21355, 5)));
        assert!(history.contains(&(140001, 7)));
        assert!(!history.contains(&(140001, 6)));

        // bangumi 条目（S=140001, A=21355）：
        // ep5 命中旧 anilistId 完成键 → 跳过建任务（回归主场景）。
        assert!(completed_history_blocks_event(&history, 140001, 21355, 5));
        // ep7 命中 subjectId 完成键 → 跳过建任务。
        assert!(completed_history_blocks_event(&history, 140001, 21355, 7));
        // ep6 无完成历史 → 正常建任务。
        assert!(!completed_history_blocks_event(&history, 140001, 21355, 6));
        // anilist 键条目（A 无关联）不误伤他番同集号。
        assert!(!completed_history_blocks_event(&history, 999, 0, 5));

        // 幂等：同一事件重复判定结果一致（merge_status 侧反复灌入同一事件
        // 另由 known id 查重兜底，内核判定不随调用次数漂移）。
        let first = completed_history_blocks_event(&history, 140001, 21355, 5);
        assert!(first);
        assert_eq!(
            first,
            completed_history_blocks_event(&history, 140001, 21355, 5)
        );
    }

    #[cfg(feature = "standard")]
    #[test]
    fn canonicalize_absorbs_duplicate_pendings_with_completed_history() {
        // 原 cleanup_bangumi_duplicate_pendings 场景（问题 3：43 待看缩影）
        // 改写为规范化语义：completed 历史在场 → 重复 pending 被删，completed
        // 归一到 subjectId 键且语义字段不丢；无历史 pending 保留；他番不动。
        let mut state = default_state(false);
        state["following"] = json!([{
            "id": 140001, "source": "bangumi", "anilistId": 21355,
            "displayTitle": "黄泉的使者", "followedAt": 0, "syncUpdatedAt": 1
        }]);
        state["tasks"] = json!([
            // 旧版观看历史：completed 挂 anilistId 键，永不删除。
            {"id": "21355-1", "animeId": 21355, "episode": 1, "status": "completed", "completedAt": 10, "syncUpdatedAt": 1},
            {"id": "21355-2", "animeId": 21355, "episode": 2, "status": "completed", "completedAt": 20, "syncUpdatedAt": 1},
            // 新版重复生成的 subjectId 键 pending（有历史）→ 删。
            {"id": "140001-1", "animeId": 140001, "subjectId": 140001, "episode": 1, "status": "pending", "completedAt": null, "syncUpdatedAt": 2},
            {"id": "140001-2", "animeId": 140001, "subjectId": 140001, "episode": 2, "status": "pending", "completedAt": null, "syncUpdatedAt": 2},
            // 无历史集 → 保留。
            {"id": "140001-3", "animeId": 140001, "subjectId": 140001, "episode": 3, "status": "pending", "completedAt": null, "syncUpdatedAt": 2},
            // 他番 pending（无对应追番条目身份）→ 保留且原样不动。
            {"id": "999-1", "animeId": 999, "episode": 1, "status": "pending", "completedAt": null, "syncUpdatedAt": 2}
        ]);

        assert!(canonicalize_cross_key_tasks(&mut state, &json!({}), false));

        let tasks = state["tasks"].as_array().unwrap();
        assert_eq!(tasks.len(), 4);
        let episode1 = tasks
            .iter()
            .find(|task| value_string(task.get("id")) == "140001-1")
            .expect("normalized episode 1");
        assert_eq!(episode1["status"], "completed");
        assert_eq!(episode1["completedAt"], 10);
        assert_eq!(episode1["animeId"], 140001);
        assert_eq!(episode1["subjectId"], 140001);
        let episode2 = tasks
            .iter()
            .find(|task| value_string(task.get("id")) == "140001-2")
            .expect("normalized episode 2");
        assert_eq!(episode2["status"], "completed");
        assert_eq!(episode2["completedAt"], 20);
        let episode3 = tasks
            .iter()
            .find(|task| value_string(task.get("id")) == "140001-3")
            .expect("unique pending kept");
        assert_eq!(episode3["status"], "pending");
        // 无身份归属的他番记录完全不动（无 episodeSortKey 补齐、时间戳不变）。
        let other = tasks
            .iter()
            .find(|task| value_string(task.get("id")) == "999-1")
            .expect("unrelated task kept");
        assert_eq!(other["syncUpdatedAt"], 2);
        assert!(other.get("episodeSortKey").is_none());

        // 幂等：再次规范化零变更。
        assert!(!canonicalize_cross_key_tasks(&mut state, &json!({}), false));

        // completed 键为 subjectId（subjectId==S 命中）时同样清理。
        let mut state = default_state(false);
        state["following"] = json!([
            {"id": 140001, "source": "bangumi", "anilistId": null, "followedAt": 0, "syncUpdatedAt": 1}
        ]);
        state["tasks"] = json!([
            {"id": "140001-5", "animeId": 140001, "subjectId": 140001, "episode": 5, "status": "completed", "completedAt": 10, "syncUpdatedAt": 1},
            {"id": "140001-5b", "animeId": 140001, "subjectId": 140001, "episode": 5, "status": "pending", "completedAt": null, "syncUpdatedAt": 2}
        ]);
        assert!(canonicalize_cross_key_tasks(&mut state, &json!({}), false));
        assert_eq!(state["tasks"].as_array().unwrap().len(), 1);
        assert_eq!(state["tasks"][0]["status"], "completed");
        assert_eq!(state["tasks"][0]["completedAt"], 10);

        // 通过 reconcile_following_entries 接线后同样生效（before/after 捕获变更）：
        // 脏对（completed "21355-5" + pending "140001-5"）→ 唯一权威记录
        // "140001-5" completed。
        let mut state = default_state(false);
        state["following"] = json!([
            {"id": 140001, "source": "bangumi", "anilistId": 21355, "followedAt": 0, "syncUpdatedAt": 1}
        ]);
        state["tasks"] = json!([
            {"id": "21355-5", "animeId": 21355, "episode": 5, "status": "completed", "completedAt": 10, "syncUpdatedAt": 1},
            {"id": "140001-5", "animeId": 140001, "subjectId": 140001, "episode": 5, "status": "pending", "completedAt": null, "syncUpdatedAt": 2}
        ]);
        assert!(reconcile_following_entries(&mut state, &json!({}), false));
        assert_eq!(state["tasks"].as_array().unwrap().len(), 1);
        assert_eq!(state["tasks"][0]["id"], "140001-5");
        assert_eq!(state["tasks"][0]["status"], "completed");
        // 无变更时 reconcile 仍返回 false。
        assert!(!reconcile_following_entries(&mut state, &json!({}), false));
    }

    #[cfg(feature = "standard")]
    #[test]
    fn canonicalize_merges_double_completed_across_keys_keeping_history() {
        // 双 completed 跨键同集 → 只剩一条，completedAt 保留（取全组最新非空）。
        let map = cross_key_map();
        let mut state = default_state(false);
        state["following"] = json!([{
            "id": 45678, "source": "bangumi", "anilistId": 21355, "bangumiId": 45678,
            "title": {"native": "無職転生 III"}, "displayTitle": "无职转生 III",
            "followedAt": 2_000, "syncUpdatedAt": 6_000
        }]);
        state["tasks"] = json!([
            {"id": "21355-5", "animeId": 21355, "animeTitle": "Mushoku Tensei III", "episode": 5, "airingAt": 50, "status": "completed", "createdAt": 30, "completedAt": 40, "syncUpdatedAt": 9_000},
            {"id": "45678-5", "animeId": 45678, "subjectId": 45678, "animeTitle": "无职转生 III", "episode": 5, "airingAt": 50, "status": "completed", "createdAt": 31, "completedAt": 45, "syncUpdatedAt": 8_000}
        ]);

        assert!(canonicalize_cross_key_tasks(&mut state, &map, false));

        let tasks = state["tasks"].as_array().unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0]["id"], "45678-5");
        assert_eq!(tasks[0]["status"], "completed");
        assert_eq!(tasks[0]["completedAt"], 45);
        assert_eq!(tasks[0]["createdAt"], 31);
        // 键被规范化 → 时间戳提到 now（LWW 保证对端采纳）。
        assert!(value_i64(tasks[0].get("syncUpdatedAt")) > 9_000);

        // 幂等。
        assert!(!canonicalize_cross_key_tasks(&mut state, &map, false));
    }

    #[cfg(feature = "standard")]
    #[test]
    fn canonicalize_resolves_empty_anilist_id_via_offline_map() {
        // bangumi 条目 anilistId 为空 → bySubject[S].a 兜底解析出 A → 同样合并。
        let map = cross_key_map();
        let mut state = default_state(false);
        state["following"] = json!([{
            "id": 45678, "source": "bangumi", "anilistId": null, "bangumiId": 45678,
            "title": {"native": "無職転生 III"}, "displayTitle": "无职转生 III",
            "followedAt": 2_000, "syncUpdatedAt": 6_000
        }]);
        state["tasks"] = json!([
            {"id": "21355-5", "animeId": 21355, "episode": 5, "airingAt": 50, "status": "completed", "createdAt": 40, "completedAt": 50, "syncUpdatedAt": 9_000},
            {"id": "45678-5", "animeId": 45678, "subjectId": 45678, "episode": 5, "airingAt": 50, "status": "pending", "createdAt": 50, "completedAt": null, "syncUpdatedAt": 9_500}
        ]);

        assert!(canonicalize_cross_key_tasks(&mut state, &map, false));

        let tasks = state["tasks"].as_array().unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0]["id"], "45678-5");
        assert_eq!(tasks[0]["status"], "completed");
        assert_eq!(tasks[0]["completedAt"], 50);

        // bySubject 缺失时 anilistIndex 反查兜底（A→S 指向已追番的 subject）。
        let reverse_map = json!({"bySubject": {}, "anilistIndex": {"21355": 45678}});
        let mut state = default_state(false);
        state["following"] = json!([{
            "id": 45678, "source": "bangumi", "anilistId": null, "followedAt": 0, "syncUpdatedAt": 1
        }]);
        state["tasks"] = json!([
            {"id": "21355-5", "animeId": 21355, "episode": 5, "status": "completed", "completedAt": 10, "syncUpdatedAt": 1},
            {"id": "45678-5", "animeId": 45678, "subjectId": 45678, "episode": 5, "status": "pending", "completedAt": null, "syncUpdatedAt": 2}
        ]);
        assert!(canonicalize_cross_key_tasks(
            &mut state,
            &reverse_map,
            false
        ));
        let tasks = state["tasks"].as_array().unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0]["id"], "45678-5");
        assert_eq!(tasks[0]["status"], "completed");
    }

    #[cfg(feature = "standard")]
    #[test]
    fn canonicalize_leaves_unmapped_records_untouched() {
        // 无映射关系（A 不在 map、无对应 bangumi 身份）的记录一律不动。
        let mut state = default_state(false);
        state["following"] = json!([{
            "id": 45678, "source": "bangumi", "anilistId": null, "followedAt": 0, "syncUpdatedAt": 1
        }]);
        state["tasks"] = json!([
            {"id": "21355-5", "animeId": 21355, "episode": 5, "status": "completed", "completedAt": 10, "syncUpdatedAt": 1},
            {"id": "999-1", "animeId": 999, "episode": 1, "status": "pending", "completedAt": null, "syncUpdatedAt": 2}
        ]);

        assert!(!canonicalize_cross_key_tasks(&mut state, &json!({}), false));

        let tasks = state["tasks"].as_array().unwrap();
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0]["id"], "21355-5");
        assert_eq!(tasks[0]["syncUpdatedAt"], 1);
        assert_eq!(tasks[1]["id"], "999-1");

        // original：永不执行。
        let mut state = default_state(false);
        state["following"] = json!([{
            "id": 45678, "source": "bangumi", "anilistId": 21355, "followedAt": 0, "syncUpdatedAt": 1
        }]);
        state["tasks"] = json!([
            {"id": "21355-5", "animeId": 21355, "episode": 5, "status": "completed", "completedAt": 10, "syncUpdatedAt": 1},
            {"id": "45678-5", "animeId": 45678, "subjectId": 45678, "episode": 5, "status": "pending", "completedAt": null, "syncUpdatedAt": 2}
        ]);
        assert!(!canonicalize_cross_key_tasks(
            &mut state,
            &cross_key_map(),
            true
        ));
        assert_eq!(state["tasks"].as_array().unwrap().len(), 2);
    }

    #[cfg(feature = "standard")]
    #[test]
    fn canonicalize_heals_dirty_remote_pair_in_upload_document() {
        // 愈合主场景：本机干净 + 远端文档含脏对（completed "21355-5" +
        // pending "45678-5" 同集）→ merge → reconcile（含规范化）→ 上传文档
        // （document_from_state 输出）该作品该集只剩一条且为 completed
        // subjectId 键。远端文档被本轮回写覆盖愈合。
        let map = cross_key_map();
        let mut state = default_state(false);
        state["following"] = json!([{
            "id": 45678, "source": "bangumi", "anilistId": 21355, "bangumiId": 45678,
            "title": {"native": "無職転生 III"}, "displayTitle": "无职转生 III",
            "followedAt": 2_000, "syncUpdatedAt": 6_000
        }]);
        let remote = json!({
            "version": SYNC_VERSION,
            "following": [{
                "id": 45678, "source": "bangumi", "anilistId": 21355, "bangumiId": 45678,
                "title": {"native": "無職転生 III"}, "displayTitle": "无职转生 III",
                "followedAt": 2_000, "syncUpdatedAt": 6_000
            }],
            "tasks": [
                {"id": "21355-5", "animeId": 21355, "animeTitle": "Mushoku Tensei III", "episode": 5, "airingAt": 50, "status": "completed", "createdAt": 40, "completedAt": 50, "syncUpdatedAt": 9_000},
                {"id": "45678-5", "animeId": 45678, "subjectId": 45678, "animeTitle": "无职转生 III", "episode": 5, "airingAt": 50, "status": "pending", "createdAt": 50, "completedAt": null, "syncUpdatedAt": 9_500}
            ],
            "followingDeletedAt": {}
        });

        let (changed, _, _) = merge_document_into_state(&mut state, &remote).unwrap();
        assert!(changed, "远端脏对先按 LWW 合并进本地");
        // perform_webdav_sync / mobile sync_webdav 挂载序列：合并后立即
        // reconcile（含规范化），再 document_from_state 重建上传文档。
        assert!(reconcile_following_entries(&mut state, &map, false));
        let document = document_from_state(&mut state);
        // 规范化清掉了重复键 → 上传文档 != 远端文档（reconcile 分支会以此
        // 重算 remote_changed 并回写坚果云，愈合远端）。
        assert_ne!(
            comparable_document(&remote).ok(),
            comparable_document(&document).ok()
        );
        let tasks = document["tasks"].as_array().unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0]["id"], "45678-5");
        assert_eq!(tasks[0]["animeId"], 45678);
        assert_eq!(tasks[0]["subjectId"], 45678);
        assert_eq!(tasks[0]["status"], "completed");
        assert_eq!(tasks[0]["completedAt"], 50);
        // 权威记录带新时间戳 → 其他 v0.7 设备以 LWW 采纳愈合结果。
        assert!(value_i64(tasks[0].get("syncUpdatedAt")) > 9_500);

        // 幂等：再次 reconcile + 重建文档完全一致。
        assert!(!reconcile_following_entries(&mut state, &map, false));
        let again = document_from_state(&mut state);
        assert_eq!(
            serde_json::to_string(&document).unwrap(),
            serde_json::to_string(&again).unwrap()
        );
    }

    #[cfg(feature = "standard")]
    #[test]
    fn bangumi_sync_loop_kernel_quiet_period_and_gate() {
        // 问题 2b 循环内核：静默期/周期等待时长 + 执行前判定。周期自第 8 轮
        // 起运行时读取设置（wait_duration 第二参数），不再有固定 INTERVAL_SECS。
        use super::bangumi_sync_loop;
        // 动作唤醒 → 30 秒静默期；周期路径 → interval_secs 现值。
        assert_eq!(
            bangumi_sync_loop::wait_duration(false, 3_600),
            std::time::Duration::from_secs(3_600)
        );
        assert_eq!(
            bangumi_sync_loop::wait_duration(true, 3_600),
            std::time::Duration::from_secs(30)
        );
        // 静默期必须短于下限周期（唤醒比周期更快触达）。
        assert!(bangumi_sync_loop::QUIET_SECS < bangumi_sync_loop::interval_secs(5));
        // 执行前判定：无 Token 不跑；门被占不跑；两者齐备才跑。
        assert!(!bangumi_sync_loop::should_execute(false, true));
        assert!(!bangumi_sync_loop::should_execute(true, false));
        assert!(bangumi_sync_loop::should_execute(true, true));
    }

    #[cfg(feature = "standard")]
    #[test]
    fn bangumi_sync_loop_interval_floor_follows_settings() {
        // 第 8 轮问题 3：周期 = max(pollIntervalMinutes, 15) 分钟。设置页
        // "同步间隔"现有选择 1/5/10/15 → 统一抬到下限 15 分钟保护 Bangumi
        // API；缺失/无法读取设置（0/负数）→ 15 分钟兜底；更大的设置值原样
        // 透传（上限 1440 分钟与桌面主同步循环钳制同口径）。
        use super::bangumi_sync_loop;
        assert_eq!(bangumi_sync_loop::interval_secs(5), 15 * 60);
        assert_eq!(bangumi_sync_loop::interval_secs(1), 15 * 60);
        assert_eq!(bangumi_sync_loop::interval_secs(10), 15 * 60);
        assert_eq!(bangumi_sync_loop::interval_secs(15), 15 * 60);
        assert_eq!(bangumi_sync_loop::interval_secs(0), 15 * 60);
        assert_eq!(bangumi_sync_loop::interval_secs(-3), 15 * 60);
        assert_eq!(bangumi_sync_loop::interval_secs(30), 30 * 60);
        assert_eq!(bangumi_sync_loop::interval_secs(1_440), 1_440 * 60);
        assert_eq!(bangumi_sync_loop::interval_secs(5_000), 1_440 * 60);
    }

    #[cfg(feature = "standard")]
    #[test]
    fn subject_image_url_prefers_uncropped_large_then_medium_small() {
        // 问题 4 回归：角色 images 链 large → common → medium → small → grid；
        // large 是未裁剪全身图（medium/small 是中心方形裁剪，只剩腰以下）。
        let parse = |value: Value| -> bangumi::BangumiSubjectImages {
            serde_json::from_value(value).expect("valid images object")
        };
        // large 优先。
        let all = parse(json!({
            "large": "https://lain.bgm.tv/pic/crt/l/00/00/1_a.jpg",
            "medium": "https://lain.bgm.tv/pic/crt/m/00/00/1_a.jpg",
            "small": "https://lain.bgm.tv/pic/crt/s/00/00/1_a.jpg"
        }));
        assert_eq!(
            bangumi_commands::subject_image_url(Some(&all)),
            json!("https://lain.bgm.tv/pic/crt/l/00/00/1_a.jpg")
        );
        // 缺 large → medium；缺 medium → small（此前 small 未反序列化，会落 null）。
        let medium_only = parse(json!({"medium": "https://lain.bgm.tv/pic/crt/m/00/00/1_a.jpg"}));
        assert_eq!(
            bangumi_commands::subject_image_url(Some(&medium_only)),
            json!("https://lain.bgm.tv/pic/crt/m/00/00/1_a.jpg")
        );
        let small_only = parse(json!({"small": "https://lain.bgm.tv/pic/crt/s/00/00/1_a.jpg"}));
        assert_eq!(
            bangumi_commands::subject_image_url(Some(&small_only)),
            json!("https://lain.bgm.tv/pic/crt/s/00/00/1_a.jpg")
        );
        // 关联条目 common（large 缺失时的全身图）仍优先于 medium。
        let common_only = parse(json!({"common": "https://lain.bgm.tv/pic/cover/c/1.jpg"}));
        assert_eq!(
            bangumi_commands::subject_image_url(Some(&common_only)),
            json!("https://lain.bgm.tv/pic/cover/c/1.jpg")
        );
        // 全缺 / images 为 null → null。
        assert!(bangumi_commands::subject_image_url(None).is_null());
        let empty = parse(json!({}));
        assert!(bangumi_commands::subject_image_url(Some(&empty)).is_null());
    }

    #[cfg(feature = "standard")]
    #[test]
    fn subject_characters_fixture_images_deserialize_small() {
        // 问题 4 fixture 断言：subject-characters.json 的 images 结构必须
        // 解析出 large/medium/small（small 此前被 serde 丢弃）。
        let characters: Vec<bangumi::BangumiCharacter> =
            serde_json::from_str(include_str!("../fixtures/bangumi/subject-characters.json"))
                .expect("fixture parses");
        assert_eq!(characters.len(), 2);
        let images = characters[0].images.as_ref().expect("character images");
        assert_eq!(
            images.large.as_deref(),
            Some("https://lain.bgm.tv/pic/crt/l/00/00/12345_crt_Ab12C.jpg")
        );
        assert_eq!(
            images.medium.as_deref(),
            Some("https://lain.bgm.tv/pic/crt/m/00/00/12345_crt_Ab12C.jpg")
        );
        assert_eq!(
            images.small.as_deref(),
            Some("https://lain.bgm.tv/pic/crt/s/00/00/12345_crt_Ab12C.jpg")
        );
    }

    // -- 状态驱动追踪（任务 1-4）：门控 / 完结转 done / 收藏状态内核 / 写回 type --

    /// 任务 2 门控判定本身：非空且 != doing 才拦截。
    #[cfg(feature = "standard")]
    #[test]
    fn bangumi_status_blocks_tracking_only_non_doing() {
        assert!(bangumi_status_blocks_tracking("wish"));
        assert!(bangumi_status_blocks_tracking("on_hold"));
        assert!(bangumi_status_blocks_tracking("done"));
        assert!(bangumi_status_blocks_tracking("dropped"));
        assert!(!bangumi_status_blocks_tracking("doing"));
        assert!(!bangumi_status_blocks_tracking(""));
    }

    /// 任务 2 门控：wish/on_hold/done 新集不建任务（nextAiringEpisode 展示更新
    /// 保留）；doing 与空状态（anilist 来源兼容）正常建任务。
    #[cfg(all(feature = "standard", not(target_os = "android")))]
    #[test]
    fn airing_schedule_gates_task_creation_by_bangumi_status() {
        let schedule = json!({
            "mediaId": 45678, "episode": 3, "airingAt": 20,
            "media": {"nextAiringEpisode": {"episode": 4, "airingAt": 30}}
        });
        let base = || -> Value {
            let mut state = default_state(false);
            state["following"] = json!([{
                "id": 45678, "source": "bangumi", "bangumiId": 45678,
                "displayTitle": "示例 45678", "followedAt": 0, "syncUpdatedAt": 1
            }]);
            state["tasks"] = json!([]);
            state["seenAiringEvents"] = json!([]);
            state
        };

        // 收录不追踪：三种非 doing 状态都不建任务，但展示字段照常更新。
        for status in ["wish", "on_hold", "done"] {
            let mut state = base();
            state["following"][0]["bangumiStatus"] = json!(status);
            let outcome = apply_airing_schedules(&mut state, &[schedule.clone()], 20);
            assert_eq!(outcome.created, 0, "{status} must not create tasks");
            assert!(state["tasks"].as_array().unwrap().is_empty());
            assert_eq!(state["following"][0]["nextAiringEpisode"]["episode"], 4);
        }

        // doing：恢复追踪 → 正常建任务（subjectId 键）。
        let mut state = base();
        state["following"][0]["bangumiStatus"] = json!("doing");
        let outcome = apply_airing_schedules(&mut state, &[schedule.clone()], 20);
        assert_eq!(outcome.created, 1);
        assert_eq!(state["tasks"][0]["id"], "45678-3");
        assert_eq!(state["tasks"][0]["subjectId"], 45678);

        // 空状态（anilist 来源 / 从未同步）：行为不变。
        let mut state = base();
        let outcome = apply_airing_schedules(&mut state, &[schedule], 20);
        assert_eq!(outcome.created, 1);
        assert_eq!(state["tasks"][0]["id"], "45678-3");
    }

    /// 任务 2 门控（离线调度链）：wish 条目仍生成离线调度（供 nextAiringEpisode
    /// 展示），但 apply_airing_schedules 不为其创建任务。
    #[cfg(all(feature = "standard", not(target_os = "android")))]
    #[test]
    fn bangumi_offline_schedules_wish_entry_updates_display_without_tasks() {
        let mut state = default_state(false);
        state["following"] = json!([{
            "id": 45678, "source": "bangumi", "displayTitle": "Re:从零开始的异世界生活 第3章",
            "episodes": 16, "bangumiStatus": "wish", "followedAt": 0, "syncUpdatedAt": 1
        }]);
        state["tasks"] = json!([]);
        state["seenAiringEvents"] = json!([]);
        let map = json!({
            "bySubject": {
                "45678": offline_entry(
                    21355,
                    json!("2026-07-08T13:00:22Z"),
                    json!("R/2026-07-08T13:00:22.000Z/P7D"),
                    Value::Null
                )
            }
        });
        let now = at("2026-07-19T16:00:00+00:00");

        let schedules = bangumi_offline_schedules(&state, &map, now);
        assert_eq!(schedules.len(), 2, "wish 条目调度照常生成供展示");
        let outcome = apply_airing_schedules(&mut state, &schedules, now);
        assert_eq!(
            outcome,
            AiringOutcome {
                aired: 2,
                created: 0
            }
        );
        assert!(state["tasks"].as_array().unwrap().is_empty());
        assert_eq!(state["following"][0]["nextAiringEpisode"]["episode"], 3);
    }

    #[cfg(feature = "standard")]
    #[test]
    fn bangumi_episode_authority_uses_episode_airdates_not_weekly_formula() {
        let records = vec![
            bangumi::BangumiEpisode {
                id: 1575796,
                ep_type: 0,
                sort: Some(22.0),
                airdate: Some("2026-09-05".into()),
                ..Default::default()
            },
            bangumi::BangumiEpisode {
                id: 1575797,
                ep_type: 0,
                sort: Some(23.0),
                airdate: Some("2026-09-12".into()),
                ..Default::default()
            },
            bangumi::BangumiEpisode {
                id: 1705016,
                ep_type: 0,
                sort: Some(9.0),
                airdate: Some("2026-09-04".into()),
                ..Default::default()
            },
            bangumi::BangumiEpisode {
                id: 1705017,
                ep_type: 0,
                sort: Some(10.0),
                airdate: Some("2026-09-11".into()),
                ..Default::default()
            },
            bangumi::BangumiEpisode {
                id: 1701429,
                ep_type: 0,
                sort: Some(9.0),
                airdate: Some("2026-09-03".into()),
                ..Default::default()
            },
            bangumi::BangumiEpisode {
                id: 1701430,
                ep_type: 0,
                sort: Some(10.0),
                airdate: Some("2026-09-10".into()),
                ..Default::default()
            },
        ];
        let now = chrono::DateTime::parse_from_rfc3339("2026-09-07T12:00:00+08:00")
            .unwrap()
            .timestamp();
        let yomi = bangumi_episode_schedule(&records[..2], now);
        assert_eq!(
            yomi.iter()
                .find(|(episode, _, _, _)| *episode == 22)
                .map(|(_, id, _, aired)| (*id, *aired)),
            Some((1575796, true))
        );
        assert_eq!(
            yomi.iter()
                .find(|(episode, _, _, _)| *episode == 23)
                .map(|(_, id, _, aired)| (*id, *aired)),
            Some((1575797, false))
        );
        let draw = bangumi_episode_schedule(&records[2..4], now);
        assert_eq!(
            draw.iter()
                .find(|(episode, _, _, _)| *episode == 9)
                .map(|(_, id, _, aired)| (*id, *aired)),
            Some((1705016, true))
        );
        assert_eq!(
            draw.iter()
                .find(|(episode, _, _, _)| *episode == 10)
                .map(|(_, id, _, aired)| (*id, *aired)),
            Some((1705017, false))
        );
        let cat = bangumi_episode_schedule(&records[4..], now);
        assert_eq!(
            cat.iter()
                .find(|(episode, _, _, _)| *episode == 9)
                .map(|(_, id, _, aired)| (*id, *aired)),
            Some((1701429, true))
        );
        assert_eq!(
            cat.iter()
                .find(|(episode, _, _, _)| *episode == 10)
                .map(|(_, id, _, aired)| (*id, *aired)),
            Some((1701430, false))
        );
        let precise = HashMap::from([(23, at("2026-09-12T13:30:00+00:00"))]);
        let precise_schedule =
            bangumi_episode_schedule_with_precision(&records[..2], now, &precise);
        assert_eq!(
            precise_schedule
                .iter()
                .find(|(episode, _, _, _)| *episode == 23)
                .map(|(_, _, airing_at, _)| *airing_at),
            Some(at("2026-09-12T13:30:00+00:00"))
        );
        assert_eq!(
            precise_schedule
                .iter()
                .find(|(episode, _, _, _)| *episode == 23)
                .map(|(_, _, _, aired)| *aired),
            Some(false),
            "AniList 的未来精确时间必须覆盖 Bangumi 错误的已播日期判断"
        );
        let mismatched = HashMap::from([(23, at("2026-09-13T13:30:00+00:00"))]);
        let safe_schedule =
            bangumi_episode_schedule_with_precision(&records[..2], now, &mismatched);
        assert_eq!(
            safe_schedule
                .iter()
                .find(|(episode, _, _, _)| *episode == 23)
                .map(|(_, _, airing_at, _)| *airing_at),
            Some(at("2026-09-13T13:30:00+00:00"))
        );
        assert_eq!(
            safe_schedule
                .iter()
                .find(|(episode, _, _, _)| *episode == 23)
                .map(|(_, _, _, aired)| *aired),
            Some(false),
            "改档后的 AniList 日期也必须作为未来集门禁"
        );
    }

    #[cfg(feature = "standard")]
    #[test]
    fn split_subject_precision_never_falls_back_to_local_anilist_number() {
        let now = at("2026-09-07T12:00:00+00:00");
        let records = vec![bangumi::BangumiEpisode {
            id: 1656862,
            ep_type: 0,
            ep: Some(5.0),
            sort: Some(82.0),
            airdate: Some("2026-09-09".into()),
            ..Default::default()
        }];
        // AniList's local ep5 is an old May date; its current split-season ep16
        // is the September 9 airing that belongs to Bangumi local ep5.
        let precise = HashMap::from([
            (5, at("2026-05-06T13:00:00+00:00")),
            (16, at("2026-09-09T13:00:00+00:00")),
        ]);
        let schedule = bangumi_episode_schedule_with_precision(&records, now, &precise);
        assert_eq!(
            schedule,
            vec![(
                5,
                1656862,
                at("2026-09-09T13:00:00+00:00"),
                false
            )]
        );
    }

    #[cfg(feature = "standard")]
    #[test]
    fn bangumi_episode_authority_cleans_old_weekly_offset_state() {
        // 真实安装包曾把 Bangumi-data 的首播锚点按 +7 天展开，造成“下一集号码
        // 已加一、时间却属于上一集”的状态：黄泉 24@9/12、描绘 11@9/11、
        // 尼古 11@9/10，同时留下 23/10/10 的 pending 假票。逐集表纠偏必须
        // 一次性把三条 next 恢复为正确集号，并删除未来假票；已完成历史保留。
        let now = at("2026-09-07T12:00:00+00:00");
        let mut state = default_state(false);
        state["settings"]["createWatchTasks"] = json!(true);
        state["following"] = json!([
            {"id": 568572, "source": "bangumi", "anilistId": 195600,
             "displayTitle": "黄泉的使者", "bangumiStatus": "doing",
             "watchedEpisode": 22,
             "nextAiringEpisode": {"episode": 24, "airingAt": at("2026-09-12T16:00:00+00:00")}},
            {"id": 545917, "source": "bangumi", "anilistId": 188525,
             "displayTitle": "描绘直至生命尽头", "bangumiStatus": "doing",
             "watchedEpisode": 9,
             "nextAiringEpisode": {"episode": 11, "airingAt": at("2026-09-11T15:35:00+00:00")}},
            {"id": 622206, "source": "bangumi", "anilistId": 207141,
             "displayTitle": "尼古喵喵", "bangumiStatus": "doing",
             "watchedEpisode": 9,
             "nextAiringEpisode": {"episode": 11, "airingAt": at("2026-09-10T16:00:06+00:00")}}
        ]);
        state["tasks"] = json!([
            {"id": "568572-23", "animeId": 568572, "subjectId": 568572, "episode": 23,
             "airingAt": at("2026-09-05T16:00:00+00:00"), "status": "pending"},
            {"id": "545917-10", "animeId": 545917, "subjectId": 545917, "episode": 10,
             "airingAt": at("2026-09-04T15:35:00+00:00"), "status": "pending"},
            {"id": "622206-10", "animeId": 622206, "subjectId": 622206, "episode": 10,
             "airingAt": at("2026-09-03T16:00:06+00:00"), "status": "pending"},
            {"id": "568572-22", "animeId": 568572, "subjectId": 568572, "episode": 22,
             "airingAt": at("2026-09-05T16:00:00+00:00"), "status": "completed",
             "completedAt": at("2026-09-06T01:00:00+00:00")}
        ]);
        let records = [
            (
                568572,
                vec![
                    bangumi::BangumiEpisode {
                        id: 1575796,
                        ep_type: 0,
                        sort: Some(22.0),
                        airdate: Some("2026-09-05".into()),
                        ..Default::default()
                    },
                    bangumi::BangumiEpisode {
                        id: 1575797,
                        ep_type: 0,
                        sort: Some(23.0),
                        airdate: Some("2026-09-12".into()),
                        ..Default::default()
                    },
                    bangumi::BangumiEpisode {
                        id: 1575798,
                        ep_type: 0,
                        sort: Some(24.0),
                        airdate: Some("2026-09-19".into()),
                        ..Default::default()
                    },
                ],
            ),
            (
                545917,
                vec![
                    bangumi::BangumiEpisode {
                        id: 1705016,
                        ep_type: 0,
                        sort: Some(9.0),
                        airdate: Some("2026-09-04".into()),
                        ..Default::default()
                    },
                    bangumi::BangumiEpisode {
                        id: 1705017,
                        ep_type: 0,
                        sort: Some(10.0),
                        airdate: Some("2026-09-11".into()),
                        ..Default::default()
                    },
                    bangumi::BangumiEpisode {
                        id: 1705018,
                        ep_type: 0,
                        sort: Some(11.0),
                        airdate: Some("2026-09-18".into()),
                        ..Default::default()
                    },
                ],
            ),
            (
                622206,
                vec![
                    bangumi::BangumiEpisode {
                        id: 1701429,
                        ep_type: 0,
                        sort: Some(9.0),
                        airdate: Some("2026-09-03".into()),
                        ..Default::default()
                    },
                    bangumi::BangumiEpisode {
                        id: 1701430,
                        ep_type: 0,
                        sort: Some(10.0),
                        airdate: Some("2026-09-10".into()),
                        ..Default::default()
                    },
                    bangumi::BangumiEpisode {
                        id: 1701431,
                        ep_type: 0,
                        sort: Some(11.0),
                        airdate: Some("2026-09-17".into()),
                        ..Default::default()
                    },
                ],
            ),
        ];
        for (subject_id, episodes) in records {
            assert!(apply_bangumi_episode_records_to_state(
                &mut state, subject_id, &episodes, now, true, None
            ));
        }
        assert_eq!(state["following"][0]["nextAiringEpisode"]["episode"], 23);
        assert_eq!(state["following"][1]["nextAiringEpisode"]["episode"], 10);
        assert_eq!(state["following"][2]["nextAiringEpisode"]["episode"], 10);
        let pending: Vec<String> = state["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|task| value_string(task.get("status")) == "pending")
            .map(|task| value_string(task.get("id")))
            .collect();
        assert!(
            pending.is_empty(),
            "旧周播偏移产生的三条未来假票必须清理: {pending:?}"
        );
        assert_eq!(state["tasks"].as_array().unwrap().len(), 1);
        assert_eq!(state["tasks"][0]["status"], "completed");
        assert_eq!(state["tasks"][0]["episodeId"], 1575796);
    }

    #[cfg(feature = "standard")]
    #[test]
    fn anilist_precise_future_time_blocks_task_from_stale_bangumi_date() {
        let now = at("2026-09-07T12:00:00+00:00");
        let mut state = default_state(false);
        state["settings"]["createWatchTasks"] = json!(true);
        state["following"] = json!([{
            "id": 45678,
            "source": "bangumi",
            "displayTitle": "黄泉的使者",
            "anilistId": 12345,
            "bangumiStatus": "doing",
            "followedAt": now - 10 * 86_400,
            "watchedEpisode": 22
        }]);
        state["tasks"] = json!([]);
        let records = vec![bangumi::BangumiEpisode {
            id: 1575797,
            ep_type: 0,
            sort: Some(23.0),
            // 模拟已污染的 Bangumi 日期：把 9 月 12 日误报成 9 月 5 日。
            airdate: Some("2026-09-05".into()),
            ..Default::default()
        }];
        let precise = HashMap::from([(23, at("2026-09-12T13:30:00+00:00"))]);

        let changed = apply_bangumi_episode_records_to_state(
            &mut state,
            45678,
            &records,
            now,
            true,
            Some(&precise),
        );

        assert!(changed);
        assert_eq!(state["tasks"].as_array().unwrap().len(), 0);
        assert_eq!(state["following"][0]["nextAiringEpisode"]["episode"], 23);
        assert_eq!(
            state["following"][0]["nextAiringEpisode"]["airingAt"],
            at("2026-09-12T13:30:00+00:00")
        );
    }

    /// 任务 1 内核：四种保留条目的状态 + dropped 取消追番语义 + 无条目报错。
    #[cfg(feature = "standard")]
    #[test]
    fn bangumi_collection_status_kernel_applies_local_semantics() {
        let base = || -> Value {
            let mut state = default_state(false);
            state["following"] = json!([{
                "id": 45678, "source": "bangumi", "bangumiId": 45678,
                "displayTitle": "示例 45678", "bangumiStatus": "doing",
                "followedAt": 0, "syncUpdatedAt": 1
            }]);
            state["tasks"] = json!([
                {"id": "45678-1", "animeId": 45678, "animeTitle": "示例 45678", "episode": 1,
                 "airingAt": 10, "status": "pending", "createdAt": 10, "completedAt": null,
                 "syncUpdatedAt": 1},
                {"id": "45678-2", "animeId": 45678, "animeTitle": "示例 45678", "episode": 2,
                 "airingAt": 10, "status": "completed", "createdAt": 10, "completedAt": 20,
                 "syncUpdatedAt": 1},
                {"id": "99999-1", "animeId": 99999, "animeTitle": "其他", "episode": 1,
                 "airingAt": 10, "status": "pending", "createdAt": 10, "completedAt": null,
                 "syncUpdatedAt": 1}
            ]);
            state
        };

        // wish：pending 删、completed 留、其他作品不动、lastChangedBy=local。
        let mut state = base();
        assert!(apply_bangumi_collection_status(&mut state, 45678, "wish"));
        assert_eq!(state["following"][0]["bangumiStatus"], "wish");
        assert_eq!(state["following"][0]["lastChangedBy"], "local");
        assert!(value_i64(state["following"][0].get("syncUpdatedAt")) > 1);
        let ids: Vec<String> = state["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|task| value_string(task.get("id")))
            .collect();
        assert_eq!(ids, vec!["45678-2".to_string(), "99999-1".to_string()]);

        // on_hold：pending 保留。
        let mut state = base();
        assert!(apply_bangumi_collection_status(
            &mut state, 45678, "on_hold"
        ));
        assert_eq!(state["following"][0]["bangumiStatus"], "on_hold");
        assert_eq!(state["tasks"].as_array().unwrap().len(), 3);

        // done：该作品 pending 全部标记完成（completedAt=now 秒、
        // lastChangedBy=local），已完成任务与其他作品不动。
        let mut state = base();
        assert!(apply_bangumi_collection_status(&mut state, 45678, "done"));
        assert_eq!(state["following"][0]["bangumiStatus"], "done");
        let tasks = state["tasks"].as_array().unwrap();
        let first = tasks
            .iter()
            .find(|task| value_string(task.get("id")) == "45678-1")
            .unwrap();
        assert_eq!(first["status"], "completed");
        assert!(value_i64(first.get("completedAt")) > 0);
        assert_eq!(first["lastChangedBy"], "local");
        let history = tasks
            .iter()
            .find(|task| value_string(task.get("id")) == "45678-2")
            .unwrap();
        assert_eq!(history["completedAt"], 20, "已完成任务不被改写");
        let other = tasks
            .iter()
            .find(|task| value_string(task.get("id")) == "99999-1")
            .unwrap();
        assert_eq!(other["status"], "pending");

        // doing：恢复追踪（pending 保留）。
        let mut state = base();
        assert!(apply_bangumi_collection_status(&mut state, 45678, "doing"));
        assert_eq!(state["following"][0]["bangumiStatus"], "doing");
        assert_eq!(state["tasks"].as_array().unwrap().len(), 3);

        // dropped：复用取消追番（pending 删/completed 留/墓碑/取消队列入列）。
        let mut state = base();
        assert!(apply_bangumi_collection_status(
            &mut state, 45678, "dropped"
        ));
        assert!(
            !state["following"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| value_i64(item.get("id")) == 45678)
        );
        let ids: Vec<String> = state["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|task| value_string(task.get("id")))
            .collect();
        assert_eq!(ids, vec!["45678-2".to_string(), "99999-1".to_string()]);
        assert!(value_i64(state["syncMetadata"]["followingDeletedAt"].get("45678")) > 0);
        assert!(
            state
                .get("pendingBangumiUnfollows")
                .and_then(Value::as_array)
                .is_some_and(|items| items
                    .iter()
                    .any(|item| value_i64(item.get("subjectId")) == 45678))
        );

        // 无条目 → false（命令层返回 ok=false）。
        let mut state = base();
        assert!(!apply_bangumi_collection_status(&mut state, 12345, "doing"));

        // bangumiId 反查定位（id 与 subjectId 不一致的旧记录）。
        let mut state = base();
        state["following"][0]["id"] = json!(1);
        assert!(apply_bangumi_collection_status(
            &mut state, 45678, "on_hold"
        ));
        assert_eq!(state["following"][0]["bangumiStatus"], "on_hold");
    }

    /// 任务 3 内核：完结集完成 → 条目转 done（一次）；非完结/episodes 未知/
    /// 已 done / wish / on_hold 不触发；anilist 键任务经 anilistId 反查。
    #[cfg(feature = "standard")]
    #[test]
    fn finale_completion_marks_entry_done_once() {
        let base = |status: Value| -> Value {
            let mut state = default_state(false);
            let mut entry = json!({
                "id": 45678, "source": "bangumi", "bangumiId": 45678,
                "displayTitle": "示例 45678", "episodes": 12, "followedAt": 0, "syncUpdatedAt": 1
            });
            if !status.is_null() {
                entry["bangumiStatus"] = status;
            }
            state["following"] = json!([entry]);
            state["tasks"] = json!([
                {"id": "45678-12", "animeId": 45678, "animeTitle": "示例 45678", "episode": 12,
                 "airingAt": 10, "status": "completed", "createdAt": 10, "completedAt": 20,
                 "syncUpdatedAt": 1, "subjectId": 45678}
            ]);
            state
        };
        let finale_task =
            json!({"id": "45678-12", "animeId": 45678, "subjectId": 45678, "episode": 12});

        // 完结集完成：doing → done，返回事件载荷（subjectId + displayTitle）。
        let mut state = base(json!("doing"));
        assert_eq!(
            mark_entry_done_on_finale(&mut state, &finale_task),
            Some((45678, "示例 45678".to_string()))
        );
        assert_eq!(state["following"][0]["bangumiStatus"], "done");
        assert_eq!(state["following"][0]["lastChangedBy"], "local");

        // 已 done：重复完成不触发（只触发一次）。
        let mut state = base(json!("done"));
        assert_eq!(mark_entry_done_on_finale(&mut state, &finale_task), None);

        // 非完结集（episode < episodes）不触发。
        let mut state = base(json!("doing"));
        let mid_task = json!({"id": "45678-5", "animeId": 45678, "subjectId": 45678, "episode": 5});
        assert_eq!(mark_entry_done_on_finale(&mut state, &mid_task), None);
        assert_eq!(state["following"][0]["bangumiStatus"], "doing");

        // episodes 未知不触发。
        let mut state = base(json!("doing"));
        state["following"][0]["episodes"] = Value::Null;
        assert_eq!(mark_entry_done_on_finale(&mut state, &finale_task), None);

        // wish / on_hold（收录不追踪）不触发。
        for status in ["wish", "on_hold"] {
            let mut state = base(json!(status));
            assert_eq!(
                mark_entry_done_on_finale(&mut state, &finale_task),
                None,
                "{status} must not trigger finale"
            );
        }

        // 空状态（anilist 迁移条目）触发；任务无 subjectId 时经 anilistId 反查。
        let mut state = base(Value::Null);
        state["following"][0]["anilistId"] = json!(21355);
        let anilist_task = json!({"id": "21355-12", "animeId": 21355, "episode": 12});
        assert_eq!(
            mark_entry_done_on_finale(&mut state, &anilist_task),
            Some((45678, "示例 45678".to_string()))
        );
        assert_eq!(state["following"][0]["bangumiStatus"], "done");
    }

    /// 任务 4：写回 type 由条目 bangumiStatus 映射（on_hold=4 / wish=1 /
    /// done=2 / doing=3），hash 幂等逻辑不受影响。
    #[cfg(feature = "standard")]
    #[test]
    fn bangumi_push_collection_type_follows_bangumi_status() {
        use crate::bangumi::test_support::MockBangumiServer;

        let server = MockBangumiServer::spawn(Arc::new(move |_method, target, _headers, _body| {
            if target.starts_with("/v0/users/-/collections/") {
                return (204, vec![], String::new());
            }
            (404, vec![], "{}".into())
        }));
        let mut state = phase3_state("https://unused.example.com/v0");
        let entry = |id: i64, status: &str| {
            json!({
                "id": id, "source": "bangumi", "bangumiId": id,
                "displayTitle": format!("示例 {id}"), "followedAt": 1, "syncUpdatedAt": 1,
                "bangumiStatus": status, "lastChangedBy": "local",
                "lastPulledPayloadHash": "stale-baseline"
            })
        };
        state["following"] = json!([
            entry(11111, "on_hold"),
            entry(22222, "wish"),
            entry(33333, "done"),
            entry(44444, "doing")
        ]);
        let state = std::sync::Mutex::new(state);
        let tokens = bangumi::MemoryTokenStore::new();
        tokens.store("type-token").unwrap();
        let username_cache = std::sync::Mutex::new(None);
        let client =
            bangumi::HttpBangumiClient::with_base(bangumi_test_base(&server.url())).unwrap();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio test runtime");

        let report = rt.block_on(bangumi_sync::push_local_changes(
            &client,
            &tokens,
            &username_cache,
            &state,
            &episodes_cache_dir("collection-type"),
        ));
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(report.pushed, 4);
        let mut writes: Vec<(i64, u32)> = server
            .requests()
            .iter()
            .filter(|request| request.method == "PATCH")
            .filter_map(|request| {
                let subject_id: i64 = request
                    .target
                    .trim_start_matches("/v0/users/-/collections/")
                    .parse()
                    .ok()?;
                let payload: Value = serde_json::from_str(&request.body).ok()?;
                Some((subject_id, payload["type"].as_u64()? as u32))
            })
            .collect();
        writes.sort_unstable();
        assert_eq!(writes, vec![(11111, 4), (22222, 1), (33333, 2), (44444, 3)]);
        // 推送成功后记账 hash（幂等基线更新）。
        let guard = state.lock().unwrap();
        let held = guard["following"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| value_i64(item.get("id")) == 11111)
            .unwrap();
        assert_eq!(
            held["lastPushedPayloadHash"],
            json!(bangumi_sync::local_collection_hash(held))
        );
    }
}
