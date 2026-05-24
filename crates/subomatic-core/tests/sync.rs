// SPDX-License-Identifier: Apache-2.0
//! End-to-end test of the public sync API: parse SRT, sync to a reference
//! activity signal, and confirm the cues move while payloads stay intact.

use subomatic_core::{srt, sync, AlignParams, Span};

#[test]
fn sync_corrects_a_global_shift_end_to_end() {
    // The subtitle is 3 s late relative to the reference activity.
    let late = "1\n00:00:04,000 --> 00:00:05,000\nHi\n\n2\n00:00:08,000 --> 00:00:09,000\nBye\n";
    let sub = srt::parse(late).unwrap();
    let reference = vec![Span::new(1_000, 2_000), Span::new(5_000, 6_000)];

    let synced = sync(&sub, &reference, &AlignParams::default());

    // Cues should land within one search step of the reference positions.
    assert!((synced.cues[0].start_ms - 1_000).abs() <= 20);
    assert!((synced.cues[1].start_ms - 5_000).abs() <= 20);
    // Payloads are preserved.
    assert_eq!(synced.cues[0].payload, "Hi");
    assert_eq!(synced.cues[1].payload, "Bye");
}
