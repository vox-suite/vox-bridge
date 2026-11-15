use std::{io, time::Duration};

use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::{net::TcpStream, time::timeout};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async,
    tungstenite::{Message, client::IntoClientRequest},
};

pub type AssemblySocket = WebSocketStream<MaybeTlsStream<TcpStream>>;

pub async fn connect(
    api_key: &str,
) -> Result<AssemblySocket, Box<dyn std::error::Error + Send + Sync>> {
    let url = "wss://streaming.assemblyai.com/v3/ws\
            ?speech_model=universal-3-6-pro\
            &sample_rate=8000\
            &encoding=pcm_mulaw";

    let mut request = url.into_client_request()?;

    request
        .headers_mut()
        .insert("Authorization", api_key.parse()?);

    let (mut socket, _response) =
        timeout(Duration::from_secs(10), connect_async(request)).await??;

    timeout(Duration::from_secs(10), async {
        loop {
            let message = socket.next().await.ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "AssemblyAI disconnected before Begin",
                )
            })??;

            match message {
                Message::Text(text) => {
                    let event: Value = serde_json::from_str(&text)?;
                    if event["type"].as_str() != Some("Begin") {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "Expected AssemblyAI Begin message",
                        )
                        .into());
                    }

                    return Ok::<(), Box<dyn std::error::Error + Send + Sync>>(());
                }
                Message::Close(_) => {
                    return Err(io::Error::new(
                        io::ErrorKind::ConnectionAborted,
                        "AssemblyAI closed before Begin",
                    )
                    .into());
                }
                _ => {}
            }
        }
    })
    .await??;

    Ok(socket)
}

pub async fn send_audio(
    socket: &mut AssemblySocket,
    buffer: &mut Vec<u8>,
    audio_bytes: &[u8],
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // 100 ms of 8 kHz mono mu-law audio, at one byte per sample.
    const CHUNK_BYTES: usize = 800;

    buffer.extend_from_slice(audio_bytes);

    while buffer.len() >= CHUNK_BYTES {
        let chunk: Vec<u8> = buffer.drain(..CHUNK_BYTES).collect();
        timeout(
            Duration::from_secs(5),
            socket.send(Message::Binary(chunk.into())),
        )
        .await??;
    }

    Ok(())
}

pub async fn receive_event(
    socket: &mut AssemblySocket,
) -> Result<Value, Box<dyn std::error::Error + Send + Sync>> {
    loop {
        let message = socket.next().await.ok_or_else(|| {
            io::Error::new(io::ErrorKind::UnexpectedEof, "AssemblyAI disconnected")
        })??;

        match message {
            Message::Text(text) => {
                let event: Value = serde_json::from_str(&text)?;
                if event["type"].as_str() == Some("Turn") {
                    println!(
                        "AssemblyAI turn {} (final={}): {}",
                        event["turn_order"], event["end_of_turn"], event["transcript"]
                    );
                }
                return Ok(event);
            }
            Message::Close(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::ConnectionAborted,
                    "AssemblyAI closed the connection",
                )
                .into());
            }
            _ => {}
        }
    }
}

pub async fn finish(
    socket: &mut AssemblySocket,
    buffer: &mut Vec<u8>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    timeout(Duration::from_secs(10), async {
        if !buffer.is_empty() {
            // Pad a short tail to 50 ms with mu-law silence (0xff).
            buffer.resize(buffer.len().max(400), 0xff);
            let tail = std::mem::take(buffer);
            socket.send(Message::Binary(tail.into())).await?;
        }

        socket
            .send(Message::Text(r#"{"type":"Terminate"}"#.into()))
            .await?;

        while receive_event(socket).await?["type"].as_str() != Some("Termination") {}

        let _ = socket.close(None).await;
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    })
    .await?
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;
    use tokio_tungstenite::tungstenite::protocol::Role;

    async fn socket_pair() -> (AssemblySocket, WebSocketStream<TcpStream>) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let (client, server) = tokio::join!(TcpStream::connect(address), listener.accept());
        let (server, _) = server.unwrap();

        let client = WebSocketStream::from_raw_socket(
            MaybeTlsStream::Plain(client.unwrap()),
            Role::Client,
            None,
        )
        .await;
        let server = WebSocketStream::from_raw_socket(server, Role::Server, None).await;
        (client, server)
    }

    #[tokio::test]
    async fn sends_complete_audio_chunks_in_order_and_keeps_the_remainder() {
        let (mut client, mut server) = socket_pair().await;
        let mut buffer = Vec::new();
        let audio: Vec<u8> = (0..2160).map(|index| (index % 256) as u8).collect();

        send_audio(&mut client, &mut buffer, &audio[..160])
            .await
            .unwrap();
        assert_eq!(buffer, audio[..160]);

        send_audio(&mut client, &mut buffer, &audio[160..])
            .await
            .unwrap();

        for expected in audio[..1600].as_chunks::<800>().0 {
            let message = timeout(Duration::from_secs(1), server.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            assert_eq!(message, Message::Binary(expected.to_vec().into()));
        }
        assert_eq!(buffer, audio[1600..]);
    }

    #[tokio::test]
    async fn finish_flushes_short_audio_tail_and_waits_for_termination() {
        let (mut client, mut server) = socket_pair().await;
        let server_task = tokio::spawn(async move {
            let Message::Binary(audio) = server.next().await.unwrap().unwrap() else {
                panic!("Expected final audio before Terminate");
            };
            assert_eq!(&audio[..160], &[0x12; 160]);
            assert_eq!(&audio[160..], &[0xff; 240]);

            let Message::Text(text) = server.next().await.unwrap().unwrap() else {
                panic!("Expected Terminate after final audio");
            };
            assert_eq!(
                serde_json::from_str::<Value>(&text).unwrap()["type"],
                "Terminate"
            );

            server
                .send(Message::Text(
                    r#"{"type":"Turn","turn_order":0,"end_of_turn":true,"transcript":"Hello"}"#
                        .into(),
                ))
                .await
                .unwrap();
            server
                .send(Message::Text(r#"{"type":"Termination"}"#.into()))
                .await
                .unwrap();
            assert!(matches!(server.next().await, Some(Ok(Message::Close(_)))));
        });

        let mut buffer = vec![0x12; 160];
        finish(&mut client, &mut buffer).await.unwrap();
        assert!(buffer.is_empty());
        server_task.await.unwrap();
    }

    #[tokio::test]
    async fn receive_event_reports_unexpected_socket_close() {
        let (mut client, mut server) = socket_pair().await;
        server.close(None).await.unwrap();

        let error = receive_event(&mut client).await.unwrap_err();
        assert_eq!(
            error.downcast_ref::<io::Error>().unwrap().kind(),
            io::ErrorKind::ConnectionAborted,
        );
    }
}
