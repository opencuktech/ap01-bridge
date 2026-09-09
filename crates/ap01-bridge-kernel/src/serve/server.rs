//! 主线程接收连接，四个固定工作线程处理请求。

use super::{
    http::{Request, RequestError, read_request},
    response::{Response, write_response},
};
use crate::{
    KernelError,
    events::Event,
    store::{self, CurrentStatus, FallbackStatus, Serving, ServingStatus},
};
use serde::Serialize;
use std::{
    collections::HashMap,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    net::{Shutdown, SocketAddr, TcpListener, TcpStream},
    panic::{AssertUnwindSafe, catch_unwind},
    path::PathBuf,
    sync::{Arc, Mutex, OnceLock, mpsc},
    thread,
    time::{Duration, Instant},
};

pub struct ServeConfig {
    pub data_dir: PathBuf,
    pub version: String,
}

pub struct Server {
    listener: TcpListener,
    config: ServeConfig,
    sender: mpsc::SyncSender<(TcpStream, Instant)>,
    runtime: Arc<OnceLock<Runtime>>,
}

pub type EventSink = Arc<Mutex<dyn FnMut(&Event) + Send>>;
pub type Clock = Arc<dyn Fn() -> u64 + Send + Sync>;
type Snapshot = (String, Arc<Vec<u8>>);

struct Runtime {
    config: ServeConfig,
    state: Arc<ServeState>,
    clock: Clock,
    sink: EventSink,
}

struct ServeState {
    started_at: u64,
    cache: Mutex<HashMap<String, Arc<Vec<u8>>>>,
    last_served: Mutex<Option<Snapshot>>,
    requests: Mutex<RequestStats>,
}

#[derive(Default, Clone, Serialize)]
struct RequestStats {
    screen_total: u64,
    screen_last_at: Option<u64>,
    screen_last_status: Option<u16>,
    screen_last_client: Option<String>,
}

#[derive(Serialize)]
struct Health<'a> {
    ok: bool,
    serving: Serving,
    now: u64,
    uptime_seconds: u64,
    version: &'a str,
    data_dir: String,
    current: &'a Option<CurrentStatus>,
    fallback: &'a Option<FallbackStatus>,
    error: &'a Option<String>,
    requests: RequestStats,
}

impl ServeState {
    fn new(started_at: u64) -> Self {
        Self {
            started_at,
            cache: Mutex::new(HashMap::new()),
            last_served: Mutex::new(None),
            requests: Mutex::new(RequestStats::default()),
        }
    }

    fn health(&self, config: &ServeConfig, status: &ServingStatus, now: u64) -> Response {
        let health = Health {
            ok: status.serving != Serving::None && status.error.is_none(),
            serving: status.serving,
            now,
            uptime_seconds: now.saturating_sub(self.started_at),
            version: &config.version,
            data_dir: config.data_dir.to_string_lossy().into_owned(),
            current: &status.current,
            fallback: &status.fallback,
            error: &status.error,
            requests: self
                .requests
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .clone(),
        };
        Response {
            status: 200,
            content_type: "application/json",
            body: Arc::new(serde_json::to_vec(&health).expect("健康状态只含可序列化字段")),
            allow: false,
        }
    }

    /// 读取和填充缓存共用锁，防止并发请求重复读取同一哈希。
    fn cached_content(
        &self,
        hash: &str,
        bytes: u64,
        read: &mut impl FnMut(&str) -> io::Result<Vec<u8>>,
    ) -> Option<Arc<Vec<u8>>> {
        let mut cache = self.cache.lock().unwrap_or_else(|error| error.into_inner());
        if let Some(content) = cache.get(hash) {
            return (content.len() as u64 == bytes).then(|| content.clone());
        }
        let content = read(hash).ok()?;
        if content.len() as u64 != bytes || content.len() > 262_144 {
            return None;
        }
        let content = Arc::new(content);
        if cache.len() >= 2 {
            cache.clear();
        }
        cache.insert(hash.into(), content.clone());
        Some(content)
    }

    /// 把解析和读取作为显式函数传入，测试可确定性重现回收夹在两次读取之间。
    fn screen(
        &self,
        status: &mut ServingStatus,
        mut resolve: impl FnMut() -> ServingStatus,
        mut read: impl FnMut(&str) -> io::Result<Vec<u8>>,
    ) -> Option<Snapshot> {
        for attempt in 0..2 {
            if attempt == 1 {
                *status = resolve();
            }
            // 当前内容未过期却不可用，多半是被下一次发布的回收删掉；先重读记录，不先改供回退。
            if attempt == 0
                && status.serving != Serving::Current
                && status.current.as_ref().is_some_and(|c| !c.expired)
            {
                continue;
            }
            let candidate = match status.serving {
                Serving::Current => status.current.as_ref().map(|c| (c.gif.as_str(), c.bytes)),
                Serving::Fallback => status
                    .fallback
                    .as_ref()
                    .and_then(|f| Some((f.gif.as_deref()?, f.bytes?))),
                // 磁盘内容已回收时，未过期记录仍可命中该哈希的内存缓存。
                Serving::None => status
                    .current
                    .as_ref()
                    .filter(|c| !c.expired)
                    .map(|c| (c.gif.as_str(), c.bytes)),
            };
            let Some((hash, bytes)) = candidate else {
                // 过期无可用回退或没有可解析记录必须 404，不能被旧快照掩盖。
                return None;
            };
            if let Some(content) = self.cached_content(hash, bytes, &mut read) {
                let snapshot = (hash.to_owned(), content);
                *self
                    .last_served
                    .lock()
                    .unwrap_or_else(|error| error.into_inner()) = Some(snapshot.clone());
                return Some(snapshot);
            }
        }
        // 沿用旧内容只用于「当前内容未过期却暂时读不到」的恢复；过期后必须 404，不能被旧快照掩盖。
        if !status.current.as_ref().is_some_and(|c| !c.expired) {
            return None;
        }
        self.last_served
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }
}

impl Server {
    pub fn bind(addr: SocketAddr, config: ServeConfig) -> Result<Self, KernelError> {
        let listener = TcpListener::bind(addr)
            .map_err(|error| KernelError::Runtime(format!("无法绑定监听地址 {addr}：{error}")))?;
        let logs = config.data_dir.join("logs");
        fs::create_dir_all(&logs).map_err(|error| {
            KernelError::State(format!("无法创建日志目录 {}：{error}", logs.display()))
        })?;
        // 用与每个请求相同的方式打开一次访问日志，目录不可写、文件不可写或被目录占据都在启动时暴露。
        let access_log = logs.join("access.log");
        OpenOptions::new()
            .append(true)
            .create(true)
            .open(&access_log)
            .map_err(|error| {
                KernelError::State(format!(
                    "无法打开访问日志文件 {}：{error}",
                    access_log.display()
                ))
            })?;
        // 在启动事件之前创建线程，线程创建失败也能走启动失败的统一出口。
        let (sender, receiver) = mpsc::sync_channel::<(TcpStream, Instant)>(4);
        let receiver = Arc::new(Mutex::new(receiver));
        let runtime = Arc::new(OnceLock::<Runtime>::new());
        for index in 0..4 {
            let receiver = receiver.clone();
            let runtime = runtime.clone();
            thread::Builder::new()
                .name(format!("ap01-serve-{index}"))
                .spawn(move || {
                    loop {
                        // 只在领取连接时持有接收锁，网络读写不串行化。
                        let connection = receiver
                            .lock()
                            .unwrap_or_else(|error| error.into_inner())
                            .recv();
                        let Ok((stream, accepted_at)) = connection else {
                            break;
                        };
                        // 单个连接（含注入回调）发生 panic 时丢弃套接字，线程继续领取。
                        // 共享锁允许恢复中毒后的内容，避免一次 panic 逐个耗尽工作线程。
                        let _ = catch_unwind(AssertUnwindSafe(|| {
                            let runtime = runtime.get().expect("分发连接前必须注入服务运行上下文");
                            handle(
                                stream,
                                accepted_at,
                                &runtime.config,
                                &runtime.state,
                                &runtime.clock,
                                &runtime.sink,
                            );
                        }));
                    }
                })
                .map_err(|error| KernelError::Runtime(format!("无法启动服务工作线程：{error}")))?;
        }
        Ok(Self {
            listener,
            config,
            sender,
            runtime,
        })
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.listener.local_addr()
    }

    /// 持续服务直至监听器或工作线程池不可继续使用；不安装信号处理。
    pub fn run(self, clock: Clock, sink: EventSink) -> KernelError {
        let state = Arc::new(ServeState::new(clock()));
        // 有界队列避免慢客户端持续占用无限量的已接收套接字。
        assert!(
            self.runtime
                .set(Runtime {
                    config: self.config,
                    state,
                    clock,
                    sink
                })
                .is_ok(),
            "运行上下文只能初始化一次"
        );
        loop {
            match self.listener.accept() {
                Ok((stream, _)) => {
                    // 排队及有界通道的等待也计入请求头的五秒截止时间。
                    let accepted_at = Instant::now();
                    if self.sender.send((stream, accepted_at)).is_err() {
                        return KernelError::Runtime("服务工作线程池已停止".into());
                    }
                }
                Err(error) if fatal_accept_error(&error) => {
                    return KernelError::Runtime(format!("监听服务无法继续接收连接：{error}"));
                }
                Err(_) => thread::sleep(Duration::from_millis(50)),
            }
        }
    }
}

fn fatal_accept_error(error: &io::Error) -> bool {
    if matches!(
        error.kind(),
        io::ErrorKind::InvalidInput | io::ErrorKind::PermissionDenied | io::ErrorKind::Unsupported
    ) {
        return true;
    }
    // 无效句柄或非套接字不可恢复；资源暂时耗尽（例如 EMFILE）应重试。
    #[cfg(unix)]
    if error.raw_os_error() == Some(9) {
        return true;
    }
    #[cfg(target_os = "macos")]
    if error.raw_os_error() == Some(38) {
        return true;
    }
    #[cfg(target_os = "linux")]
    if error.raw_os_error() == Some(88) {
        return true;
    }
    #[cfg(windows)]
    if matches!(error.raw_os_error(), Some(6 | 10038)) {
        return true;
    }
    false
}

fn handle(
    mut stream: TcpStream,
    accepted_at: Instant,
    config: &ServeConfig,
    state: &ServeState,
    clock: &Clock,
    sink: &EventSink,
) {
    let deadline = accepted_at + Duration::from_secs(5);
    let timeout = Some(Duration::from_secs(5));
    if stream.set_write_timeout(timeout).is_err() {
        close_connection(stream, deadline);
        return;
    }
    let client = stream
        .peer_addr()
        .map(|addr| addr.ip().to_string())
        .unwrap_or_default();
    let parsed = read_request(&mut stream, deadline, |stream, remaining| {
        stream.set_read_timeout(Some(remaining))
    });
    if matches!(parsed, Err(RequestError::Timeout)) {
        // 超时直接关闭，不再尝试写 400，避免写阶段再次占用已耗尽时限的线程。
        close_connection(stream, deadline);
        return;
    }
    let now = clock();
    let mut status = store::resolve_status(&config.data_dir, now);
    let mut gif = None;
    let (request, response) = match parsed {
        Err(_) => (
            Request {
                method: String::new(),
                path: String::new(),
                version: String::new(),
            },
            Response::text(400),
        ),
        Ok(request) => {
            let response = if !matches!(request.method.as_str(), "GET" | "HEAD") {
                Response::text(405)
            } else if request.path == "/health" {
                state.health(config, &status, now)
            } else if request.path == "/screen.gif" {
                match state.screen(
                    &mut status,
                    || store::resolve_status(&config.data_dir, now),
                    |hash| {
                        let mut bytes = Vec::new();
                        File::open(store::content_path(&config.data_dir, hash))?
                            .take(262_145)
                            .read_to_end(&mut bytes)?;
                        Ok(bytes)
                    },
                ) {
                    Some((hash, body)) => {
                        gif = Some(hash);
                        Response {
                            status: 200,
                            content_type: "image/gif",
                            body,
                            allow: false,
                        }
                    }
                    None => Response::text(404),
                }
            } else {
                Response::text(404)
            };
            (request, response)
        }
    };
    let bytes = write_response(
        &mut stream,
        &response,
        request.method == "HEAD",
        &config.version,
    );
    if request.path == "/screen.gif" && matches!(request.method.as_str(), "GET" | "HEAD") {
        let mut stats = state
            .requests
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        stats.screen_total = stats.screen_total.saturating_add(1);
        stats.screen_last_at = Some(now);
        stats.screen_last_status = Some(response.status);
        stats.screen_last_client = Some(client.clone());
    }
    if !(matches!(request.method.as_str(), "GET" | "HEAD") && request.path == "/health") {
        let event = Event::Request {
            ts: now,
            client,
            method: request.method,
            path: request.path,
            version: request.version,
            status: response.status,
            bytes: bytes as u64,
            serving: match status.serving {
                Serving::Current => "current",
                Serving::Fallback => "fallback",
                Serving::None => "none",
            }
            .into(),
            gif,
        };
        // 共用事件出口锁防止多线程交错写入日志行；输出完成再关闭连接。
        let mut sink = sink.lock().unwrap_or_else(|error| error.into_inner());
        if let Ok(mut line) = serde_json::to_vec(&event) {
            line.push(b'\n');
            if let Ok(mut log) = OpenOptions::new()
                .append(true)
                .create(true)
                .open(config.data_dir.join("logs/access.log"))
            {
                let _ = log.write_all(&line);
            }
        }
        sink(&event);
    }
    close_connection(stream, deadline);
}

fn close_connection(mut stream: TcpStream, deadline: Instant) {
    // 接收缓冲区仍有未读数据时直接关闭套接字会触发 RST，客户端可能先收到
    // 连接重置而读不到已写出的响应。先关闭写端发出 FIN，再丢弃残余数据，
    // 等对端读完响应并关闭后读到 EOF。排空总计最多一秒且不超过连接截止时间，
    // 每次读取都重算剩余超时，并最多丢弃 64 KiB，避免滴流或持续发送无限占用线程。
    let _ = stream.shutdown(Shutdown::Write);
    let drain_deadline = deadline.min(Instant::now() + Duration::from_secs(1));
    let mut buffer = [0; 4096];
    let mut discarded = 0;
    while discarded < 64 * 1024 {
        let remaining = drain_deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() || stream.set_read_timeout(Some(remaining)).is_err() {
            break;
        }
        let limit = buffer.len().min(64 * 1024 - discarded);
        match stream.read(&mut buffer[..limit]) {
            Ok(0) | Err(_) => break,
            Ok(count) => discarded += count,
        }
    }
    drop(stream);
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
