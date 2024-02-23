use std::{collections::HashMap, path::PathBuf};

use anyhow::Context;
use axum::{
    extract::Query,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
    Router,
};
use once_cell::sync::Lazy;
use serde::Deserialize;
use tokio::{
    fs::File,
    io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt},
    sync::{RwLock, RwLockReadGuard, RwLockWriteGuard},
};

const VIDEOS_DIR: &str = "videos";

static FILE_NAMES: Lazy<RwLock<Vec<(PathBuf, usize)>>> = Lazy::new(|| RwLock::new(Vec::new()));
static FILE_IDS: Lazy<RwLock<HashMap<String, usize>>> = Lazy::new(|| RwLock::new(HashMap::new()));

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut file_names: RwLockWriteGuard<'_, Vec<_>> = FILE_NAMES.write().await;
    let mut file_ids: RwLockWriteGuard<'_, HashMap<_, _>> = FILE_IDS.write().await;
    for (index, file) in std::fs::read_dir(VIDEOS_DIR)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_file())
        .enumerate()
    {
        let file_name = file.file_name().unwrap().to_string_lossy();
        let Some(file_name_no_extension) = file_name.strip_suffix(".bin") else {
            continue;
        };
        file_ids.insert(file_name_no_extension.to_string(), index);

        let file_size = file.metadata()?.len() as usize;
        file_names.push((file, file_size));
    }
    drop(file_names);
    drop(file_ids);

    let app = Router::new()
        .route("/api/video/frames", get(frames))
        .route("/api/video/info", get(video_info))
        .route("/api/log", post(logger));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:3123").await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("error running HTTP server")
}

#[derive(Deserialize)]
struct FrameInfo {
    id: usize,
    start: usize,
    length: usize,
}

async fn frames(Query(frame_query): Query<FrameInfo>) -> Result<Vec<u8>, AppError> {
    let file_names: RwLockReadGuard<'_, Vec<(PathBuf, usize)>> = FILE_NAMES.read().await;
    let Some((file_name, file_size)) = file_names.get(frame_query.id) else {
        return Err(anyhow::anyhow!("Video with id {} does not exist", frame_query.id).into());
    };
    if frame_query.start * 256 >= *file_size {
        return Err(anyhow::anyhow!("Frame {} does not exist", frame_query.start).into());
    }
    let file_name = file_name.clone();
    let file_size = *file_size;
    drop(file_names);

    let mut file = File::open(file_name).await?;

    file.seek(std::io::SeekFrom::Start((frame_query.start * 256) as u64))
        .await?;

    let content_length = usize::min(
        frame_query.length * 256,
        file_size - frame_query.start * 256,
    );

    let mut buffer = Vec::with_capacity(content_length + 4);
    buffer.write_u32(content_length as u32).await?;
    buffer.extend([0].iter().cycle().take(content_length));
    file.read_exact(&mut buffer[4..]).await?;

    Ok(buffer)
}

#[derive(Deserialize)]
struct VideoInfo {
    name: String,
}

async fn video_info(Query(video_query): Query<VideoInfo>) -> Result<String, AppError> {
    let file_ids: RwLockReadGuard<'_, HashMap<_, usize>> = FILE_IDS.read().await;
    let Some(&file_id) = file_ids.get(&video_query.name) else {
        return Err(anyhow::anyhow!("Video {} does not exist", video_query.name).into());
    };
    drop(file_ids);

    let file_names: RwLockReadGuard<'_, Vec<(PathBuf, usize)>> = FILE_NAMES.read().await;
    let file_size = file_names[file_id].1;
    drop(file_names);

    Ok(format!("{file_id},{file_size}"))
}

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

async fn shutdown_signal() {
    use tokio::signal;

    let ctrl_c = async {
        signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("failed to install signal handler")
            .recv()
            .await;
    };

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}

async fn logger(body: String) {
    println!("{body}");
}
