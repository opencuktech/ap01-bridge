//! 长任务的时钟注入与逐行事件输出。

use ap01_bridge_kernel::{
    KernelError,
    events::Event,
    serve::{
        ServeConfig, Server,
        server::{Clock, EventSink},
    },
};
use std::{
    io::{self, Write},
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

pub(super) fn run(bind: IpAddr, port: u16, data_dir: PathBuf, json_mode: bool) -> KernelError {
    let result = start(bind, port, data_dir, json_mode);
    match result {
        Ok(error) | Err(error) => error,
    }
}

fn start(
    bind: IpAddr,
    port: u16,
    data_dir: PathBuf,
    json_mode: bool,
) -> Result<KernelError, KernelError> {
    // 保留存储命令的调试时钟约定；运行期间时钟回拨至 epoch 以前则按 0 处理。
    super::store_commands::now()?;
    let clock: Clock = Arc::new(|| super::store_commands::now().unwrap_or(0));
    let server = Server::bind(
        SocketAddr::new(bind, port),
        ServeConfig {
            data_dir: data_dir.clone(),
            version: env!("CARGO_PKG_VERSION").into(),
        },
    )?;
    let addr = server
        .local_addr()
        .map_err(|error| KernelError::Runtime(format!("无法获取实际监听地址：{error}")))?;
    let sink: EventSink = Arc::new(Mutex::new(move |event: &Event| {
        let mut stdout = io::stdout().lock();
        if json_mode {
            if let Ok(line) = serde_json::to_string(event) {
                let _ = writeln!(stdout, "{line}");
            }
        } else {
            let _ = writeln!(stdout, "{}", event.human_line());
        }
        let _ = stdout.flush();
    }));
    sink.lock().unwrap_or_else(|error| error.into_inner())(&listening_event(addr, &data_dir));
    Ok(server.run(clock, sink))
}

fn listening_event(addr: SocketAddr, data_dir: &Path) -> Event {
    Event::Listening {
        bind: addr.ip().to_string(),
        port: addr.port(),
        data_dir: data_dir.to_string_lossy().into_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Cli, Commands};
    use clap::Parser;

    #[test]
    fn default_serve_arguments_produce_listening_event_without_binding() {
        let Commands::Serve { bind, port } =
            Cli::try_parse_from(["bridge", "serve"]).unwrap().command
        else {
            panic!("应解析为服务命令");
        };
        let event = listening_event(SocketAddr::new(bind, port), Path::new("data"));
        let Event::Listening { bind, port, .. } = &event else {
            panic!("应构造监听事件");
        };
        assert_eq!(bind, "0.0.0.0");
        assert_eq!(*port, 8765);
        assert!(
            serde_json::to_string(&event)
                .unwrap()
                .contains("\"event\":\"listening\"")
        );
    }
}
