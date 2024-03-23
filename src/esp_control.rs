use std::time::{Duration, SystemTime};

use once_cell::sync::Lazy;
use tokio::{
    io::AsyncWriteExt as _,
    net::{TcpListener, TcpStream},
    sync::{mpsc, Mutex},
    task::JoinHandle,
    time::sleep,
};

use crate::{AsyncIterator, DisplayFrame, PLAYBACK_STREAM};

pub async fn start_stream(
    stream: Box<dyn AsyncIterator<Item = DisplayFrame> + Send>,
) -> anyhow::Result<()> {
    let mut playback_stream = PLAYBACK_STREAM.lock().await;
    *playback_stream = Some(stream);
    drop(playback_stream);

    ESP_CONTROL_CHANNEL.0.send(()).await?;
    Ok(())
}

pub async fn esp_stream_controller() -> anyhow::Result<()> {
    let tcp_listener = TcpListener::bind("192.168.178.30:3123").await?;

    loop {
        let (stream, _) = tcp_listener.accept().await?;
        let stream = make_tcp_stream_keepalive(stream)?;
        println!("Handling stream...");

        if let Err(e) = handle_esp_stream_connection(stream).await {
            println!("[ESP] Stream Connection ended with error: {e}");
        }
    }
}

async fn handle_esp_stream_connection(mut tcp_stream: TcpStream) -> anyhow::Result<()> {
    tcp_stream.set_nodelay(true)?;

    {
        let fps = PLAYBACK_STREAM.lock().await.as_ref().unwrap().fps();
        tcp_stream.write_f32_le(fps).await?;
    }

    loop {
        let mut playback_stream = PLAYBACK_STREAM.lock().await;
        let next_frame = playback_stream
            .as_deref_mut()
            .unwrap()
            .next()
            .await
            .unwrap();
        drop(playback_stream);
        tcp_stream.write_all(&next_frame.0).await?;
    }
}

pub async fn esp_state_controller() -> anyhow::Result<()> {
    let tcp_listener = TcpListener::bind("192.168.178.30:3124").await?;

    loop {
        let (stream, _) = tcp_listener.accept().await?;
        let stream = make_tcp_stream_keepalive(stream)?;
        println!("Handling state control...");

        if let Err(e) = handle_esp_state_connection(stream).await {
            println!("[ESP] State Control Connection ended with error: {e}");
        }
    }
}

async fn handle_esp_state_connection(mut tcp_stream: TcpStream) -> anyhow::Result<()> {
    let mut receiver = ESP_CONTROL_CHANNEL.1.lock().await;
    tcp_stream.set_nodelay(true)?;

    loop {
        receiver
            .recv()
            .await
            .expect("State Control channel closed unexpectedly");
        tcp_stream.write_u8(0xff).await?;
    }
}

static ESP_CONTROL_CHANNEL: Lazy<(mpsc::Sender<()>, Mutex<mpsc::Receiver<()>>)> = Lazy::new(|| {
    let (esp_control_sender, esp_control_receiver) = mpsc::channel(256);
    (esp_control_sender, Mutex::new(esp_control_receiver))
});

pub async fn esp_time_controller() -> anyhow::Result<()> {
    let tcp_listener = TcpListener::bind("192.168.178.30:3125").await?;
    let mut last_handle: Option<JoinHandle<anyhow::Result<()>>> = None;

    loop {
        let (stream, _) = tcp_listener.accept().await?;
        let stream = make_tcp_stream_keepalive(stream)?;
        println!("Handling time control...");
        if let Some(last_task_handle) = last_handle {
            if last_task_handle.is_finished() {
                if let Err(e) = last_task_handle.await {
                    println!("[ESP] Time Control Connection ended with error: {e}");
                }
            } else {
                last_task_handle.abort();
                println!("[ESP] Time Control Aborted stale connection");
            }
        }

        last_handle = Some(tokio::spawn(handle_esp_time_connection(stream)));
    }
}

async fn handle_esp_time_connection(mut tcp_stream: TcpStream) -> anyhow::Result<()> {
    tcp_stream.set_nodelay(true)?;
    loop {
        tcp_stream
            .write_u64_le(
                SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .unwrap()
                    .as_secs(),
            )
            .await?;
        sleep(Duration::from_secs(15)).await;
    }
}

fn make_tcp_stream_keepalive(
    stream: tokio::net::TcpStream,
) -> anyhow::Result<tokio::net::TcpStream> {
    let stream: std::net::TcpStream = stream.into_std().unwrap();
    let socket: socket2::Socket = socket2::Socket::from(stream);
    let keepalive = socket2::TcpKeepalive::new()
        .with_time(Duration::from_secs(4))
        .with_interval(Duration::from_secs(2))
        .with_retries(4);
    socket.set_tcp_keepalive(&keepalive)?;
    let stream: std::net::TcpStream = socket.into();
    Ok(tokio::net::TcpStream::from_std(stream).unwrap())
}
