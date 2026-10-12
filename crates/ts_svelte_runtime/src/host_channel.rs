//! Synchronous policy callbacks for a native promise-graph operation. Reading a
//! reply happens on the same thread as the callback, before the engine resumes
//! jobs. The channel grants no filesystem, module, or network capabilities.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::{BufRead, Read, Write};

const MAX_EVENT_BYTES: usize = 8 * 1024 * 1024;
const MAX_EVENTS: u64 = 100_000;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Envelope<'a> {
    host_event: Event<'a>,
}
#[derive(Serialize)]
struct Event<'a> {
    sequence: u64,
    event: &'a Value,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReplyEnvelope {
    host_reply: Reply,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Reply {
    sequence: u64,
    outcome: Outcome,
}
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
enum Outcome {
    Return { value: Value },
    Throw { message: String },
}

/// One instance belongs to one outer operation. A protocol/host failure is
/// terminal; reuse would risk interpreting a stale callback reply as a request.
pub struct HostChannel<R, W> {
    input: R,
    output: W,
    sequence: u64,
    failed: bool,
}
impl<R: BufRead, W: Write> HostChannel<R, W> {
    pub fn new(input: R, output: W) -> Self {
        Self {
            input,
            output,
            sequence: 0,
            failed: false,
        }
    }
    pub fn exchange(&mut self, event: &Value) -> Result<Value, String> {
        if self.failed {
            return Err("Native policy channel is failed".into());
        }
        let result = self.exchange_inner(event);
        if result.is_err() {
            self.failed = true;
        }
        result
    }
    fn exchange_inner(&mut self, event: &Value) -> Result<Value, String> {
        if self.sequence >= MAX_EVENTS {
            return Err("Native policy event limit exceeded".into());
        }
        self.sequence += 1;
        let bytes = serde_json::to_vec(&Envelope {
            host_event: Event {
                sequence: self.sequence,
                event,
            },
        })
        .map_err(|e| e.to_string())?;
        if bytes.len() + 1 > MAX_EVENT_BYTES {
            return Err("Native policy event exceeds 8 MiB".into());
        }
        self.output
            .write_all(&bytes)
            .and_then(|_| self.output.write_all(b"\n"))
            .and_then(|_| self.output.flush())
            .map_err(|e| format!("Write native policy event: {e}"))?;
        let mut bytes = vec![];
        (&mut self.input)
            .take((crate::protocol::MAX_REQUEST_BYTES + 1) as u64)
            .read_until(b'\n', &mut bytes)
            .map_err(|e| format!("Read native policy reply: {e}"))?;
        if bytes.len() > crate::protocol::MAX_REQUEST_BYTES {
            return Err("Native policy reply exceeds 96 MiB".into());
        }
        if bytes.last() != Some(&b'\n') {
            return Err("Native policy reply missing complete newline frame".into());
        }
        let reply: ReplyEnvelope = serde_json::from_slice(&bytes)
            .map_err(|e| format!("Invalid native policy reply: {e}"))?;
        if reply.host_reply.sequence != self.sequence {
            return Err("Native policy reply has stale or unexpected sequence".into());
        }
        match reply.host_reply.outcome {
            Outcome::Return { value } => Ok(value),
            Outcome::Throw { message } => Err(message),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Cursor;

    #[test]
    fn callbacks_are_framed_in_order_and_do_not_consume_the_next_request() {
        let input = concat!(
            "{\"hostReply\":{\"sequence\":1,\"outcome\":{\"kind\":\"return\",\"value\":{\"publish\":[0,255]}}}}\n",
            "{\"hostReply\":{\"sequence\":2,\"outcome\":{\"kind\":\"return\",\"value\":null}}}\n",
            "{\"op\":\"capabilities\"}\n"
        );
        let mut input = Cursor::new(input.as_bytes());
        let mut output = Vec::new();
        {
            let mut channel = HostChannel::new(&mut input, &mut output);
            assert_eq!(
                channel.exchange(&json!({"kind":"mainBody"})).unwrap(),
                json!({"publish":[0,255]})
            );
            assert_eq!(
                channel.exchange(&json!({"kind":"crawlHead"})).unwrap(),
                Value::Null
            );
        }
        let frames: Vec<Value> = String::from_utf8(output)
            .unwrap()
            .lines()
            .map(|s| serde_json::from_str(s).unwrap())
            .collect();
        assert_eq!(
            frames,
            vec![
                json!({"hostEvent":{"sequence":1,"event":{"kind":"mainBody"}}}),
                json!({"hostEvent":{"sequence":2,"event":{"kind":"crawlHead"}}})
            ]
        );
        let mut next = String::new();
        input.read_line(&mut next).unwrap();
        assert_eq!(next, "{\"op\":\"capabilities\"}\n");
    }

    #[test]
    fn failed_or_mismatched_callbacks_poison_the_channel() {
        for line in [
            "{\"hostReply\":{\"sequence\":0,\"outcome\":{\"kind\":\"return\",\"value\":null}}}\n",
            "{\"hostReply\":{\"sequence\":1,\"extra\":0,\"outcome\":{\"kind\":\"return\",\"value\":null}}}\n",
            "{\"hostReply\":{\"sequence\":1,\"outcome\":{\"kind\":\"throw\",\"message\":\"host failed\"}}}\n",
            "{\"op\":\"capabilities\"}\n",
            "null\n",
            "",
            "{}",
        ] {
            let mut channel = HostChannel::new(Cursor::new(line), Vec::new());
            assert!(channel.exchange(&Value::Null).is_err(), "{line}");
            assert_eq!(
                channel.exchange(&Value::Null).unwrap_err(),
                "Native policy channel is failed"
            );
        }
    }
}
