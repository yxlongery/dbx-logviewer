use crate::path::{is_gzip_log_name, safe_join};
use crate::session::{rt, Session, SshLive};
use crate::ssh::{remote_resolve, ssh_live_of};
use crate::Plugin;
use dbx_plugin_sdk::PluginError;
use flate2::bufread::MultiGzDecoder;
use serde_json::Value;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read};
use tokio::io::{AsyncRead, AsyncReadExt};

pub(crate) struct LogFile<'a> {
    name: &'a str,
    path: String,
    live: Option<SshLive>,
    gzip: bool,
}

impl<'a> LogFile<'a> {
    pub(crate) fn resolve(
        plugin: &Plugin,
        session: &Session,
        params: &'a Value,
    ) -> Result<Self, PluginError> {
        let name = params
            .get("file")
            .and_then(Value::as_str)
            .ok_or_else(|| PluginError::new(-32602, "Missing file"))?
            .trim();
        let live = if session.ssh.is_some() {
            Some(ssh_live_of(plugin, params, session)?)
        } else {
            None
        };
        let path = match &live {
            Some(live) => rt().block_on(remote_resolve(live, session, name))?,
            None => safe_join(session, name)?,
        };
        Ok(Self {
            name,
            path,
            live,
            gzip: is_gzip_log_name(name),
        })
    }

    pub(crate) fn open(&self) -> Result<LogLines<'_>, PluginError> {
        let open_error =
            |err| PluginError::new(-32000, format!("无法打开文件 {}：{err}", self.name));
        let reader = match &self.live {
            Some(live) => {
                let file = rt()
                    .block_on(live.sftp.open(&self.path))
                    .map_err(|err| open_error(format!("{err:?}")))?;
                log_reader(BlockingReader(file), self.gzip)
            }
            None => {
                let file = File::open(&self.path).map_err(|err| open_error(err.to_string()))?;
                log_reader(file, self.gzip)
            }
        };
        Ok(LogLines {
            name: self.name,
            reader,
            number: 0,
            skip_invalid_utf8: !self.gzip && self.live.is_none(),
        })
    }
}

// RPC 处理器是同步的。每次只等待一个缓冲块，SFTP 和解压都无需读完整个归档。
struct BlockingReader<R>(R);

impl<R: AsyncRead + Unpin> Read for BlockingReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        rt().block_on(self.0.read(buf))
    }
}

fn log_reader(reader: impl Read + 'static, gzip: bool) -> Box<dyn BufRead> {
    if gzip {
        let compressed = BufReader::with_capacity(64 * 1024, reader);
        Box::new(BufReader::new(MultiGzDecoder::new(compressed)))
    } else {
        Box::new(BufReader::new(reader))
    }
}

pub(crate) struct LogLines<'a> {
    name: &'a str,
    reader: Box<dyn BufRead>,
    number: usize,
    skip_invalid_utf8: bool,
}

impl LogLines<'_> {
    pub(crate) fn next_line(&mut self) -> Result<Option<(usize, String)>, PluginError> {
        let mut bytes = Vec::new();
        let read_error =
            |err| PluginError::new(-32000, format!("读取文件失败 {}：{err}", self.name));
        loop {
            if self
                .reader
                .read_until(b'\n', &mut bytes)
                .map_err(|err| read_error(err.to_string()))?
                == 0
            {
                return Ok(None);
            }
            self.number += 1;
            if bytes.last() == Some(&b'\n') {
                bytes.pop();
                if bytes.last() == Some(&b'\r') {
                    bytes.pop();
                }
            }
            match String::from_utf8(bytes) {
                Ok(text) => return Ok(Some((self.number, text))),
                Err(err) if self.skip_invalid_utf8 => {
                    bytes = err.into_bytes();
                    bytes.clear();
                }
                Err(err) => return Err(read_error(err.to_string())),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::path::root_name;
    use crate::session::test_session;
    use flate2::{write::GzEncoder, Compression};
    use russh_sftp::protocol::{Data, FileAttributes, Handle, OpenFlags, Status, StatusCode};
    use russh_sftp::{client::SftpSession, server};
    use serde_json::json;
    use std::io::Write;
    use std::sync::Arc;

    fn gzip(data: &[u8]) -> Vec<u8> {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(data).unwrap();
        encoder.finish().unwrap()
    }

    #[test]
    fn gzip_logs_support_browse_search_context_and_chunked_download() {
        let base = std::env::temp_dir().join(format!("dbx-logviewer-gzip-{}", std::process::id()));
        std::fs::create_dir(&base).unwrap();
        let archive = base.join("app.log.1.gz");
        let bytes = [
            gzip(b"INFO start\r\nERROR one\n"),
            gzip(b"INFO between\nERROR two"),
        ]
        .concat();
        std::fs::write(&archive, &bytes).unwrap();
        std::fs::write(base.join("damaged.log.gz"), b"not a gzip file").unwrap();
        let root = root_name(base.to_str().unwrap());
        let file = format!("{root}/app.log.1.gz");
        let plugin = Plugin::default();
        plugin.sessions.lock().unwrap().insert(
            "gzip".into(),
            test_session(vec![base.to_str().unwrap().into()]),
        );

        let browse = plugin
            .browse(&json!({ "connectionId": "gzip", "dir": root }))
            .unwrap();
        assert_eq!(browse["entries"][0]["name"], file);
        assert_eq!(browse["entries"][0]["size"], bytes.len());
        let search = plugin.search(&json!({
            "connectionId": "gzip", "file": file, "keyword": "ERROR", "pageSize": 1, "page": 2, "context": 1
        })).unwrap();
        assert_eq!(search["total"], 2);
        assert_eq!(
            search["lines"],
            json!([
                { "no": 3, "text": "INFO between", "match": false },
                { "no": 2, "text": "ERROR one", "match": true },
                { "no": 1, "text": "INFO start", "match": false }
            ])
        );
        let download = plugin
            .download_chunk(&json!({
                "connectionId": "gzip", "file": file, "level": "ERROR", "offset": 1, "length": 2
            }))
            .unwrap();
        assert_eq!(
            download,
            json!({ "lines": ["ERROR two"], "nextOffset": 2, "eof": true })
        );
        assert!(plugin
            .search(&json!({
                "connectionId": "gzip", "file": format!("{root}/damaged.log.gz")
            }))
            .is_err());
        assert_eq!(std::fs::read(&archive).unwrap(), bytes);
        std::fs::remove_dir_all(base).unwrap();
    }

    struct SftpArchive(Vec<u8>);

    impl server::Handler for SftpArchive {
        type Error = StatusCode;

        fn unimplemented(&self) -> StatusCode {
            StatusCode::OpUnsupported
        }

        async fn open(
            &mut self,
            id: u32,
            path: String,
            flags: OpenFlags,
            _: FileAttributes,
        ) -> Result<Handle, StatusCode> {
            if path != "/logs/app.log.gz" || flags.bits() != OpenFlags::READ.bits() {
                return Err(StatusCode::PermissionDenied);
            }
            Ok(Handle { id, handle: path })
        }

        async fn read(
            &mut self,
            id: u32,
            _: String,
            offset: u64,
            len: u32,
        ) -> Result<Data, StatusCode> {
            let start = offset as usize;
            if start >= self.0.len() {
                return Err(StatusCode::Eof);
            }
            let end = (start + len.min(8) as usize).min(self.0.len());
            Ok(Data {
                id,
                data: self.0[start..end].to_vec(),
            })
        }

        async fn close(&mut self, id: u32, _: String) -> Result<Status, StatusCode> {
            Ok(Status {
                id,
                status_code: StatusCode::Ok,
                error_message: String::new(),
                language_tag: String::new(),
            })
        }
    }

    #[test]
    fn gzip_reads_a_fragmented_sftp_file_in_read_only_mode() {
        let sftp = rt().block_on(async {
            let (client, server) = tokio::io::duplex(4096);
            server::run(server, SftpArchive(gzip(b"first\nsecond\n"))).await;
            SftpSession::new(client).await.unwrap()
        });
        let file = LogFile {
            name: "logs/app.log.gz",
            path: "/logs/app.log.gz".into(),
            live: Some(SshLive {
                sftp: Arc::new(sftp),
            }),
            gzip: true,
        };
        let mut lines = file.open().unwrap();
        assert_eq!(lines.next_line().unwrap(), Some((1, "first".into())));
        assert_eq!(lines.next_line().unwrap(), Some((2, "second".into())));
        assert_eq!(lines.next_line().unwrap(), None);
    }

    #[test]
    fn gzip_reports_a_truncated_archive() {
        let mut bytes = gzip(b"one\ntwo\n");
        bytes.pop();
        let mut reader = log_reader(io::Cursor::new(bytes), true);
        assert!(reader.read_to_end(&mut Vec::new()).is_err());
    }
}
