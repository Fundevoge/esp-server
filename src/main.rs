use std::{
    collections::HashMap,
    io::{self, ErrorKind, Read, Write},
    net::TcpListener,
    ops::DerefMut,
    path::PathBuf,
    thread,
    time::Duration,
};

use anyhow::Context;
use axum::{
    async_trait,
    extract::Query,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
    Router,
};
use byteorder::ByteOrder;
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use tokio::{
    fs::File,
    io::AsyncReadExt,
    sync::{Mutex, RwLock, RwLockWriteGuard},
    time::{self, Interval},
};

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

static PLAYBACK_STREAM: Mutex<Option<Box<dyn AsyncIterator<Item = DisplayFrame> + Sync + Send>>> =
    Mutex::const_new(None);

#[async_trait]
pub trait AsyncIterator {
    type Item;
    async fn next(&mut self) -> Option<Self::Item>;
    fn fps(&self) -> f32;
}

#[derive(Serialize, Deserialize, Clone, Copy)]
struct VideoMetadata {
    #[serde(rename = "FPS")]
    fps: f32,
}

struct VideoFrameIterator {
    ticker: Interval,
    buffer: Vec<u8>,
    offset: usize,
    metadata: VideoMetadata,
}

async fn get_video_metadata(file_name: &str) -> Option<VideoMetadata> {
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

async fn get_video_frames(file_name: &str) -> Option<Vec<u8>> {
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

impl VideoFrameIterator {
    async fn from_file_name(file_name: &str) -> Option<Self> {
        let metadata = get_video_metadata(file_name).await?;
        let ticker = time::interval(Duration::from_micros((1e6 / metadata.fps).round() as u64));
        let frames = get_video_frames(file_name).await?;

        Some(VideoFrameIterator {
            ticker,
            buffer: frames,
            offset: 0,
            metadata,
        })
    }
}

#[derive(Debug)]
struct DisplayFrame([u8; 256]);

#[async_trait]
impl AsyncIterator for VideoFrameIterator {
    type Item = DisplayFrame;

    async fn next(&mut self) -> Option<DisplayFrame> {
        if self.offset + 256 > self.buffer.len() {
            self.offset = 0;
        }
        let next_frame = DisplayFrame(
            self.buffer[self.offset..self.offset + 256]
                .try_into()
                .unwrap(),
        );
        self.offset += 256;
        self.ticker.tick().await;
        Some(next_frame)
    }

    fn fps(&self) -> f32 {
        self.metadata.fps
    }
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

#[tokio::main]
async fn main() -> anyhow::Result<()> {
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

    let mut playback_stream = PLAYBACK_STREAM.lock().await;
    playback_stream.deref_mut().replace(Box::new(
        VideoFrameIterator::from_file_name("Bad Apple")
            .await
            .unwrap(),
    ));
    drop(playback_stream);

    let app_router = Router::new()
        .route("/", get(root_handler))
        .route("/api/persist_video_upload", post(persist_video_upload))
        .route("/api/temp_video_upload", post(temp_video_upload))
        .route("/api/persist_script_upload", post(persist_script_upload))
        .route("/api/temp_script_upload", post(temp_script_upload))
        .route("/api/ping", get(ping_handler));

    let http_listener = tokio::net::TcpListener::bind("192.168.178.30:3122").await?;
    thread::spawn(controller);
    axum::serve(http_listener, app_router)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("error running HTTP server")
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

fn controller() -> anyhow::Result<()> {
    let tcp_listener = TcpListener::bind("192.168.178.30:3123")?;

    while let Ok((stream, _)) = tcp_listener.accept() {
        if let Err(e) = handle_esp_client(stream) {
            println!("[ESP] Connection ended with error: {e}");
        }
    }
    Ok(())
}

fn handle_esp_client(mut tcp_stream: std::net::TcpStream) -> anyhow::Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    tcp_stream.set_nodelay(true)?;
    let mut client_is_receiving = false;
    let mut receive_buffer = [0_u8; 1];

    loop {
        if client_is_receiving {
            let mut playback_stream_lock = rt.block_on(PLAYBACK_STREAM.lock());
            let frame_iter = playback_stream_lock.as_deref_mut().unwrap();
            let next_frame = rt.block_on(frame_iter.next()).unwrap();
            tcp_stream.write_all(&next_frame.0)?;
        } else {
            thread::sleep(Duration::from_millis(50));
        }

        match tcp_stream.read(&mut receive_buffer) {
            Ok(1) => {
                println!("\n[ESP] Sent: {}", receive_buffer[0]);
                io::stdout().flush()?;
                client_is_receiving = receive_buffer[0] == 0xff;
                if client_is_receiving {
                    let fps = rt.block_on(PLAYBACK_STREAM.lock()).as_ref().unwrap().fps();
                    let mut temp_buf = [0_u8; 4];
                    byteorder::LE::write_f32(&mut temp_buf, fps);
                    tcp_stream.write_all(&temp_buf)?;
                }
            }
            Err(e) => match e.kind() {
                ErrorKind::WouldBlock => {}
                _ => {
                    println!("Error reading from tcp socket: {e}");
                }
            },
            _ => {}
        }
    }
}
