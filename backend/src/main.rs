use std::collections::HashMap;
use std::sync::{
    atomic::AtomicBool,
    Mutex,
};
use std::sync::Arc;

use dbx_plugin_sdk::{PluginEmitter, PluginError, PluginHandler, PluginMetadata, PluginServer, RequestContext};
use serde_json::{json, Value};

mod session;
mod path;
mod regex;
mod lifecycle;
mod browse;
mod search;
mod tail;
mod download;
mod ssh;
use session::{Session, SshLive};




pub(crate) struct Plugin {
    pub(crate) sessions: Mutex<HashMap<String, Session>>,
    // streamId -> 停止旗标，tail 线程轮询该旗标退出
    pub(crate) tails: Mutex<HashMap<String, Arc<AtomicBool>>>,
    // connectionId -> 复用 SFTP 会话（SSH 模式 connect 建连一次，browse/search/tail/download 共用）
    pub(crate) ssh_lives: Mutex<HashMap<String, SshLive>>,
}

impl Default for Plugin {
    fn default() -> Self {
        Self { sessions: Mutex::new(HashMap::new()),
            tails: Mutex::new(HashMap::new()), ssh_lives: Mutex::new(HashMap::new()) }
    }
}

// 允许的日志扩展名：只展示 *.log 与 nohup.out
pub(crate) const ALLOWED_EXTS: [&str; 2] = ["log", "out"];
// 单次 search 最多返回行数：防 8MB JSON 上限，也防大文件全量进内存
pub(crate) const MAX_RETURN_LINES: usize = 20000;
// 单页上限：截图默认 100，前端可调
pub(crate) const MAX_PAGE_SIZE: usize = 500;
// tail 轮询间隔：本地 inotify 太重，轮询足够且无新依赖
pub(crate) const TAIL_POLL_MS: u64 = 500;

impl PluginHandler for Plugin {
    fn handle(
        &self,
        _context: RequestContext,
        method: &str,
        params: Value,
        emitter: &PluginEmitter,
    ) -> Result<Value, PluginError> {
        match method {
            "connection/test" => self.test(&params),
            "connection/connect" => self.connect(&params),
            "connection/disconnect" => self.disconnect(&params),
            "logs/browse" => self.browse(&params),
            "logs/search" => self.search(&params),
            "logs/tail" => self.tail(&params, emitter),
            "logs/stop" => self.stop(&params),
            "logs/downloadChunk" => self.download_chunk(&params),
            "dbx-logviewer/ping" => Ok(json!({
                "ok": true,
                "plugin": "io.github.yxlonger.logviewer",
                "language": "rust",
            })),
            _ => Err(PluginError::method_not_found(method)),
        }
    }
}








pub(crate) fn now_millis() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

fn main() -> std::io::Result<()> {
    let metadata = PluginMetadata::new("io.github.yxlonger.logviewer", env!("CARGO_PKG_VERSION")).with_capability("connections");
    PluginServer::new(metadata, Plugin::default()).serve()
}
