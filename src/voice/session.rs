use crate::voice::{
    context::CallContext,
    provider::{SttEvent, VoiceError},
    registry::ProviderSet,
};
use bytes::Bytes;
use futures_util::StreamExt;
use std::{collections::VecDeque, sync::Arc, time::Duration};
use tokio::{sync::mpsc, task::JoinHandle};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallEvent {
    Audio(Bytes),
    Stop,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallCommand {
    Media(Bytes),
    Mark(String),
    Clear,
}

enum SessionSignal {
    Stt(SttEvent),
    Stop,
    Failure(VoiceError),
    ResponseFinished(u64),
}

pub async fn run_voice_session(
    providers: ProviderSet,
    context: CallContext,
    mut input: mpsc::Receiver<CallEvent>,
    output: mpsc::Sender<CallCommand>,
) -> Result<(), VoiceError> {
    let stt = providers.stt.connect().await?;
    let (signal_tx, mut signal_rx) = mpsc::channel(32);
    let input_stt = stt.clone();
    let input_signal = signal_tx.clone();
    let input_task = tokio::spawn(async move {
        while let Some(event) = input.recv().await {
            match event {
                CallEvent::Audio(audio) => {
                    if let Err(error) = input_stt.send_audio(audio).await {
                        let _ = input_signal.send(SessionSignal::Failure(error)).await;
                        return;
                    }
                }
                CallEvent::Stop => {
                    let _ = input_signal.send(SessionSignal::Stop).await;
                    return;
                }
            }
        }
        let _ = input_signal.send(SessionSignal::Stop).await;
    });
    let event_stt = stt.clone();
    let event_signal = signal_tx.clone();
    let stt_task = tokio::spawn(async move {
        loop {
            match event_stt.next_event().await {
                Ok(Some(event)) => {
                    if event_signal.send(SessionSignal::Stt(event)).await.is_err() {
                        return;
                    }
                }
                Ok(None) => {
                    let _ = event_signal.send(SessionSignal::Stop).await;
                    return;
                }
                Err(error) => {
                    let _ = event_signal.send(SessionSignal::Failure(error)).await;
                    return;
                }
            }
        }
    });
    let mut active_response: Option<JoinHandle<()>> = None;
    let mut response_number = 0_u64;
    let mut pending_transcripts = VecDeque::new();
    let mut failure = None;

    while let Some(signal) = signal_rx.recv().await {
        match signal {
            SessionSignal::Stt(SttEvent::SpeechStarted) => {
                if let Some(task) = active_response.take() {
                    task.abort();
                    output
                        .send(CallCommand::Clear)
                        .await
                        .map_err(|_| VoiceError::Protocol("call output closed".into()))?;
                }
                if let Some(transcript) = pending_transcripts.pop_front() {
                    response_number += 1;
                    active_response = Some(spawn_response(
                        response_number,
                        context.clone(),
                        transcript,
                        providers.agent.clone(),
                        providers.tts.clone(),
                        output.clone(),
                        signal_tx.clone(),
                    ));
                }
            }
            SessionSignal::Stt(SttEvent::FinalTranscript(transcript)) => {
                if active_response.is_some() {
                    pending_transcripts.push_back(transcript);
                } else {
                    response_number += 1;
                    active_response = Some(spawn_response(
                        response_number,
                        context.clone(),
                        transcript,
                        providers.agent.clone(),
                        providers.tts.clone(),
                        output.clone(),
                        signal_tx.clone(),
                    ));
                }
            }
            SessionSignal::ResponseFinished(number) => {
                if number == response_number {
                    active_response.take();
                    if let Some(transcript) = pending_transcripts.pop_front() {
                        response_number += 1;
                        active_response = Some(spawn_response(
                            response_number,
                            context.clone(),
                            transcript,
                            providers.agent.clone(),
                            providers.tts.clone(),
                            output.clone(),
                            signal_tx.clone(),
                        ));
                    }
                }
            }
            SessionSignal::Stop => break,
            SessionSignal::Failure(error) => {
                failure = Some(error);
                break;
            }
        }
    }

    if let Some(task) = active_response {
        task.abort();
    }
    stop_task(input_task).await;
    stop_task(stt_task).await;
    let finish_result = tokio::time::timeout(Duration::from_secs(3), stt.finish())
        .await
        .map_err(|_| VoiceError::Timeout("AssemblyAI termination"))?;
    if let Some(error) = failure {
        return Err(error);
    }
    finish_result
}

fn spawn_response(
    number: u64,
    context: CallContext,
    transcript: String,
    agent: Arc<dyn crate::voice::provider::AgentProvider>,
    tts: Arc<dyn crate::voice::provider::TtsProvider>,
    output: mpsc::Sender<CallCommand>,
    signal: mpsc::Sender<SessionSignal>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let result = stream_response(number, &context, &transcript, agent, tts, output).await;
        if let Err(error) = result {
            tracing::warn!(provider_error = %error, "voice response failed");
        }
        let _ = signal.send(SessionSignal::ResponseFinished(number)).await;
    })
}

async fn stream_response(
    number: u64,
    context: &CallContext,
    transcript: &str,
    agent: Arc<dyn crate::voice::provider::AgentProvider>,
    tts: Arc<dyn crate::voice::provider::TtsProvider>,
    output: mpsc::Sender<CallCommand>,
) -> Result<(), VoiceError> {
    let response =
        tokio::time::timeout(Duration::from_secs(30), agent.respond(context, transcript))
            .await
            .map_err(|_| VoiceError::Timeout("agent response"))??;
    let mut audio = tts.synthesize(&response).await?;
    while let Some(chunk) = tokio::time::timeout(Duration::from_secs(10), audio.next())
        .await
        .map_err(|_| VoiceError::Timeout("TTS audio"))?
    {
        output
            .send(CallCommand::Media(chunk?))
            .await
            .map_err(|_| VoiceError::Protocol("call output closed".into()))?;
    }
    output
        .send(CallCommand::Mark(format!("response-{number}")))
        .await
        .map_err(|_| VoiceError::Protocol("call output closed".into()))
}

async fn stop_task(task: JoinHandle<()>) {
    task.abort();
    let _ = task.await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::voice::{
        provider::{AgentProvider, AudioStream, SttProvider, SttSession, TtsProvider},
        registry::ProviderSet,
    };
    use async_trait::async_trait;
    use futures_util::stream;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    use tokio::sync::Notify;
    use tokio::sync::{Mutex, mpsc};

    struct FakeSttProvider {
        session: Arc<FakeSttSession>,
    }

    struct FakeSttSession {
        audio: Mutex<Vec<Bytes>>,
        events: Mutex<mpsc::Receiver<SttEvent>>,
        finished: AtomicBool,
    }

    struct FakeAgent {
        transcripts: Mutex<Vec<String>>,
        response: String,
        first_gate: Mutex<Option<Arc<Notify>>>,
    }

    struct FakeTts {
        texts: Mutex<Vec<String>>,
        chunks: Vec<Bytes>,
        pending: AtomicBool,
        fail: AtomicBool,
    }

    #[async_trait]
    impl SttProvider for FakeSttProvider {
        async fn connect(&self) -> Result<Arc<dyn SttSession>, VoiceError> {
            Ok(self.session.clone())
        }
    }

    #[async_trait]
    impl SttSession for FakeSttSession {
        async fn send_audio(&self, audio: Bytes) -> Result<(), VoiceError> {
            self.audio.lock().await.push(audio);
            Ok(())
        }

        async fn next_event(&self) -> Result<Option<SttEvent>, VoiceError> {
            Ok(self.events.lock().await.recv().await)
        }

        async fn finish(&self) -> Result<(), VoiceError> {
            self.finished.store(true, Ordering::SeqCst);
            Ok(())
        }
    }

    #[async_trait]
    impl AgentProvider for FakeAgent {
        async fn respond(
            &self,
            _context: &CallContext,
            transcript: &str,
        ) -> Result<String, VoiceError> {
            self.transcripts.lock().await.push(transcript.into());
            if transcript == "first"
                && let Some(gate) = self.first_gate.lock().await.clone()
            {
                gate.notified().await;
            }
            Ok(self.response.clone())
        }
    }

    fn call_context() -> CallContext {
        CallContext {
            channel: "phone".into(),
            external_identity: "+14155550100".into(),
            external_conversation_id: "CA123".into(),
            initiation_context: None,
        }
    }

    #[async_trait]
    impl TtsProvider for FakeTts {
        async fn synthesize(&self, text: &str) -> Result<AudioStream, VoiceError> {
            self.texts.lock().await.push(text.into());
            if self.fail.load(Ordering::SeqCst) {
                return Err(VoiceError::Provider {
                    provider: "fake-tts",
                    message: "failed".into(),
                });
            }
            let chunks = stream::iter(self.chunks.clone().into_iter().map(Ok));
            if self.pending.load(Ordering::SeqCst) {
                Ok(Box::pin(chunks.chain(stream::pending())))
            } else {
                Ok(Box::pin(chunks))
            }
        }
    }

    fn providers() -> (
        ProviderSet,
        mpsc::Sender<SttEvent>,
        Arc<FakeSttSession>,
        Arc<FakeAgent>,
        Arc<FakeTts>,
    ) {
        let (event_tx, event_rx) = mpsc::channel(8);
        let stt_session = Arc::new(FakeSttSession {
            audio: Mutex::new(Vec::new()),
            events: Mutex::new(event_rx),
            finished: AtomicBool::new(false),
        });
        let agent = Arc::new(FakeAgent {
            transcripts: Mutex::new(Vec::new()),
            response: "Hi there".into(),
            first_gate: Mutex::new(None),
        });
        let tts = Arc::new(FakeTts {
            texts: Mutex::new(Vec::new()),
            chunks: vec![Bytes::from_static(&[1, 2]), Bytes::from_static(&[3, 4])],
            pending: AtomicBool::new(false),
            fail: AtomicBool::new(false),
        });
        (
            ProviderSet {
                stt: Arc::new(FakeSttProvider {
                    session: stt_session.clone(),
                }),
                agent: agent.clone(),
                tts: tts.clone(),
            },
            event_tx,
            stt_session,
            agent,
            tts,
        )
    }

    #[tokio::test]
    async fn turns_call_audio_into_marked_response_audio() {
        let (providers, event_tx, stt, agent, tts) = providers();
        let (input_tx, input_rx) = mpsc::channel(8);
        let (output_tx, mut output_rx) = mpsc::channel(8);
        let session = tokio::spawn(run_voice_session(
            providers,
            call_context(),
            input_rx,
            output_tx,
        ));

        input_tx
            .send(CallEvent::Audio(Bytes::from_static(&[0xff, 0x7f])))
            .await
            .unwrap();
        event_tx
            .send(SttEvent::FinalTranscript("hello".into()))
            .await
            .unwrap();

        let mut commands = Vec::new();
        for _ in 0..3 {
            commands.push(output_rx.recv().await.unwrap());
        }
        input_tx.send(CallEvent::Stop).await.unwrap();
        session.await.unwrap().unwrap();

        assert_eq!(
            stt.audio.lock().await.as_slice(),
            &[Bytes::from_static(&[0xff, 0x7f])]
        );
        assert_eq!(agent.transcripts.lock().await.as_slice(), &["hello"]);
        assert_eq!(tts.texts.lock().await.as_slice(), &["Hi there"]);
        assert_eq!(
            commands,
            vec![
                CallCommand::Media(Bytes::from_static(&[1, 2])),
                CallCommand::Media(Bytes::from_static(&[3, 4])),
                CallCommand::Mark("response-1".into()),
            ]
        );
        assert!(stt.finished.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn speech_started_cancels_playback_and_clears_twilio() {
        let (providers, event_tx, _, agent, tts) = providers();
        tts.pending.store(true, Ordering::SeqCst);
        let (input_tx, input_rx) = mpsc::channel(8);
        let (output_tx, mut output_rx) = mpsc::channel(8);
        let session = tokio::spawn(run_voice_session(
            providers,
            call_context(),
            input_rx,
            output_tx,
        ));
        event_tx
            .send(SttEvent::FinalTranscript("first".into()))
            .await
            .unwrap();
        assert_eq!(
            output_rx.recv().await,
            Some(CallCommand::Media(Bytes::from_static(&[1, 2])))
        );
        assert_eq!(
            output_rx.recv().await,
            Some(CallCommand::Media(Bytes::from_static(&[3, 4])))
        );

        event_tx.send(SttEvent::SpeechStarted).await.unwrap();
        let clear = tokio::time::timeout(std::time::Duration::from_millis(100), output_rx.recv())
            .await
            .unwrap();
        assert_eq!(clear, Some(CallCommand::Clear));

        tts.pending.store(false, Ordering::SeqCst);
        event_tx
            .send(SttEvent::FinalTranscript("second".into()))
            .await
            .unwrap();
        for _ in 0..3 {
            output_rx.recv().await.unwrap();
        }
        input_tx.send(CallEvent::Stop).await.unwrap();
        session.await.unwrap().unwrap();
        assert_eq!(
            agent.transcripts.lock().await.as_slice(),
            &["first", "second"]
        );
    }

    #[tokio::test]
    async fn finalized_turns_are_processed_serially() {
        let (providers, event_tx, _, agent, _) = providers();
        let gate = Arc::new(Notify::new());
        *agent.first_gate.lock().await = Some(gate.clone());
        let (input_tx, input_rx) = mpsc::channel(8);
        let (output_tx, mut output_rx) = mpsc::channel(16);
        let session = tokio::spawn(run_voice_session(
            providers,
            call_context(),
            input_rx,
            output_tx,
        ));

        event_tx
            .send(SttEvent::FinalTranscript("first".into()))
            .await
            .unwrap();
        while agent.transcripts.lock().await.is_empty() {
            tokio::task::yield_now().await;
        }
        event_tx
            .send(SttEvent::FinalTranscript("second".into()))
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert_eq!(agent.transcripts.lock().await.as_slice(), &["first"]);

        gate.notify_one();
        for _ in 0..6 {
            output_rx.recv().await.unwrap();
        }
        input_tx.send(CallEvent::Stop).await.unwrap();
        session.await.unwrap().unwrap();
        assert_eq!(
            agent.transcripts.lock().await.as_slice(),
            &["first", "second"]
        );
    }

    #[tokio::test]
    async fn a_failed_response_does_not_leak_or_end_the_call() {
        let (providers, event_tx, stt, _, tts) = providers();
        tts.fail.store(true, Ordering::SeqCst);
        let (input_tx, input_rx) = mpsc::channel(8);
        let (output_tx, mut output_rx) = mpsc::channel(8);
        let session = tokio::spawn(run_voice_session(
            providers,
            call_context(),
            input_rx,
            output_tx,
        ));
        event_tx
            .send(SttEvent::FinalTranscript("first".into()))
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        assert!(output_rx.try_recv().is_err());

        tts.fail.store(false, Ordering::SeqCst);
        event_tx
            .send(SttEvent::FinalTranscript("second".into()))
            .await
            .unwrap();
        for _ in 0..3 {
            output_rx.recv().await.unwrap();
        }
        input_tx.send(CallEvent::Stop).await.unwrap();
        session.await.unwrap().unwrap();
        assert!(stt.finished.load(Ordering::SeqCst));
    }
}
