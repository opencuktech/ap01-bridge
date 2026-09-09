//! 仅用于结构测试的字节拼装器，图像数据为占位字节，不保证能够解码。

/// 插在图形控制扩展与图像描述符之间的扩展块，用于测试延时归属。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Extension {
    /// 纯文本扩展（标签 0x01），是图形渲染块。
    PlainText,
    /// 注释扩展（标签 0xfe），不是图形渲染块。
    Comment,
}

/// 一帧的矩形、可选延时与占位图像数据长度。
#[derive(Debug, Clone)]
pub struct Frame {
    pub left: u16,
    pub top: u16,
    pub width: u16,
    pub height: u16,
    /// 以百分之一秒为单位，与图形控制扩展的原始字段一致。
    pub delay_cs: Option<u16>,
    pub data_bytes: usize,
    /// 色表大小指数，取值为 0 到 7；空值表示没有局部色表。
    pub local_color_table: Option<u8>,
    /// 写在图形控制扩展之后、图像描述符之前的扩展块。
    pub leading_extensions: Vec<Extension>,
}

impl Default for Frame {
    fn default() -> Self {
        Self {
            left: 0,
            top: 0,
            width: 320,
            height: 240,
            delay_cs: None,
            data_bytes: 2,
            local_color_table: None,
            leading_extensions: Vec::new(),
        }
    }
}

/// 可逐项修改的夹具配置；默认是无帧、无色表、带结束符的 GIF89a。
#[derive(Debug, Clone)]
pub struct GifBuilder {
    pub version: String,
    pub width: u16,
    pub height: u16,
    /// 色表大小指数，取值为 0 到 7；空值表示没有全局色表。
    pub global_color_table: Option<u8>,
    pub loop_count: Option<u16>,
    pub frames: Vec<Frame>,
    pub trailer: bool,
    pub trailing_bytes: Vec<u8>,
}

impl Default for GifBuilder {
    fn default() -> Self {
        Self {
            version: "GIF89a".into(),
            width: 320,
            height: 240,
            global_color_table: None,
            loop_count: None,
            frames: Vec::new(),
            trailer: true,
            trailing_bytes: Vec::new(),
        }
    }
}

impl GifBuilder {
    /// 追加一帧，允许链式构造。
    pub fn frame(mut self, frame: Frame) -> Self {
        self.frames.push(frame);
        self
    }

    /// 拼装字节，不产生文件；版本必须恰好为六字节。
    pub fn build(&self) -> Vec<u8> {
        assert_eq!(self.version.len(), 6, "版本必须恰好为 6 字节");
        let mut bytes = self.version.as_bytes().to_vec();
        bytes.extend(self.width.to_le_bytes());
        bytes.extend(self.height.to_le_bytes());
        bytes.extend([packed(self.global_color_table), 0, 0]);
        color_table(&mut bytes, self.global_color_table);
        if let Some(count) = self.loop_count {
            bytes.extend([0x21, 0xff, 11]);
            bytes.extend(b"NETSCAPE2.0");
            bytes.extend([3, 1]);
            bytes.extend(count.to_le_bytes());
            bytes.push(0);
        }
        for frame in &self.frames {
            if let Some(delay) = frame.delay_cs {
                bytes.extend([0x21, 0xf9, 4, 0]);
                bytes.extend(delay.to_le_bytes());
                bytes.extend([0, 0]);
            }
            for extension in &frame.leading_extensions {
                match extension {
                    Extension::PlainText => {
                        // 12 字节头：文本网格位置与尺寸、字符格尺寸、前景与背景色索引。
                        bytes.extend([0x21, 0x01, 12]);
                        bytes.extend([0; 12]);
                        bytes.extend([2, b'A', b'P', 0]);
                    }
                    Extension::Comment => {
                        bytes.extend([0x21, 0xfe, 4]);
                        bytes.extend(b"AP01");
                        bytes.push(0);
                    }
                }
            }
            bytes.push(0x2c);
            for value in [frame.left, frame.top, frame.width, frame.height] {
                bytes.extend(value.to_le_bytes());
            }
            bytes.push(packed(frame.local_color_table));
            color_table(&mut bytes, frame.local_color_table);
            bytes.push(2);
            let mut remaining = frame.data_bytes;
            while remaining > 0 {
                let count = remaining.min(255);
                bytes.push(count as u8);
                bytes.resize(bytes.len() + count, 0);
                remaining -= count;
            }
            bytes.push(0);
        }
        if self.trailer {
            bytes.push(0x3b);
        }
        bytes.extend(&self.trailing_bytes);
        bytes
    }
}

fn packed(table: Option<u8>) -> u8 {
    table
        .map(|n| {
            assert!(n <= 7, "色表大小指数必须在 0 到 7 之间");
            0x80 | n
        })
        .unwrap_or(0)
}

fn color_table(bytes: &mut Vec<u8>, table: Option<u8>) {
    if let Some(n) = table {
        bytes.resize(bytes.len() + 3 * (1 << (n + 1)), 0);
    }
}

/// 额度面板的六帧结构，总时长为 480000 毫秒且不携带循环块。
pub fn quota_gif() -> Vec<u8> {
    let mut builder = GifBuilder {
        global_color_table: Some(0),
        ..GifBuilder::default()
    };
    for delay in [60, 60, 60, 60, 41760, 6000] {
        builder = builder.frame(Frame {
            delay_cs: Some(delay),
            ..Frame::default()
        });
    }
    builder.build()
}
