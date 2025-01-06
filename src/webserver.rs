use std::{collections::HashMap, path::PathBuf, time::Duration};

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
    Router,
};

use once_cell::sync::Lazy;
use tokio::{
    fs::File,
    io::AsyncReadExt,
    net::TcpListener,
    sync::{RwLock, RwLockWriteGuard},
};

use crate::VideoMetadata;

const VIDEOS_DIR: &str = "videos";
const VIDEOS_META_DIR: &str = "videos_meta";
const SCRIPTS_DIR: &str = "scripts";

static VIDEO_FILE_NAMES: RwLock<Vec<PathBuf>> = RwLock::const_new(Vec::new());
static VIDEO_FILE_IDS: Lazy<RwLock<HashMap<String, usize>>> =
    Lazy::new(|| RwLock::const_new(HashMap::new()));

static VIDEO_META_FILE_NAMES: RwLock<Vec<PathBuf>> = RwLock::const_new(Vec::new());
static VIDEO_META_FILE_IDS: Lazy<RwLock<HashMap<String, usize>>> =
    Lazy::new(|| RwLock::const_new(HashMap::new()));

static SCRIPT_FILE_NAMES: RwLock<Vec<PathBuf>> = RwLock::const_new(Vec::new());
static SCRIPT_FILE_IDS: Lazy<RwLock<HashMap<String, usize>>> =
    Lazy::new(|| RwLock::const_new(HashMap::new()));

struct AppError(anyhow::Error);
impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Something went wrong: {}", self.0),
        )
            .into_response()
    }
}
impl<E> From<E> for AppError
where
    E: Into<anyhow::Error>,
{
    fn from(err: E) -> Self {
        Self(err.into())
    }
}

pub async fn get_video_metadata(file_name: &str) -> Option<VideoMetadata> {
    let videos_meta_file_ids = VIDEO_META_FILE_IDS.read().await;
    let videos_meta_file_names = VIDEO_META_FILE_NAMES.read().await;
    let video_meta_file_id = *videos_meta_file_ids.get(file_name)?;
    let video_meta_file_name = videos_meta_file_names.get(video_meta_file_id)?;
    let mut video_metadata = String::new();
    let mut file = File::open(video_meta_file_name).await.ok()?;
    drop(videos_meta_file_ids);
    drop(videos_meta_file_names);

    file.read_to_string(&mut video_metadata).await.ok()?;
    serde_json::from_str(&video_metadata).ok()
}

pub async fn get_video_frames(file_name: &str) -> Option<Vec<u8>> {
    let videos_file_ids = VIDEO_FILE_IDS.read().await;
    let videos_file_names = VIDEO_FILE_NAMES.read().await;
    let video_file_id = *videos_file_ids.get(file_name)?;
    let video_file_name = videos_file_names.get(video_file_id)?;
    let mut video_frames = Vec::new();
    let mut file = File::open(video_file_name).await.ok()?;
    drop(videos_file_ids);
    drop(videos_file_names);

    file.read_to_end(&mut video_frames).await.ok()?;
    Some(video_frames)
}

fn initialize_file_map(
    dir: &str,
    file_extension: &str,
    mut file_names: RwLockWriteGuard<'_, Vec<PathBuf>>,
    mut file_ids: RwLockWriteGuard<'_, HashMap<String, usize>>,
) -> anyhow::Result<()> {
    for (index, file) in std::fs::read_dir(dir)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_file())
        .enumerate()
    {
        let file_name = file.file_name().unwrap().to_string_lossy();
        let Some(file_name_no_extension) = file_name.strip_suffix(file_extension) else {
            continue;
        };
        file_ids.insert(file_name_no_extension.to_string(), index);

        file_names.push(file);
    }
    drop(file_names);
    drop(file_ids);
    Ok(())
}

pub async fn init_server() -> anyhow::Result<(TcpListener, Router)> {
    initialize_file_map(
        VIDEOS_DIR,
        ".bin",
        VIDEO_FILE_NAMES.write().await,
        VIDEO_FILE_IDS.write().await,
    )?;
    initialize_file_map(
        VIDEOS_META_DIR,
        ".json",
        VIDEO_META_FILE_NAMES.write().await,
        VIDEO_META_FILE_IDS.write().await,
    )?;
    initialize_file_map(
        SCRIPTS_DIR,
        ".js",
        SCRIPT_FILE_NAMES.write().await,
        SCRIPT_FILE_IDS.write().await,
    )?;

    log::info!("[WEB] MAIN Binding Listener...");
    let http_listener = loop {
        let http_listener = TcpListener::bind("192.168.178.30:3122").await;
        match http_listener {
            Ok(http_listener) => break http_listener,
            Err(e) => {
                log::warn!("[WEB] MAIN Binding Listener Failed, retrying...");
                tokio::time::sleep(Duration::from_secs(30)).await;
            }
        }
    };
    log::info!("[WEB] MAIN Listener bound!");
    let app_router = Router::new()
        .route("/", get(root_handler))
        .route("/api/persist_video_upload", post(persist_video_upload))
        .route("/api/temp_video_upload", post(temp_video_upload))
        .route("/api/persist_script_upload", post(persist_script_upload))
        .route("/api/temp_script_upload", post(temp_script_upload))
        .route("/api/ping", get(ping_handler));
    Ok((http_listener, app_router))
}

async fn persist_video_upload() {
    todo!()
}
async fn temp_video_upload() {
    todo!()
}
async fn persist_script_upload() {
    todo!()
}
async fn temp_script_upload() {
    todo!()
}
async fn root_handler() {
    todo!()
}

async fn ping_handler() -> &'static str {
    "hello"
}
