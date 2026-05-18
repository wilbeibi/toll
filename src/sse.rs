/// Bounded SSE frame splitter. It keeps only the incomplete current event.
pub struct SseSplitter {
    buf: Vec<u8>,
    max_event_bytes: usize,
}

#[derive(Debug, PartialEq, Eq)]
pub struct SseBufferOverflow;

impl SseSplitter {
    pub fn new(max_event_bytes: usize) -> Self {
        Self {
            buf: Vec::new(),
            max_event_bytes,
        }
    }

    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<SseEvent>, SseBufferOverflow> {
        let mut events = Vec::new();

        self.buf.extend_from_slice(chunk);

        while let Some((event_len, delimiter_len)) = find_event(&self.buf) {
            if let Some(ev) = parse_event(&self.buf[..event_len]) {
                events.push(ev);
            }
            self.buf.drain(..event_len + delimiter_len);
        }

        if self.buf.len() > self.max_event_bytes {
            self.buf.clear();
            return Err(SseBufferOverflow);
        }

        Ok(events)
    }
}

/// Returns the position and length of the earliest event delimiter (`\n\n` or
/// `\r\n\r\n`) in `buf`, enabling multiple complete events per chunk.
fn find_event(buf: &[u8]) -> Option<(usize, usize)> {
    let rn = (buf.len() >= 4)
        .then(|| buf.windows(4).position(|w| w == b"\r\n\r\n"))
        .flatten();
    let nn = (buf.len() >= 2)
        .then(|| buf.windows(2).position(|w| w == b"\n\n"))
        .flatten();
    match (rn, nn) {
        (None, None) => None,
        (Some(p), None) => Some((p, 4)),
        (None, Some(p)) => Some((p, 2)),
        (Some(rn_p), Some(nn_p)) => {
            if nn_p < rn_p {
                Some((nn_p, 2))
            } else {
                Some((rn_p, 4))
            }
        }
    }
}

#[derive(Debug, PartialEq)]
pub struct SseEvent {
    pub event_type: String,
    pub data: String,
}

fn parse_event(blob: &[u8]) -> Option<SseEvent> {
    let mut event_type = String::new();
    let mut data_parts: Vec<&str> = Vec::new();

    for line in blob.split(|&b| b == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.starts_with(b"event:") {
            event_type = std::str::from_utf8(&line[6..])
                .unwrap_or("")
                .trim()
                .to_string();
        } else if line.starts_with(b"data:") {
            let s = std::str::from_utf8(&line[5..]).unwrap_or("").trim_start();
            data_parts.push(s);
        }
    }

    if data_parts.is_empty() {
        return None;
    }

    let data = data_parts.join("\n");
    if data.is_empty() || data == "[DONE]" {
        return None;
    }

    Some(SseEvent { event_type, data })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn split_all(splitter: &mut SseSplitter, chunk: &[u8]) -> Vec<SseEvent> {
        splitter.push(chunk).unwrap()
    }

    #[test]
    fn split_across_chunks() {
        let mut splitter = SseSplitter::new(1024);
        let e1 = split_all(&mut splitter, b"event: foo\ndata: hello");
        assert!(e1.is_empty()); // no \n\n yet
        let e2 = split_all(&mut splitter, b"\n\n");
        assert_eq!(e2.len(), 1);
        assert_eq!(e2[0].data, "hello");
    }

    #[test]
    fn done_sentinel_skipped() {
        let mut splitter = SseSplitter::new(1024);
        let events = split_all(&mut splitter, b"data: [DONE]\n\n");
        assert!(events.is_empty());
    }

    #[test]
    fn crlf_normalized() {
        let mut splitter = SseSplitter::new(1024);
        let events = split_all(&mut splitter, b"data: crlf\r\n\r\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "crlf");
    }

    #[test]
    fn overflow_is_reported_and_buffer_is_cleared() {
        let mut splitter = SseSplitter::new(9);
        assert_eq!(
            splitter.push(b"data: too long without delimiter"),
            Err(SseBufferOverflow)
        );
        let events = split_all(&mut splitter, b"data:ok\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "ok");
    }
}

#[cfg(test)]
mod prop_tests {
    use super::*;
    use proptest::prelude::*;

    fn well_formed_sse(events: &[(&str, &str)]) -> Vec<u8> {
        let mut out = Vec::new();
        for (etype, data) in events {
            if !etype.is_empty() {
                out.extend_from_slice(format!("event: {etype}\n").as_bytes());
            }
            out.extend_from_slice(format!("data: {data}\n\n").as_bytes());
        }
        out
    }

    proptest! {
        #[test]
        fn chunk_boundary_independence(
            events in proptest::collection::vec(
                ("[a-z]{1,8}", "[a-z0-9 ]{1,20}"),
                1..=8usize,
            ),
            split_points in proptest::collection::vec(0usize..200, 1..=10usize),
        ) {
            let pairs: Vec<(&str, &str)> = events.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
            let stream = well_formed_sse(&pairs);
            if stream.is_empty() {
                return Ok(());
            }

            // Collect reference events from a single-chunk parse.
            let mut ref_splitter = SseSplitter::new(4096);
            let reference = ref_splitter.push(&stream).unwrap();

            // Collect events from the chunked parse.
            let mut test_splitter = SseSplitter::new(4096);
            let mut got: Vec<SseEvent> = Vec::new();
            let mut prev = 0;
            let mut points: Vec<usize> = split_points.into_iter()
                .map(|p| p % (stream.len() + 1))
                .collect();
            points.sort();
            points.dedup();
            points.push(stream.len());
            for &pt in &points {
                if pt > stream.len() { continue; }
                if pt <= prev { continue; }
                got.extend(test_splitter.push(&stream[prev..pt]).unwrap());
                prev = pt;
            }

            prop_assert_eq!(reference, got);
        }
    }
}
