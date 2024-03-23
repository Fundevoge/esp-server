use std::time::{Duration, SystemTime};

use once_cell::sync::Lazy;
use tokio::{
    io::AsyncWriteExt as _,
    net::{TcpListener, TcpStream},
    sync::{mpsc, Mutex},
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

    loop {
        let (stream, _) = tcp_listener.accept().await?;
        println!("Handling time control...");
        if let Err(e) = handle_esp_time_connection(stream).await {
            println!("[ESP] Time Control Connection ended with error: {e}");
        }
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
