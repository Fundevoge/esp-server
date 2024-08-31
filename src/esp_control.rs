use std::{
    os::unix::process::CommandExt,
    process::Command,
    time::{Duration, SystemTime},
};

use once_cell::sync::Lazy;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt as _},
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
    log::info!("[ESP] STREAM Binding controller...");
    let tcp_listener = TcpListener::bind("192.168.178.30:3123").await?;
    log::info!("[ESP] STREAM Controller bound!");

    loop {
        let (stream, _) = tcp_listener.accept().await?;
        log::info!("[ESP] STREAM Client connected!");
        if let Err(e) = handle_esp_stream_connection(stream).await {
            log::warn!("[ESP] STREAM Connection ended with error: {e}");
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
    log::info!("[ESP] STATE Binding controller...");
    let tcp_listener = TcpListener::bind("192.168.178.30:3124").await?;
    log::info!("[ESP] STATE Controller bound!");

    loop {
        let (stream, _) = tcp_listener.accept().await?;
        log::info!("[ESP] STATE Client connected!");
        if let Err(e) = handle_esp_state_connection(stream).await {
            log::warn!("[ESP] STATE Connection ended with error: {e}");
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
    log::info!("[ESP] TIME Binding controller...");
    let tcp_listener = TcpListener::bind("192.168.178.30:3125").await?;
    log::info!("[ESP] TIME Controller bound!");

    loop {
        let (stream, _) = tcp_listener.accept().await?;
        log::info!("[ESP] TIME Client connected!");
        if let Err(e) = handle_esp_time_connection(stream).await {
            log::warn!("[ESP] TIME Connection ended with error: {e}");
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

pub async fn esp_keepalive() -> anyhow::Result<()> {
    log::info!("[ESP] KEEPALIVE Binding controller...");
    let tcp_listener = TcpListener::bind("192.168.178.30:3126").await?;
    log::info!("[ESP] KEEPALIVE Controller bound!");

    loop {
        let (stream, _) = tcp_listener.accept().await?;
        log::info!("[ESP] KEEPALIVE Client connected!");
        if let Err(e) = handle_esp_keepalive(stream).await {
            log::warn!("[ESP] KEEPALIVE Connection ended with error: {e}");
        }
    }
}

async fn handle_esp_keepalive(mut tcp_stream: TcpStream) -> anyhow::Result<()> {
    tcp_stream.set_nodelay(true)?;
    let mut buf = [0_u8; 256];
    let (tx, rx) = mpsc::channel::<()>(8);
    tokio::task::spawn(restart_process_on_timeout(rx));
    let mut counter = 0;

    loop {
        tcp_stream.read_exact(&mut buf).await?;
        if buf.iter().any(|b| *b != 0b01010110) {
            restart();
        }
        tx.send(()).await?;

        for b in &mut buf {
            *b = 0;
        }
        log::info!("[ESP] KEEPALIVE counter={counter}");
        counter += 1;
    }
}

async fn restart_process_on_timeout(mut rx: mpsc::Receiver<()>) {
    loop {
        if tokio::time::timeout(Duration::from_secs(15), rx.recv())
            .await
            .is_err()
        {
            log::error!("[ESP] KEEPALIVE Did not receive value within 15 s, restarting");
            restart();
        }
    }
}

fn restart() {
    let err = Command::new("/proc/self/exe").exec();
    log::error!("Failed to exec: {:?}", err);

    std::process::exit(1);
}
