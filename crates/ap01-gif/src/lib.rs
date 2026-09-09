//! AP01 的零依赖 GIF 块校验器，只读取结构，不解码或改写像素。

#[cfg(any(test, feature = "testkit"))]
pub mod testkit;

/// 稳定的九类拒绝原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    HeaderNotGif89a,
    SizeNot320x240,
    TooSmall,
    TooLarge,
    LastByteNotTrailer,
    StructureWalkFailed,
    TrailingBytes,
    NoFrames,
    FrameOutOfBounds,
}

impl ErrorCode {
    /// 返回供机器使用的稳定错误码。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::HeaderNotGif89a => "header_not_gif89a",
            Self::SizeNot320x240 => "size_not_320x240",
            Self::TooSmall => "too_small",
            Self::TooLarge => "too_large",
            Self::LastByteNotTrailer => "last_byte_not_trailer",
            Self::StructureWalkFailed => "structure_walk_failed",
            Self::TrailingBytes => "trailing_bytes",
            Self::NoFrames => "no_frames",
            Self::FrameOutOfBounds => "frame_out_of_bounds",
        }
    }
}

/// 一项机器码及包含实际观测值的中文说明。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationError {
    pub code: ErrorCode,
    pub message: String,
}

/// 原始字节与已观测结构的报告；未遍历部分保持默认值。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub ok: bool,
    pub errors: Vec<ValidationError>,
    pub bytes: usize,
    pub version: String,
    pub width: u16,
    pub height: u16,
    pub frames: usize,
    pub delays_ms: Vec<u32>,
    pub total_duration_ms: u64,
    pub loop_count: Option<u16>,
    pub trailing_bytes: usize,
}

impl Report {
    fn reject(&mut self, code: ErrorCode, message: String) {
        self.errors.push(ValidationError { code, message });
    }
}

/// 收集设备规则与块结构错误；不解码 LZW，不写文件，不修改输入。
pub fn validate(bytes: &[u8]) -> Report {
    let mut report = Report {
        ok: false,
        errors: Vec::new(),
        bytes: bytes.len(),
        version: bytes
            .get(..6)
            .map(|v| String::from_utf8_lossy(v).into_owned())
            .unwrap_or_default(),
        width: if bytes.len() >= 10 {
            le_u16(&bytes[6..8])
        } else {
            0
        },
        height: if bytes.len() >= 10 {
            le_u16(&bytes[8..10])
        } else {
            0
        },
        frames: 0,
        delays_ms: Vec::new(),
        total_duration_ms: 0,
        loop_count: None,
        trailing_bytes: 0,
    };
    let header_ok = bytes.get(..6) == Some(b"GIF89a");
    let size_ok = report.width == 320 && report.height == 240;
    if !header_ok {
        report.reject(
            ErrorCode::HeaderNotGif89a,
            format!(
                "头不是 GIF89a：实际版本 {:?}，输入共 {} 字节",
                report.version,
                bytes.len()
            ),
        );
    }
    if !size_ok {
        report.reject(
            ErrorCode::SizeNot320x240,
            format!(
                "尺寸不是 320x240：实际为 {}x{}，输入共 {} 字节",
                report.width,
                report.height,
                bytes.len()
            ),
        );
    }
    if bytes.len() < 13 {
        report.reject(
            ErrorCode::TooSmall,
            format!("体积小于 13 字节：实际 {} 字节", bytes.len()),
        );
    } else if bytes.len() > 262_144 {
        report.reject(
            ErrorCode::TooLarge,
            format!("体积大于 262144 字节：实际 {} 字节", bytes.len()),
        );
    }
    if bytes.last() != Some(&0x3b) {
        let actual = bytes
            .last()
            .map(|v| format!("0x{v:02X}"))
            .unwrap_or_else(|| "无字节".into());
        report.reject(
            ErrorCode::LastByteNotTrailer,
            format!(
                "最后一字节不是 0x3B：实际为 {actual}，输入共 {} 字节",
                bytes.len()
            ),
        );
    }
    if header_ok && size_ok {
        if let Err(message) = walk(bytes, &mut report) {
            report.reject(ErrorCode::StructureWalkFailed, message);
        }
        if report.frames == 0 {
            report.reject(ErrorCode::NoFrames, "帧数为 0：未观测到完整图像帧".into());
        }
    }
    report.ok = report.errors.is_empty();
    report
}

fn le_u16(bytes: &[u8]) -> u16 {
    u16::from_le_bytes([bytes[0], bytes[1]])
}

struct Cursor<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Cursor<'a> {
    fn take(&mut self, count: usize) -> Result<&'a [u8], String> {
        let remaining = self.bytes.len() - self.offset;
        if count > remaining {
            return Err(format!(
                "块遍历失败：偏移 {} 需要 {count} 字节，实际剩余 {remaining} 字节",
                self.offset
            ));
        }
        let start = self.offset;
        self.offset += count;
        Ok(&self.bytes[start..self.offset])
    }

    fn byte(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }

    fn color_table(&mut self, packed: u8) -> Result<(), String> {
        if packed & 0x80 != 0 {
            self.take(3 * (1 << ((packed & 7) + 1)))?;
        }
        Ok(())
    }

    fn sub_block(&mut self) -> Result<&'a [u8], String> {
        let count = usize::from(self.byte()?);
        self.take(count)
    }

    fn sub_blocks(&mut self) -> Result<(), String> {
        while !self.sub_block()?.is_empty() {}
        Ok(())
    }
}

fn walk(bytes: &[u8], report: &mut Report) -> Result<(), String> {
    let mut cursor = Cursor { bytes, offset: 6 };
    let screen = cursor.take(7)?;
    cursor.color_table(screen[4])?;
    let mut pending_delay = 0;
    loop {
        let block_offset = cursor.offset;
        match cursor.byte()? {
            0x21 => {
                let label = cursor.byte()?;
                let first = cursor.sub_block()?;
                // 纯文本扩展是图形渲染块，消耗它前面的图形控制扩展；注释与应用扩展不消耗。
                if label == 0x01 {
                    pending_delay = 0;
                }
                if first.is_empty() && label != 0xf9 {
                    continue;
                }
                match label {
                    0xf9 => {
                        if first.len() < 4 {
                            return Err(format!(
                                "块遍历失败：偏移 {block_offset} 的图形控制扩展需要至少 4 字节，实际 {} 字节",
                                first.len()
                            ));
                        }
                        pending_delay = u32::from(le_u16(&first[1..3])) * 10;
                        cursor.sub_blocks()?;
                    }
                    0xff if first == b"NETSCAPE2.0" => {
                        let data = cursor.sub_block()?;
                        if data.first() == Some(&1) {
                            if data.len() < 3 {
                                return Err(format!(
                                    "块遍历失败：偏移 {block_offset} 的循环子块需要至少 3 字节，实际 {} 字节",
                                    data.len()
                                ));
                            }
                            report.loop_count = Some(le_u16(&data[1..3]));
                        }
                        if !data.is_empty() {
                            cursor.sub_blocks()?;
                        }
                    }
                    _ => cursor.sub_blocks()?,
                }
            }
            0x2c => {
                let descriptor = cursor.take(9)?;
                let left = u32::from(le_u16(&descriptor[..2]));
                let top = u32::from(le_u16(&descriptor[2..4]));
                let width = u32::from(le_u16(&descriptor[4..6]));
                let height = u32::from(le_u16(&descriptor[6..8]));
                if (left + width > 320 || top + height > 240)
                    && !report
                        .errors
                        .iter()
                        .any(|e| e.code == ErrorCode::FrameOutOfBounds)
                {
                    report.reject(ErrorCode::FrameOutOfBounds, format!("帧矩形超出逻辑屏幕 320x240：块偏移 {block_offset}，左上角 ({left},{top})，尺寸 {width}x{height}"));
                }
                cursor.color_table(descriptor[8])?;
                cursor.byte()?;
                cursor.sub_blocks()?;
                report.frames += 1;
                report.delays_ms.push(pending_delay);
                report.total_duration_ms += u64::from(pending_delay);
                pending_delay = 0;
            }
            0x3b => {
                report.trailing_bytes = bytes.len() - cursor.offset;
                if report.trailing_bytes > 0 {
                    report.reject(
                        ErrorCode::TrailingBytes,
                        format!(
                            "结束符之后有多余字节：结束符偏移 {block_offset}，多余 {} 字节",
                            report.trailing_bytes
                        ),
                    );
                }
                return Ok(());
            }
            byte => {
                return Err(format!(
                    "块遍历失败：偏移 {block_offset} 出现未知引导字节 0x{byte:02X}"
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests;
