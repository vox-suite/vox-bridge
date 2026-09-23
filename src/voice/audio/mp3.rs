/**
* this file code contains mp3 decoding and mulaw transcoding
*/
use bytes::Bytes;
use futures_util::stream::{Stream, StreamExt};
use std::pin::Pin;
use tokio_util::io::StreamReader;

use crate::voice::provider::VoiceError;

#[inline]
pub fn linear_to_mulaw(pcm_val: i16) -> u8 {
    const BIAS: i16 = 0x84;
    const CLIP: i16 = 32635;

    let (sign, mut sample) = if pcm_val < 0 {
        (0x80, (-pcm_val).min(CLIP))
    } else {
        (0x00, pcm_val.min(CLIP))
    };

    sample += BIAS;
    let mut exponent = 7;
    let mut mask = 0x4000;
    while (sample & mask) == 0 && exponent > 0 {
        exponent -= 1;
        mask >>= 1;
    }
    let mantissa = (sample >> (exponent + 3)) & 0x0F;
    let mulaw = (sign | (exponent << 4) | mantissa) as u8;
    !mulaw
}

#[inline]
pub fn mulaw_to_linear(u_val: u8) -> i16 {
    let u_val = !u_val;
    let sign = (u_val & 0x80) != 0;
    let exponent = (u_val >> 4) & 0x07;
    let mantissa = (u_val & 0x0F) as i16;
    let mut sample = ((mantissa << 3) + 0x84) << exponent;
    sample -= 0x84;
    if sign { -sample } else { sample }
}

struct CoalescingReader<R> {
    inner: R,
}

impl<R: tokio::io::AsyncRead + Unpin> tokio::io::AsyncRead for CoalescingReader<R> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let initial_len = buf.filled().len();
        loop {
            let before = buf.filled().len();
            match Pin::new(&mut self.inner).poll_read(cx, buf) {
                std::task::Poll::Ready(Ok(())) => {
                    let after = buf.filled().len();
                    if after == before {
                        return std::task::Poll::Ready(Ok(()));
                    }
                    if after - initial_len >= 2048 || buf.remaining() == 0 {
                        return std::task::Poll::Ready(Ok(()));
                    }
                }
                std::task::Poll::Ready(Err(e)) => return std::task::Poll::Ready(Err(e)),
                std::task::Poll::Pending => {
                    if buf.filled().len() > initial_len {
                        return std::task::Poll::Ready(Ok(()));
                    } else {
                        return std::task::Poll::Pending;
                    }
                }
            }
        }
    }
}

#[derive(Default)]
struct Resampler {
    resample_phase: f64,
    last_sample: Option<i16>,
}

impl Resampler {
    fn resample_and_encode(&mut self, samples: &[i16], sample_rate: f64) -> Vec<u8> {
        if samples.is_empty() {
            return Vec::new();
        }

        let step = sample_rate / 8000.0;
        let n = samples.len();
        let mut out = Vec::with_capacity(((n as f64 / step) + 1.0) as usize);

        while self.resample_phase < n as f64 {
            let idx = self.resample_phase.floor() as usize;
            let frac = self.resample_phase - idx as f64;

            let s0 = if idx == 0 {
                self.last_sample.unwrap_or(samples[0])
            } else {
                samples[idx - 1]
            };
            let s1 = samples[idx];

            let interp = (s0 as f64 * (1.0 - frac) + s1 as f64 * frac).round() as i16;
            out.push(linear_to_mulaw(interp));
            self.resample_phase += step;
        }

        self.resample_phase -= n as f64;
        self.last_sample = samples.last().copied();
        out
    }
}

pub fn transcode_mp3_to_mulaw_stream<S>(
    input: S,
) -> Pin<Box<dyn Stream<Item = Result<Bytes, VoiceError>> + Send>>
where
    S: Stream<Item = Result<Bytes, VoiceError>> + Send + 'static,
{
    let io_stream =
        Box::pin(input.map(|res| res.map_err(|e| std::io::Error::other(e.to_string()))));
    let reader = CoalescingReader {
        inner: StreamReader::new(io_stream),
    };
    let mut decoder = minimp3::Decoder::new(reader);
    let mut resampler = Resampler::default();

    let (tx, mut rx) = tokio::sync::mpsc::channel(16);

    tokio::spawn(async move {
        loop {
            match decoder.next_frame_future().await {
                Ok(frame) => {
                    let channels = frame.channels.max(1);
                    let sample_rate = frame.sample_rate.max(8000) as f64;
                    let samples_per_channel = frame.data.len() / channels;

                    let mut mono = Vec::with_capacity(samples_per_channel);
                    if channels == 1 {
                        mono.extend_from_slice(&frame.data);
                    } else {
                        for i in 0..samples_per_channel {
                            let mut sum: i32 = 0;
                            for c in 0..channels {
                                sum += frame.data[i * channels + c] as i32;
                            }
                            mono.push((sum / channels as i32) as i16);
                        }
                    }

                    let mulaw_bytes = resampler.resample_and_encode(&mono, sample_rate);
                    if !mulaw_bytes.is_empty()
                        && tx.send(Ok(Bytes::from(mulaw_bytes))).await.is_err()
                    {
                        break;
                    }
                }
                Err(minimp3::Error::Eof) => break,
                Err(minimp3::Error::SkippedData) => continue,
                Err(minimp3::Error::InsufficientData) => continue,
                Err(minimp3::Error::Io(err)) if err.kind() == std::io::ErrorKind::UnexpectedEof => {
                    break;
                }
                Err(err) => {
                    let _ = tx
                        .send(Err(VoiceError::Provider {
                            provider: "elevenlabs",
                            message: format!("MP3 decoding failed: {err}"),
                        }))
                        .await;
                    break;
                }
            }
        }
    });

    Box::pin(futures_util::stream::poll_fn(move |cx| rx.poll_recv(cx)))
}
