// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// log_buffer_test.rs — see module docs

// Copyright (c) 2026 Jules IMF
//! LogBuffer unit tests.

use teststand::logging::{LogBuffer, LogEntry};

#[test]
fn log_buffer_capacity() {
    let buf = LogBuffer::new(3);
    for i in 0..5 {
        buf.push(LogEntry {
            ts_ms: i,
            level: "info".into(),
            target: "t".into(),
            message: i.to_string(),
        });
    }
    let tail = buf.tail(10);
    assert_eq!(tail.len(), 3);
    assert_eq!(tail[0].message, "2");
    assert_eq!(tail[2].message, "4");
}

#[test]
fn log_buffer_tail_n() {
    let buf = LogBuffer::new(100);
    for i in 0..20 {
        buf.push(LogEntry {
            ts_ms: i,
            level: "info".into(),
            target: "".into(),
            message: i.to_string(),
        });
    }
    assert_eq!(buf.tail(5).len(), 5);
    assert_eq!(buf.tail(5).last().unwrap().message, "19");
}
