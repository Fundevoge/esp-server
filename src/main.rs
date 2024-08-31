use std::{future::IntoFuture, ops::DerefMut, time::Duration};

use axum::async_trait;
use serde::{Deserialize, Serialize};
use tokio::{
    sync::Mutex,
    time::{self, Interval},
    try_join,
};
use webserver::{get_video_frames, get_video_metadata, init_server};

mod esp_control;
mod webserver;

static PLAYBACK_STREAM: Mutex<Option<Box<dyn AsyncIterator<Item = DisplayFrame> + Send>>> =
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

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::init();

    let (http_listener, app_router) = init_server().await?;

    let mut playback_stream = PLAYBACK_STREAM.lock().await;
    playback_stream.deref_mut().replace(Box::new(
        VideoFrameIterator::from_file_name("Bad Apple")
            .await
            .unwrap(),
    ));
    drop(playback_stream);

    let handle_stream = tokio::spawn(esp_control::esp_stream_controller());
    let handle_state = tokio::spawn(esp_control::esp_state_controller());
    let handle_time = tokio::spawn(esp_control::esp_time_controller());
    let handle_keepalive = tokio::spawn(esp_control::esp_keepalive());

    let handle_webserver = tokio::spawn(
        axum::serve(http_listener, app_router)
            .with_graceful_shutdown(shutdown_signal())
            .into_future(),
    );
    let (stream_err, state_err, time_err, keepalive_err, webserver_err) = try_join!(
        handle_keepalive,
        handle_state,
        handle_stream,
        handle_time,
        handle_webserver
    )?;
    stream_err?;
    state_err?;
    time_err?;
    keepalive_err?;
    webserver_err?;

    Ok(())
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
        _ = ctrl_c => {
            println!("Received Ctrl-C");
            std::process::exit(1);
        },
        _ = terminate => {
            println!("Received Terminate");
            std::process::exit(1);
        },
    }
}
