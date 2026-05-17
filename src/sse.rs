/// SSE frame splitter. Takes a mutable buffer and a new chunk, parses out
/// complete events (terminated by \n\n), and returns them. Incomplete tail
/// bytes stay in the buffer.
pub fn split_events(buf: &mut Vec<u8>, chunk: &[u8]) -> Vec<SseEvent> {
    buf.extend_from_slice(chunk);

    // Normalize \r\n to \n in-place.
    let mut i = 0;
    while i + 1 < buf.len() {
        if buf[i] == b'\r' && buf[i + 1] == b'\n' {
            buf.remove(i);
        } else {
            i += 1;
        }
    }

    let mut events = Vec::new();
    let mut consumed = 0;

    while let Some(sep) = buf[consumed..]
        .windows(2)
        .position(|w| w == b"\n\n")
        .map(|p| consumed + p)
    {
        // Find next \n\n delimiter.
        let event_bytes = &buf[consumed..sep];
        if let Some(ev) = parse_event(event_bytes) {
            events.push(ev);
        }
        consumed = sep + 2;
    }

    buf.drain(..consumed);
    events
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

    #[test]
    fn single_event() {
        let mut buf = Vec::new();
        let chunk = b"event: message_start\ndata: {\"type\":\"start\"}\n\n";
        let events = split_events(&mut buf, chunk);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "message_start");
        assert_eq!(events[0].data, "{\"type\":\"start\"}");
        assert!(buf.is_empty());
    }

    #[test]
    fn split_across_chunks() {
        let mut buf = Vec::new();
        let e1 = split_events(&mut buf, b"event: foo\ndata: hello");
        assert!(e1.is_empty()); // no \n\n yet
        let e2 = split_events(&mut buf, b"\n\n");
        assert_eq!(e2.len(), 1);
        assert_eq!(e2[0].data, "hello");
    }

    #[test]
    fn done_sentinel_skipped() {
        let mut buf = Vec::new();
        let events = split_events(&mut buf, b"data: [DONE]\n\n");
        assert!(events.is_empty());
    }

    #[test]
    fn multiple_events_in_one_chunk() {
        let mut buf = Vec::new();
        let chunk = b"data: first\n\ndata: second\n\n";
        let events = split_events(&mut buf, chunk);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].data, "first");
        assert_eq!(events[1].data, "second");
    }

    #[test]
    fn crlf_normalized() {
        let mut buf = Vec::new();
        let events = split_events(&mut buf, b"data: crlf\r\n\r\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "crlf");
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
            let mut ref_buf = Vec::new();
            let reference = split_events(&mut ref_buf, &stream);

            // Collect events from the chunked parse.
            let mut test_buf = Vec::new();
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
                got.extend(split_events(&mut test_buf, &stream[prev..pt]));
                prev = pt;
            }

            prop_assert_eq!(reference, got);
        }
    }
}
