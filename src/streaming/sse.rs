//! SSE stream forwarder for protocol-specific streaming.

use axum::response::sse::Event;
use futures::stream::Stream;
use tokio_stream::wrappers::ReceiverStream;

use crate::protocol::{anthropic, openai, Protocol};
use crate::streaming::{LLMChunk, LLMStream};

/// Forward an LLMStream to an SSE stream for a given protocol.
pub async fn forward_sse_stream(
    protocol: Protocol,
    llm_stream: LLMStream,
    id: &str,
) -> impl Stream<Item = Result<Event, axum::Error>> {
    let (tx, rx) = tokio::sync::mpsc::channel(100);

    tokio::spawn(async move {
        let mut index = 0usize;
        let mut stream = llm_stream;

        while let Some(chunk_result) = stream.next().await {
            match chunk_result {
                Ok(chunk) => {
                    let sse_str = match protocol {
                        Protocol::OpenAI => openai::serialize_stream_chunk(&chunk, id, "mock", &index),
                        Protocol::Anthropic => anthropic::serialize_stream_event(&chunk, id, "mock", &index, true, "end_turn"),
                    };
                    index += 1;

                    if tx.send(Ok(Event::default().data(sse_str))).await.is_err() {
                        break; // Receiver dropped
                    }
                }
                Err(e) => {
                    if tx.send(Err(axum::Error::new(std::io::Error::new(
                        std::io::ErrorKind::Other,
                        e.to_string(),
                    )))).await.is_err() {
                        break;
                    }
                }
            }
        }
    });

    ReceiverStream::new(rx).map(|result| result.map_err(|e| axum::Error::new(e)))
}
