//! 按需求场景验证只读校验及所有硬拒绝来源。

use super::*;
use testkit::{Extension, Frame, GifBuilder, quota_gif};

fn single() -> GifBuilder {
    GifBuilder::default().frame(Frame::default())
}

fn codes(report: &Report) -> Vec<ErrorCode> {
    report.errors.iter().map(|error| error.code).collect()
}

fn assert_rejected(bytes: &[u8], expected: &[ErrorCode]) -> Report {
    let original = bytes.to_vec();
    let report = validate(bytes);
    assert!(!report.ok);
    assert_eq!(codes(&report), expected);
    for error in &report.errors {
        assert!(!error.message.is_ascii(), "错误说明必须包含中文");
        assert!(
            error.message.chars().any(|c| c.is_ascii_digit()),
            "错误说明必须包含具体数值"
        );
    }
    assert_eq!(bytes, original);
    report
}

#[test]
fn quota_six_frames_without_loop() {
    let bytes = quota_gif();
    let original = bytes.clone();
    let report = validate(&bytes);
    assert!(report.ok, "报告：{report:?}");
    assert!(report.errors.is_empty());
    assert_eq!(report.bytes, bytes.len());
    assert_eq!(report.version, "GIF89a");
    assert_eq!((report.width, report.height), (320, 240));
    assert_eq!(report.frames, 6);
    assert_eq!(report.delays_ms, [600, 600, 600, 600, 417600, 60000]);
    assert_eq!(report.total_duration_ms, 480000);
    assert_eq!(report.loop_count, None);
    assert_eq!(report.trailing_bytes, 0);
    assert_eq!(bytes, original);
}

#[test]
fn custom_four_frames_with_infinite_loop() {
    let report = validate(
        &GifBuilder {
            loop_count: Some(0),
            frames: vec![Frame::default(); 4],
            ..GifBuilder::default()
        }
        .build(),
    );
    assert!(report.ok);
    assert_eq!(report.frames, 4);
    assert_eq!(report.loop_count, Some(0));
}

#[test]
fn gif87a_skips_structure() {
    let bytes = GifBuilder {
        version: "GIF87a".into(),
        ..single()
    }
    .build();
    let report = assert_rejected(&bytes, &[ErrorCode::HeaderNotGif89a]);
    assert_eq!(report.version, "GIF87a");
    assert_eq!((report.width, report.height), (320, 240));
    assert_unobserved(&report);
}

#[test]
fn newline_after_trailer_collects_both_errors() {
    let bytes = GifBuilder {
        trailing_bytes: vec![0x0a],
        ..single()
    }
    .build();
    let report = assert_rejected(
        &bytes,
        &[ErrorCode::LastByteNotTrailer, ErrorCode::TrailingBytes],
    );
    assert_eq!(report.trailing_bytes, 1);
    assert_eq!(report.frames, 1);
}

#[test]
fn wrong_screen_size() {
    let report = assert_rejected(
        &GifBuilder {
            height: 200,
            ..single()
        }
        .build(),
        &[ErrorCode::SizeNot320x240],
    );
    assert_eq!((report.width, report.height), (320, 200));
    assert_unobserved(&report);
}

fn sized_gif(size: usize) -> Vec<u8> {
    // 无扩展及色表的单帧固定开销为 26 字节，另加每 255 字节的长度前缀。
    let data_bytes = (0..size)
        .find(|n| 26 + n + n.div_ceil(255) == size)
        .expect("目标长度必须可由子块拼装");
    let bytes = GifBuilder::default()
        .frame(Frame {
            data_bytes,
            ..Frame::default()
        })
        .build();
    assert_eq!(bytes.len(), size);
    bytes
}

#[test]
fn exceeds_ram_limit_by_one_byte() {
    let report = assert_rejected(&sized_gif(262145), &[ErrorCode::TooLarge]);
    assert_eq!(report.bytes, 262145);
    assert_eq!(report.frames, 1);
}

#[test]
fn six_bytes_is_too_small() {
    let bytes = single().build();
    let report = assert_rejected(
        &bytes[..6],
        &[
            ErrorCode::SizeNot320x240,
            ErrorCode::TooSmall,
            ErrorCode::LastByteNotTrailer,
        ],
    );
    assert_eq!(report.version, "GIF89a");
    assert_eq!((report.width, report.height), (0, 0));
    assert_unobserved(&report);
}

#[test]
fn last_byte_replaced_with_zero() {
    let mut bytes = single().build();
    *bytes.last_mut().unwrap() = 0;
    let report = assert_rejected(
        &bytes,
        &[
            ErrorCode::LastByteNotTrailer,
            ErrorCode::StructureWalkFailed,
        ],
    );
    assert_eq!(report.frames, 1);
}

#[test]
fn zero_frames() {
    assert_rejected(&GifBuilder::default().build(), &[ErrorCode::NoFrames]);
}

#[test]
fn single_frame_is_accepted() {
    let report = validate(&single().build());
    assert!(report.ok);
    assert_eq!(report.frames, 1);
}

#[test]
fn image_sub_block_truncated() {
    let mut bytes = single().build();
    bytes[24] = 255;
    let report = assert_rejected(
        &bytes,
        &[ErrorCode::StructureWalkFailed, ErrorCode::NoFrames],
    );
    assert!(report.errors[0].message.contains("25"));
    assert!(report.errors[0].message.contains("255"));
}

#[test]
fn frame_rectangle_out_of_bounds_only_once_and_walk_continues() {
    let bytes = GifBuilder::default()
        .frame(Frame {
            left: 1,
            delay_cs: Some(1),
            ..Frame::default()
        })
        .frame(Frame {
            top: u16::MAX,
            height: u16::MAX,
            delay_cs: Some(2),
            ..Frame::default()
        })
        .frame(Frame::default())
        .build();
    let report = assert_rejected(&bytes, &[ErrorCode::FrameOutOfBounds]);
    assert_eq!(report.frames, 3);
    assert_eq!(report.delays_ms, [10, 20, 0]);
    assert_eq!(report.total_duration_ms, 30);
}

#[test]
fn missing_graphic_control_means_zero_delay() {
    let report = validate(
        &GifBuilder::default()
            .frame(Frame {
                delay_cs: Some(123),
                ..Frame::default()
            })
            .frame(Frame::default())
            .frame(Frame {
                delay_cs: Some(2),
                ..Frame::default()
            })
            .build(),
    );
    assert!(report.ok);
    assert_eq!(report.delays_ms, [1230, 0, 20]);
    assert_eq!(report.total_duration_ms, 1250);
}

#[test]
fn plain_text_extension_consumes_pending_graphic_control() {
    let report = validate(
        &GifBuilder::default()
            .frame(Frame {
                delay_cs: Some(60),
                leading_extensions: vec![Extension::PlainText],
                ..Frame::default()
            })
            .frame(Frame {
                delay_cs: Some(2),
                ..Frame::default()
            })
            .build(),
    );
    assert!(report.ok);
    assert_eq!(report.frames, 2);
    assert_eq!(report.delays_ms, [0, 20]);
    assert_eq!(report.total_duration_ms, 20);
}

#[test]
fn comment_extension_keeps_pending_graphic_control() {
    let report = validate(
        &GifBuilder::default()
            .frame(Frame {
                delay_cs: Some(60),
                leading_extensions: vec![Extension::Comment],
                ..Frame::default()
            })
            .build(),
    );
    assert!(report.ok);
    assert_eq!(report.frames, 1);
    assert_eq!(report.delays_ms, [600]);
    assert_eq!(report.total_duration_ms, 600);
}

#[test]
fn unknown_block_introducer() {
    let mut bytes = single().build();
    bytes[13] = 0x7f;
    let report = assert_rejected(
        &bytes,
        &[ErrorCode::StructureWalkFailed, ErrorCode::NoFrames],
    );
    assert!(report.errors[0].message.contains("13"));
    assert!(report.errors[0].message.contains("0x7F"));
}

#[test]
fn all_device_errors_are_collected_in_order() {
    let mut bytes = GifBuilder {
        version: "GIF87a".into(),
        height: 200,
        ..single()
    }
    .build();
    bytes.truncate(12);
    assert_rejected(
        &bytes,
        &[
            ErrorCode::HeaderNotGif89a,
            ErrorCode::SizeNot320x240,
            ErrorCode::TooSmall,
            ErrorCode::LastByteNotTrailer,
        ],
    );
    bytes[..6].copy_from_slice(b"GIF89a");
    assert_rejected(
        &bytes,
        &[
            ErrorCode::SizeNot320x240,
            ErrorCode::TooSmall,
            ErrorCode::LastByteNotTrailer,
        ],
    );
}

fn assert_unobserved(report: &Report) {
    assert_eq!(report.frames, 0);
    assert!(report.delays_ms.is_empty());
    assert_eq!(report.total_duration_ms, 0);
    assert_eq!(report.loop_count, None);
    assert_eq!(report.trailing_bytes, 0);
}

#[test]
fn short_and_lossy_headers_report_raw_values_without_walking() {
    for len in 0..10 {
        let bytes = single().build();
        let report = validate(&bytes[..len]);
        assert_eq!(report.version, if len >= 6 { "GIF89a" } else { "" });
        assert_eq!((report.width, report.height), (0, 0));
        assert_unobserved(&report);
    }
    let mut bytes = single().build();
    bytes[0] = 0xff;
    let report = assert_rejected(&bytes, &[ErrorCode::HeaderNotGif89a]);
    assert_eq!(report.version, "�IF89a");
    assert_unobserved(&report);
}

#[test]
fn byte_limits_do_not_enforce_producer_or_flash_limits() {
    for size in [90001, 200000, 221446, 262144] {
        assert!(validate(&sized_gif(size)).ok, "长度：{size}");
    }
    let bytes = single().build();
    let report = validate(&bytes[..13]);
    assert!(!codes(&report).contains(&ErrorCode::TooSmall));
    assert!(codes(&report).contains(&ErrorCode::StructureWalkFailed));
}

#[test]
fn all_color_table_sizes_and_offset_rectangles() {
    for n in 0..=7 {
        let bytes = GifBuilder {
            global_color_table: Some(n),
            ..GifBuilder::default()
        }
        .frame(Frame {
            left: 300,
            top: 200,
            width: 20,
            height: 40,
            local_color_table: Some(n),
            ..Frame::default()
        })
        .build();
        assert!(validate(&bytes).ok, "色表大小指数：{n}");
    }
}

#[test]
fn every_prefix_truncation_is_detected() {
    let bytes = GifBuilder {
        global_color_table: Some(2),
        loop_count: Some(513),
        ..GifBuilder::default()
    }
    .frame(Frame {
        local_color_table: Some(3),
        delay_cs: Some(513),
        data_bytes: 300,
        ..Frame::default()
    })
    .build();
    assert!(validate(&bytes).ok);
    for end in 10..bytes.len() {
        let report = validate(&bytes[..end]);
        assert!(
            codes(&report).contains(&ErrorCode::StructureWalkFailed),
            "截断偏移 {end}，报告 {report:?}"
        );
    }
}

#[test]
fn missing_trailer_preserves_completed_frames() {
    let bytes = GifBuilder {
        trailer: false,
        ..single()
    }
    .build();
    let report = assert_rejected(
        &bytes,
        &[
            ErrorCode::LastByteNotTrailer,
            ErrorCode::StructureWalkFailed,
        ],
    );
    assert_eq!(report.frames, 1);
}

#[test]
fn extra_trailer_is_still_trailing_data() {
    let bytes = GifBuilder {
        trailing_bytes: vec![0x3b],
        ..single()
    }
    .build();
    let report = assert_rejected(&bytes, &[ErrorCode::TrailingBytes]);
    assert_eq!(report.trailing_bytes, 1);
}

fn with_extension(extension: &[u8]) -> Vec<u8> {
    let mut bytes = single().build();
    bytes.splice(13..13, extension.iter().copied());
    bytes
}

#[test]
fn unknown_extensions_skip_all_sub_blocks_without_interpreting_payload() {
    for extension in [
        vec![0x21, 0xfe, 3, 0x3b, 0x21, 0x2c, 2, 0, 0xff, 0],
        vec![0x21, 0x01, 1, 0xff, 0],
        vec![0x21, 0x77, 0],
        vec![0x21, 0xff, 3, 1, 2, 3, 0],
    ] {
        let report = validate(&with_extension(&extension));
        assert!(report.ok, "报告：{report:?}");
        assert_eq!(report.frames, 1);
        assert_eq!(report.loop_count, None);
    }
}

#[test]
fn loop_count_is_little_endian_and_only_first_data_block_is_used() {
    let report = validate(
        &GifBuilder {
            loop_count: Some(513),
            ..single()
        }
        .build(),
    );
    assert!(report.ok);
    assert_eq!(report.loop_count, Some(513));
    let mut extension = vec![0x21, 0xff, 11];
    extension.extend(b"NETSCAPE2.0");
    extension.extend([3, 2, 0, 0, 3, 1, 7, 0, 0]);
    let report = validate(&with_extension(&extension));
    assert!(report.ok);
    assert_eq!(report.loop_count, None);
}

#[test]
fn incomplete_known_extension_fields_are_structure_errors() {
    let mut netscape = vec![0x21, 0xff, 11];
    netscape.extend(b"NETSCAPE2.0");
    netscape.extend([2, 1, 0, 0]);
    for extension in [vec![0x21, 0xf9, 0], vec![0x21, 0xf9, 2, 0, 0, 0], netscape] {
        assert_rejected(
            &with_extension(&extension),
            &[ErrorCode::StructureWalkFailed, ErrorCode::NoFrames],
        );
    }
}

#[test]
fn graphic_control_survives_skipped_extensions_and_latest_value_wins() {
    let report = validate(&with_extension(&[
        0x21, 0xf9, 4, 0, 1, 0, 0, 0, 0x21, 0xf9, 4, 0, 1, 2, 0, 0, 0x21, 0xfe, 1, 0x3b, 0,
    ]));
    assert!(report.ok);
    assert_eq!(report.delays_ms, [5130]);
    assert_eq!(report.total_duration_ms, 5130);
}

#[test]
fn lzw_contents_and_zero_sized_rectangles_add_no_rejection_rules() {
    let mut bytes = GifBuilder::default()
        .frame(Frame {
            width: 0,
            height: 0,
            data_bytes: 0,
            ..Frame::default()
        })
        .build();
    bytes[23] = 255;
    assert!(validate(&bytes).ok);
}

#[test]
fn stable_machine_codes() {
    for (code, expected) in [
        (ErrorCode::HeaderNotGif89a, "header_not_gif89a"),
        (ErrorCode::SizeNot320x240, "size_not_320x240"),
        (ErrorCode::TooSmall, "too_small"),
        (ErrorCode::TooLarge, "too_large"),
        (ErrorCode::LastByteNotTrailer, "last_byte_not_trailer"),
        (ErrorCode::StructureWalkFailed, "structure_walk_failed"),
        (ErrorCode::TrailingBytes, "trailing_bytes"),
        (ErrorCode::NoFrames, "no_frames"),
        (ErrorCode::FrameOutOfBounds, "frame_out_of_bounds"),
    ] {
        assert_eq!(code.as_str(), expected);
    }
}
