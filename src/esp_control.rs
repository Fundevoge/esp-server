#![allow(dead_code)]
use std::{os::unix::process::CommandExt, process::Command, time::Duration};

use anyhow::Context;
use byteorder::ByteOrder as _;
use chrono::{DateTime, Utc};
use once_cell::sync::Lazy;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{
        tcp::{OwnedReadHalf, OwnedWriteHalf},
        TcpListener, TcpStream,
    },
    sync::{mpsc, Mutex},
    time::sleep,
    try_join,
};

use crate::{AsyncIterator, DisplayFrame, PLAYBACK_STREAM};

const PACKET_FILLER: u8 = 0b01010110;
const SEND_PACKET_SIZE: usize = 64;

enum OutgoingPacketType {
    TimeSync,
    TimeFollowUp,
    TimeDelayResp,
    StateChange,
}

impl From<OutgoingPacketType> for u8 {
    fn from(val: OutgoingPacketType) -> Self {
        match val {
            OutgoingPacketType::TimeSync => 0,
            OutgoingPacketType::TimeFollowUp => 1,
            OutgoingPacketType::TimeDelayResp => 2,
            OutgoingPacketType::StateChange => 3,
        }
    }
}

enum IncomingPacketType {
    Keepalive,
    TimeDelayReq,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum State {
    Stream,
    Clock(ClockType),
    Off,
}

impl Default for State {
    fn default() -> Self {
        State::Clock(ClockType::Large)
    }
}

impl From<State> for u8 {
    fn from(val: State) -> Self {
        match val {
            State::Stream => 1,
            State::Clock(clock_type) => 2_u8 + u8::from(clock_type),
            State::Off => 0,
        }
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum ClockType {
    Small,
    Large,
}

impl From<ClockType> for u8 {
    fn from(val: ClockType) -> Self {
        match val {
            ClockType::Small => 0,
            ClockType::Large => 1,
        }
    }
}

impl TryFrom<u8> for IncomingPacketType {
    type Error = ();

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(IncomingPacketType::Keepalive),
            1 => Ok(IncomingPacketType::TimeDelayReq),
            _ => Err(()),
        }
    }
}

pub async fn start_stream(
    stream: Box<dyn AsyncIterator<Item = DisplayFrame> + Send>,
) -> anyhow::Result<()> {
    let mut playback_stream = PLAYBACK_STREAM.lock().await;
    *playback_stream = Some(stream);
    drop(playback_stream);

    let mut packet = [PACKET_FILLER; SEND_PACKET_SIZE];
    packet[0] = OutgoingPacketType::StateChange.into();
    packet[1] = State::Stream.into();

    ESP_PACKET_CHANNEL.0.send(packet).await?;
    Ok(())
}

pub async fn esp_stream_controller() -> anyhow::Result<()> {
    log::info!("[ESP] STREAM Binding controller...");
    let tcp_listener = loop {
        let tcp_listener = TcpListener::bind("192.168.178.30:3123").await;
        if let Ok(tcp_listener) = tcp_listener {
            break tcp_listener;
        }
        tokio::time::sleep(Duration::from_secs(15)).await;
    };
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

pub async fn esp_main_controller() -> anyhow::Result<()> {
    log::info!("[ESP] MAIN Binding controller...");
    let tcp_listener = loop {
        let tcp_listener = TcpListener::bind("192.168.178.30:3124").await;
        if let Ok(tcp_listener) = tcp_listener {
            break tcp_listener;
        }
        tokio::time::sleep(Duration::from_secs(15)).await;
    };
    log::info!("[ESP] MAIN Controller bound!");

    loop {
        let (stream, _) = tcp_listener.accept().await?;
        log::info!("[ESP] MAIN Client connected!");
        if let Err(e) = handle_esp_main_connection(stream).await {
            log::warn!("[ESP] MAIN Connection ended with error: {e}");
        }
    }
}

async fn handle_esp_main_connection(tcp_stream: TcpStream) -> anyhow::Result<()> {
    tcp_stream.set_nodelay(true)?;
    let (rx, tx) = tcp_stream.into_split();
    let time_handle = tokio::spawn(time_control());
    let rx_handle = tokio::spawn(esp_main_receiver(rx));
    let tx_handle = tokio::spawn(esp_main_sender(tx));

    let (rx_err, tx_err, time_err) = try_join!(rx_handle, tx_handle, time_handle)?;
    rx_err?;
    tx_err?;
    time_err
}

async fn esp_main_receiver(mut socket_rx: OwnedReadHalf) -> anyhow::Result<()> {
    let mut buf = [0_u8; 256];
    let (tx, rx) = mpsc::channel::<()>(8);
    tokio::task::spawn(restart_process_on_timeout(rx));

    loop {
        socket_rx.read_exact(&mut buf).await?;
        let Ok(packet_type) = IncomingPacketType::try_from(buf[0]) else {
            log::warn!("[ESP] Invalid incoming packet");
            continue;
        };
        tx.send(()).await?;
        if let IncomingPacketType::TimeDelayReq = packet_type {
            ESP_TIME_CHANNEL.0.send(()).await?;
        }
    }
}

async fn esp_main_sender(mut tx: OwnedWriteHalf) -> anyhow::Result<()> {
    let mut packet_receiver = ESP_PACKET_CHANNEL.1.lock().await;
    // log::info!("[ESP] Ready to send packets");
    loop {
        let packet = packet_receiver.recv().await.context("Receiving failed")?;
        tx.write_all(&packet).await?;
        tx.flush().await?;
        // log::info!("[ESP] Sent packet");
    }
}

fn write_naive_datetime(transmit_buffer: &mut [u8], date_time: DateTime<Utc>) {
    byteorder::LE::write_i64(&mut transmit_buffer[0..8], date_time.timestamp());
    byteorder::LE::write_u32(
        &mut transmit_buffer[8..12],
        date_time.timestamp_subsec_nanos(),
    );
}

async fn time_control() -> anyhow::Result<()> {
    let mut time_request_receiver = ESP_TIME_CHANNEL.1.lock().await;

    loop {
        let mut packet = [PACKET_FILLER; SEND_PACKET_SIZE];
        packet[0] = OutgoingPacketType::TimeSync.into();
        let sent_sync = Utc::now();
        ESP_PACKET_CHANNEL.0.send(packet).await?;
        sleep(Duration::from_millis(100)).await;

        let mut packet = [PACKET_FILLER; SEND_PACKET_SIZE];
        packet[0] = OutgoingPacketType::TimeFollowUp.into();
        write_naive_datetime(&mut packet[1..], sent_sync);
        ESP_PACKET_CHANNEL.0.send(packet).await?;

        time_request_receiver.recv().await;
        let received_delay_request = Utc::now();

        let mut packet = [PACKET_FILLER; SEND_PACKET_SIZE];
        packet[0] = OutgoingPacketType::TimeDelayResp.into();
        write_naive_datetime(&mut packet[1..], received_delay_request);
        ESP_PACKET_CHANNEL.0.send(packet).await?;

        sleep(Duration::from_secs(60)).await;
    }
}

type P = [u8; SEND_PACKET_SIZE];
static ESP_PACKET_CHANNEL: Lazy<(mpsc::Sender<P>, Mutex<mpsc::Receiver<P>>)> = Lazy::new(|| {
    let (esp_control_sender, esp_control_receiver) = mpsc::channel(256);
    (esp_control_sender, Mutex::new(esp_control_receiver))
});

static ESP_TIME_CHANNEL: Lazy<(mpsc::Sender<()>, Mutex<mpsc::Receiver<()>>)> = Lazy::new(|| {
    let (esp_control_sender, esp_control_receiver) = mpsc::channel(256);
    (esp_control_sender, Mutex::new(esp_control_receiver))
});

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
