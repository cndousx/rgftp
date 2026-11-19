use crate::model::{AuthMethod, ConnectionProfile, FileEntry, KnownHost, KnownHosts, sort_entries};
use crate::util::{expand_tilde, is_hidden, join_remote};
use async_channel::Sender;
use russh_sftp::client::SftpSession;
use russh_sftp::protocol::{FileAttributes, OpenFlags};
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Upload,
    Download,
}

impl Direction {
    pub fn label(self) -> &'static str {
        match self {
            Self::Upload => "上传",
            Self::Download => "下载",
        }
    }
}

#[derive(Clone, Debug)]
pub enum BackendEvent {
    Connected {
        profile_id: String,
        home: String,
    },
    ConnectFailed {
        profile_id: String,
        error: String,
    },
    /// The server presented a host key that is unknown or has changed.
    ///
    /// The worker waits for [`HostKeyDecision`] before the SSH handshake can
    /// continue, so the UI must always answer this request.
    HostKeyVerification {
        profile_id: String,
        request_id: u64,
        host: String,
        port: u16,
        key_type: String,
        fingerprint: String,
        /// Fingerprint of the key that was trusted before, when it changed.
        previous_fingerprint: Option<String>,
    },
    Listed {
        session_id: String,
        path: String,
        entries: Vec<FileEntry>,
        /// Set when the requested path was a file and `path` is its parent
        /// directory instead, so the UI can explain the redirect.
        file_redirect: Option<String>,
    },
    ListFailed {
        session_id: String,
        path: String,
        error: String,
    },
    Disconnected {
        session_id: String,
        message: String,
    },
    TransferStarted {
        session_id: String,
        id: u64,
        name: String,
        direction: Direction,
        total: u64,
    },
    TransferTotal {
        id: u64,
        total: u64,
    },
    TransferProgress {
        id: u64,
        transferred: u64,
    },
    TransferDone {
        session_id: String,
        id: u64,
        name: String,
        direction: Direction,
        ok: bool,
        cancelled: bool,
        message: String,
    },
    /// Result of a remote rename / delete / chmod.
    FileOpFinished {
        session_id: String,
        ok: bool,
        message: String,
    },
    /// Detailed `stat` information for one remote path.
    FileStat {
        path: String,
        result: Result<RemoteFileInfo, String>,
    },
    Error {
        message: String,
    },
}

/// Everything useful the SFTP server tells us about a single path.
#[derive(Clone, Debug, Default)]
pub struct RemoteFileInfo {
    pub path: String,
    pub name: String,
    pub kind: String,
    pub size: u64,
    pub permissions: Option<u32>,
    pub uid: Option<u32>,
    pub user: Option<String>,
    pub gid: Option<u32>,
    pub group: Option<String>,
    pub accessed: Option<i64>,
    pub modified: Option<i64>,
}

#[derive(Debug)]
pub enum BackendCommand {
    Connect {
        profile: ConnectionProfile,
    },
    List {
        session_id: String,
        path: String,
    },
    /// Resolves a user-typed path: directories are listed directly, while a
    /// file path opens and lists its parent directory.
    Locate {
        session_id: String,
        path: String,
    },
    Upload {
        session_id: String,
        local: PathBuf,
        remote: String,
        display_name: String,
    },
    Download {
        session_id: String,
        remote: String,
        local: PathBuf,
        display_name: String,
    },
    Disconnect {
        session_id: String,
    },
    RenameRemote {
        session_id: String,
        from: String,
        to: String,
    },
    DeleteRemote {
        session_id: String,
        path: String,
    },
    ChmodRemote {
        session_id: String,
        path: String,
        mode: u32,
    },
    StatRemote {
        session_id: String,
        path: String,
    },
}

/// How the user answered a [`BackendEvent::HostKeyVerification`] prompt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostKeyDecision {
    /// Accept the key, saving it to the known-hosts list when `remember`.
    Trust { remember: bool },
    /// Abort the connection because the key was not trusted.
    Reject,
}

struct HostKeyReply {
    request_id: u64,
    decision: HostKeyDecision,
}

pub struct SftpWorker {
    tx: tokio::sync::mpsc::UnboundedSender<BackendCommand>,
    host_key_tx: tokio::sync::mpsc::UnboundedSender<HostKeyReply>,
    cancel: Arc<AtomicBool>,
}

impl SftpWorker {
    pub fn spawn(events: Sender<BackendEvent>) -> Self {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<BackendCommand>();
        let (host_key_tx, host_key_rx) = tokio::sync::mpsc::unbounded_channel::<HostKeyReply>();
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = cancel.clone();

        std::thread::Builder::new()
            .name("rgftp-sftp".to_string())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .build();

                let Ok(runtime) = runtime else {
                    let _ = events.send_blocking(BackendEvent::Error {
                        message: "无法启动网络运行时".to_string(),
                    });
                    return;
                };

                runtime.block_on(async move {
                    let mut clients: HashMap<String, Arc<Client>> = HashMap::new();
                    let mut next_transfer_id = 1u64;
                    let mut next_host_key_request = 1u64;
                    // Pending host-key prompts, keyed by request id. UI answers
                    // arrive on their own channel because the command loop is
                    // blocked inside `connect_client` while a key is checked.
                    let pending_host_keys: Arc<
                        tokio::sync::Mutex<
                            HashMap<u64, tokio::sync::oneshot::Sender<HostKeyDecision>>,
                        >,
                    > = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
                    {
                        let pending_host_keys = pending_host_keys.clone();
                        let mut host_key_rx = host_key_rx;
                        tokio::spawn(async move {
                            while let Some(reply) = host_key_rx.recv().await {
                                let sender =
                                    pending_host_keys.lock().await.remove(&reply.request_id);
                                if let Some(sender) = sender {
                                    let _ = sender.send(reply.decision);
                                }
                            }
                        });
                    }

                    // Poll the SSH handles so a server-side disconnect is
                    // noticed even while the command queue is idle. Ticks are
                    // skipped while a command (e.g. a large transfer) is
                    // running; the failed command reports the error instead.
                    let mut health_check = tokio::time::interval(std::time::Duration::from_secs(2));
                    health_check.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

                    loop {
                        let command = tokio::select! {
                            command = rx.recv() => {
                                let Some(command) = command else {
                                    break;
                                };
                                command
                            }
                            _ = health_check.tick() => {
                                let disconnected: Vec<String> = clients
                                    .iter()
                                    .filter(|(_, client)| client.session.is_closed())
                                    .map(|(session_id, _)| session_id.clone())
                                    .collect();
                                for session_id in disconnected {
                                    clients.remove(&session_id);
                                    let _ = events
                                        .send(BackendEvent::Disconnected {
                                            session_id,
                                            message: "服务端已断开连接".to_string(),
                                        })
                                        .await;
                                }
                                continue;
                            }
                        };

                        match command {
                            BackendCommand::Connect { profile } => {
                                clients.remove(&profile.id);

                                let request_id = next_host_key_request;
                                next_host_key_request += 1;
                                let (decision_tx, decision_rx) = tokio::sync::oneshot::channel();
                                pending_host_keys
                                    .lock()
                                    .await
                                    .insert(request_id, decision_tx);
                                let handler = ClientHandler {
                                    profile_id: profile.id.clone(),
                                    host: profile.host.clone(),
                                    port: profile.port,
                                    events: events.clone(),
                                    request_id,
                                    decision: Some(decision_rx),
                                };

                                let result = connect_client(&profile, handler).await;
                                // Neither branch needs the slot anymore: either
                                // the key was answered, or the connection failed
                                // before asking.
                                pending_host_keys.lock().await.remove(&request_id);

                                match result {
                                    Ok(new_client) => {
                                        let requested = if profile.remote_dir.trim().is_empty() {
                                            ".".to_string()
                                        } else {
                                            profile.remote_dir.trim().to_string()
                                        };
                                        let home = match new_client
                                            .sftp
                                            .canonicalize(requested.clone())
                                            .await
                                        {
                                            Ok(path) => path,
                                            Err(_) => new_client
                                                .sftp
                                                .canonicalize(".")
                                                .await
                                                .unwrap_or_else(|_| "/".to_string()),
                                        };

                                        let _ = events
                                            .send(BackendEvent::Connected {
                                                profile_id: profile.id.clone(),
                                                home: home.clone(),
                                            })
                                            .await;

                                        match list_dir(&new_client.sftp, &home).await {
                                            Ok((path, entries)) => {
                                                let _ = events
                                                    .send(BackendEvent::Listed {
                                                        session_id: profile.id.clone(),
                                                        path,
                                                        entries,
                                                        file_redirect: None,
                                                    })
                                                    .await;
                                            }
                                            Err(error) => {
                                                let _ = events
                                                    .send(BackendEvent::ListFailed {
                                                        session_id: profile.id.clone(),
                                                        path: home,
                                                        error,
                                                    })
                                                    .await;
                                            }
                                        }

                                        clients.insert(profile.id.clone(), Arc::new(new_client));
                                    }
                                    Err(error) => {
                                        let _ = events
                                            .send(BackendEvent::ConnectFailed {
                                                profile_id: profile.id,
                                                error,
                                            })
                                            .await;
                                    }
                                }
                            }
                            BackendCommand::List { session_id, path } => {
                                let Some(active) = clients.get(&session_id) else {
                                    let _ = events
                                        .send(BackendEvent::ListFailed {
                                            session_id,
                                            path,
                                            error: "尚未连接服务器".to_string(),
                                        })
                                        .await;
                                    continue;
                                };

                                match list_dir(&active.sftp, &path).await {
                                    Ok((path, entries)) => {
                                        let _ = events
                                            .send(BackendEvent::Listed {
                                                session_id,
                                                path,
                                                entries,
                                                file_redirect: None,
                                            })
                                            .await;
                                    }
                                    Err(error) => {
                                        let _ = events
                                            .send(BackendEvent::ListFailed {
                                                session_id,
                                                path,
                                                error,
                                            })
                                            .await;
                                    }
                                }
                            }
                            BackendCommand::Locate { session_id, path } => {
                                let Some(active) = clients.get(&session_id) else {
                                    let _ = events
                                        .send(BackendEvent::ListFailed {
                                            session_id,
                                            path,
                                            error: "尚未连接服务器".to_string(),
                                        })
                                        .await;
                                    continue;
                                };

                                match locate_remote(&active.sftp, &path).await {
                                    Ok((path, entries, file_redirect)) => {
                                        let _ = events
                                            .send(BackendEvent::Listed {
                                                session_id,
                                                path,
                                                entries,
                                                file_redirect,
                                            })
                                            .await;
                                    }
                                    Err(error) => {
                                        let _ = events
                                            .send(BackendEvent::ListFailed {
                                                session_id,
                                                path,
                                                error,
                                            })
                                            .await;
                                    }
                                }
                            }
                            BackendCommand::Upload {
                                session_id,
                                local,
                                remote,
                                display_name,
                            } => {
                                let id = next_transfer_id;
                                next_transfer_id += 1;

                                let Some(active) = clients.get(&session_id) else {
                                    let _ = events
                                        .send(BackendEvent::TransferStarted {
                                            session_id: session_id.clone(),
                                            id,
                                            name: display_name.clone(),
                                            direction: Direction::Upload,
                                            total: 0,
                                        })
                                        .await;
                                    emit_transfer_done(
                                        &events,
                                        session_id,
                                        id,
                                        display_name,
                                        Direction::Upload,
                                        Err("尚未连接服务器".to_string()),
                                        false,
                                    )
                                    .await;
                                    continue;
                                };
                                let active = Arc::clone(active);

                                worker_cancel.store(false, Ordering::SeqCst);

                                // Transfers run on their own task so the command
                                // loop keeps serving other sessions. Without this
                                // a large upload to one server would block lists and
                                // disconnects for every other server.
                                let events = events.clone();
                                let worker_cancel = Arc::clone(&worker_cancel);
                                tokio::spawn(async move {
                                    let _ = events
                                        .send(BackendEvent::TransferStarted {
                                            session_id: session_id.clone(),
                                            id,
                                            name: display_name.clone(),
                                            direction: Direction::Upload,
                                            total: 0,
                                        })
                                        .await;

                                    let Some(total) = local_total(&local, &worker_cancel) else {
                                        emit_transfer_done(
                                            &events,
                                            session_id,
                                            id,
                                            display_name,
                                            Direction::Upload,
                                            Err("已取消".to_string()),
                                            true,
                                        )
                                        .await;
                                        worker_cancel.store(false, Ordering::SeqCst);
                                        return;
                                    };

                                    let _ = events
                                        .send(BackendEvent::TransferTotal { id, total })
                                        .await;

                                    let mut transferred = 0u64;
                                    let mut last_emit = 0u64;
                                    let result = upload_path(
                                        &active.sftp,
                                        &local,
                                        &remote,
                                        &events,
                                        id,
                                        &mut transferred,
                                        &mut last_emit,
                                        &worker_cancel,
                                    )
                                    .await;

                                    let _ = events
                                        .send(BackendEvent::TransferProgress { id, transferred })
                                        .await;

                                    let cancelled =
                                        result.is_err() && worker_cancel.load(Ordering::SeqCst);
                                    emit_transfer_done(
                                        &events,
                                        session_id,
                                        id,
                                        display_name,
                                        Direction::Upload,
                                        result,
                                        cancelled,
                                    )
                                    .await;
                                    worker_cancel.store(false, Ordering::SeqCst);
                                });
                            }
                            BackendCommand::Download {
                                session_id,
                                remote,
                                local,
                                display_name,
                            } => {
                                let id = next_transfer_id;
                                next_transfer_id += 1;

                                let Some(active) = clients.get(&session_id) else {
                                    let _ = events
                                        .send(BackendEvent::TransferStarted {
                                            session_id: session_id.clone(),
                                            id,
                                            name: display_name.clone(),
                                            direction: Direction::Download,
                                            total: 0,
                                        })
                                        .await;
                                    emit_transfer_done(
                                        &events,
                                        session_id,
                                        id,
                                        display_name,
                                        Direction::Download,
                                        Err("尚未连接服务器".to_string()),
                                        false,
                                    )
                                    .await;
                                    continue;
                                };
                                let active = Arc::clone(active);

                                worker_cancel.store(false, Ordering::SeqCst);

                                let events = events.clone();
                                let worker_cancel = Arc::clone(&worker_cancel);
                                tokio::spawn(async move {
                                    let _ = events
                                        .send(BackendEvent::TransferStarted {
                                            session_id: session_id.clone(),
                                            id,
                                            name: display_name.clone(),
                                            direction: Direction::Download,
                                            total: 0,
                                        })
                                        .await;

                                    let Some(total) =
                                        remote_total(&active.sftp, &remote, &worker_cancel).await
                                    else {
                                        emit_transfer_done(
                                            &events,
                                            session_id,
                                            id,
                                            display_name,
                                            Direction::Download,
                                            Err("已取消".to_string()),
                                            true,
                                        )
                                        .await;
                                        worker_cancel.store(false, Ordering::SeqCst);
                                        return;
                                    };

                                    let _ = events
                                        .send(BackendEvent::TransferTotal { id, total })
                                        .await;

                                    let mut transferred = 0u64;
                                    let mut last_emit = 0u64;
                                    let result = download_path(
                                        &active.sftp,
                                        &remote,
                                        &local,
                                        &events,
                                        id,
                                        &mut transferred,
                                        &mut last_emit,
                                        &worker_cancel,
                                    )
                                    .await;

                                    let _ = events
                                        .send(BackendEvent::TransferProgress { id, transferred })
                                        .await;

                                    let cancelled =
                                        result.is_err() && worker_cancel.load(Ordering::SeqCst);
                                    emit_transfer_done(
                                        &events,
                                        session_id,
                                        id,
                                        display_name,
                                        Direction::Download,
                                        result,
                                        cancelled,
                                    )
                                    .await;
                                    worker_cancel.store(false, Ordering::SeqCst);
                                });
                            }
                            BackendCommand::Disconnect { session_id } => {
                                let had_client = clients.remove(&session_id).is_some();
                                let _ = events
                                    .send(BackendEvent::Disconnected {
                                        session_id,
                                        message: if had_client {
                                            "已断开连接".to_string()
                                        } else {
                                            "当前没有活动连接".to_string()
                                        },
                                    })
                                    .await;
                            }
                            BackendCommand::RenameRemote {
                                session_id,
                                from,
                                to,
                            } => {
                                let message = match clients.get(&session_id) {
                                    Some(active) => match active.sftp.rename(from, to).await {
                                        Ok(()) => ("已重命名".to_string(), true),
                                        Err(error) => (format!("重命名失败：{error}"), false),
                                    },
                                    None => ("尚未连接服务器".to_string(), false),
                                };
                                let _ = events
                                    .send(BackendEvent::FileOpFinished {
                                        session_id,
                                        ok: message.1,
                                        message: message.0,
                                    })
                                    .await;
                            }
                            BackendCommand::DeleteRemote { session_id, path } => {
                                let message = match clients.get(&session_id) {
                                    Some(active) => {
                                        match delete_remote_path(&active.sftp, &path).await {
                                            Ok(()) => ("已删除".to_string(), true),
                                            Err(error) => (format!("删除失败：{error}"), false),
                                        }
                                    }
                                    None => ("尚未连接服务器".to_string(), false),
                                };
                                let _ = events
                                    .send(BackendEvent::FileOpFinished {
                                        session_id,
                                        ok: message.1,
                                        message: message.0,
                                    })
                                    .await;
                            }
                            BackendCommand::ChmodRemote {
                                session_id,
                                path,
                                mode,
                            } => {
                                let message = match clients.get(&session_id) {
                                    Some(active) => {
                                        let attributes = FileAttributes {
                                            permissions: Some(mode),
                                            ..Default::default()
                                        };
                                        match active.sftp.set_metadata(path, attributes).await {
                                            Ok(()) => ("权限已修改".to_string(), true),
                                            Err(error) => (format!("修改权限失败：{error}"), false),
                                        }
                                    }
                                    None => ("尚未连接服务器".to_string(), false),
                                };
                                let _ = events
                                    .send(BackendEvent::FileOpFinished {
                                        session_id,
                                        ok: message.1,
                                        message: message.0,
                                    })
                                    .await;
                            }
                            BackendCommand::StatRemote { session_id, path } => {
                                let result = match clients.get(&session_id) {
                                    Some(active) => {
                                        match active.sftp.metadata(path.clone()).await {
                                            Ok(attributes) => {
                                                Ok(RemoteFileInfo::new(path.clone(), attributes))
                                            }
                                            Err(error) => Err(format!("读取文件信息失败：{error}")),
                                        }
                                    }
                                    None => Err("尚未连接服务器".to_string()),
                                };
                                let _ = events.send(BackendEvent::FileStat { path, result }).await;
                            }
                        }
                    }
                });
            })
            .expect("failed to spawn SFTP worker thread");

        Self {
            tx,
            host_key_tx,
            cancel,
        }
    }

    pub fn send(&self, command: BackendCommand) {
        let _ = self.tx.send(command);
    }

    /// Answers a pending [`BackendEvent::HostKeyVerification`] prompt.
    ///
    /// This uses its own channel because the worker is blocked waiting for the
    /// decision while the command queue is paused.
    pub fn respond_host_key(&self, request_id: u64, decision: HostKeyDecision) {
        let _ = self.host_key_tx.send(HostKeyReply {
            request_id,
            decision,
        });
    }

    /// Ask the active upload/download to stop at the next chunk boundary.
    ///
    /// This is deliberately separate from [`Self::send`]: the worker is busy
    /// while transferring, so cancellation must not wait in the command queue.
    pub fn cancel_current_transfer(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }
}

impl RemoteFileInfo {
    fn new(path: String, attributes: FileAttributes) -> Self {
        let file_type = attributes.file_type();
        let kind = if file_type.is_dir() {
            "目录"
        } else if file_type.is_symlink() {
            "符号链接"
        } else if file_type.is_file() {
            "文件"
        } else {
            "其他"
        };
        let name = path
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .filter(|name| !name.is_empty())
            .unwrap_or(path.as_str())
            .to_string();

        Self {
            path,
            name,
            kind: kind.to_string(),
            size: attributes.size.unwrap_or_default(),
            permissions: attributes.permissions,
            uid: attributes.uid,
            user: attributes.user,
            gid: attributes.gid,
            group: attributes.group,
            accessed: attributes.atime.map(|time| time as i64),
            modified: attributes.mtime.map(|time| time as i64),
        }
    }
}

struct Client {
    // Dropped before the SSH session so the SFTP channel shuts down first.
    sftp: SftpSession,
    session: russh::client::Handle<ClientHandler>,
}
struct ClientHandler {
    profile_id: String,
    host: String,
    port: u16,
    events: Sender<BackendEvent>,
    request_id: u64,
    /// Consumed by the single `check_server_key` call of the initial key
    /// exchange, then resolved with the user's answer.
    decision: Option<tokio::sync::oneshot::Receiver<HostKeyDecision>>,
}

impl russh::client::Handler for ClientHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &russh::keys::PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        let public_key = server_public_key.public_key();
        let key_type = public_key.algorithm().as_str().to_string();
        let public_key_openssh = public_key.to_openssh().unwrap_or_default();
        let fingerprint = public_key
            .fingerprint(russh::keys::HashAlg::Sha256)
            .to_string();

        let mut known_hosts = KnownHosts::load();
        let stored = known_hosts.find(&self.host, self.port, &key_type);
        if let Some(stored) = stored
            && stored.public_key.trim() == public_key_openssh.trim()
        {
            return Ok(true);
        }

        let previous_fingerprint = stored.map(|entry| entry.fingerprint.clone());
        let _ = self
            .events
            .send(BackendEvent::HostKeyVerification {
                profile_id: self.profile_id.clone(),
                request_id: self.request_id,
                host: self.host.clone(),
                port: self.port,
                key_type: key_type.clone(),
                fingerprint: fingerprint.clone(),
                previous_fingerprint,
            })
            .await;

        let Some(decision) = self.decision.take() else {
            return Ok(false);
        };
        let Ok(decision) = decision.await else {
            return Ok(false);
        };

        match decision {
            HostKeyDecision::Trust { remember } => {
                if remember {
                    known_hosts.upsert(KnownHost {
                        host: self.host.clone(),
                        port: self.port,
                        key_type,
                        public_key: public_key_openssh,
                        fingerprint,
                    });
                    known_hosts.save();
                }
                Ok(true)
            }
            HostKeyDecision::Reject => Ok(false),
        }
    }
}

/// Adds an actionable hint to the low-level reason for a failed connection.
fn describe_connect_error(error: &str) -> String {
    let lower = error.to_ascii_lowercase();
    let hint = if lower.contains("connection refused") || lower.contains("os error 10061") {
        Some("服务器拒绝连接，请检查端口是否正确，或确认 SSH 服务已启动")
    } else if lower.contains("timed out")
        || lower.contains("timeout")
        || lower.contains("os error 10060")
    {
        Some("连接超时，请检查网络和防火墙设置")
    } else if lower.contains("os error 11001")
        || lower.contains("failed to lookup address")
        || lower.contains("no such host")
        || lower.contains("name or service not known")
    {
        Some("无法解析主机地址，请检查主机名是否正确")
    } else if lower.contains("unreachable")
        || lower.contains("os error 10051")
        || lower.contains("os error 10065")
    {
        Some("无法访问服务器，请检查网络连接")
    } else if lower.contains("reset by peer") || lower.contains("os error 10054") {
        Some("连接被服务器重置")
    } else if lower.contains("unknown server key") || lower.contains("unknown key") {
        Some("未信任服务器主机密钥，连接已取消")
    } else {
        None
    };

    match hint {
        Some(hint) => format!("{hint}（{error}）"),
        None => error.to_string(),
    }
}

async fn connect_client(
    profile: &ConnectionProfile,
    handler: ClientHandler,
) -> Result<Client, String> {
    let config = Arc::new(russh::client::Config {
        // Detect half-open connections too: without keepalives a dropped
        // network path can go unnoticed until the next SFTP request.
        keepalive_interval: Some(std::time::Duration::from_secs(30)),
        keepalive_max: 3,
        ..Default::default()
    });
    let mut session =
        russh::client::connect(config, (profile.host.as_str(), profile.port), handler)
            .await
            .map_err(|error| describe_connect_error(&error.to_string()))?;

    let authenticated = match &profile.auth {
        AuthMethod::Password { password } => session
            .authenticate_password(profile.username.clone(), password.clone())
            .await
            .map_err(|error| format!("密码认证失败：{error}"))?,
        AuthMethod::PrivateKey { path, passphrase } => {
            let key_path = expand_tilde(path);
            let passphrase = if passphrase.is_empty() {
                None
            } else {
                Some(passphrase.as_str())
            };
            let key = russh::keys::load_secret_key(&key_path, passphrase)
                .map_err(|error| format!("读取私钥失败：{error}"))?;
            let rsa_hash = session
                .best_supported_rsa_hash()
                .await
                .map_err(|error| format!("公钥认证失败：{error}"))?
                .flatten();
            let key = russh::keys::PrivateKeyWithHashAlg::new(Arc::new(key), rsa_hash);
            session
                .authenticate_publickey(profile.username.clone(), key)
                .await
                .map_err(|error| format!("公钥认证失败：{error}"))?
        }
    };

    if !authenticated.success() {
        return Err("认证失败，请检查用户名、密码或私钥".to_string());
    }

    let channel = session
        .channel_open_session()
        .await
        .map_err(|error| format!("打开 SSH 通道失败：{error}"))?;
    channel
        .request_subsystem(true, "sftp")
        .await
        .map_err(|error| format!("服务器不支持 SFTP：{error}"))?;

    let sftp = SftpSession::new(channel.into_stream())
        .await
        .map_err(|error| format!("初始化 SFTP 会话失败：{error}"))?;

    Ok(Client { sftp, session })
}

async fn list_dir(sftp: &SftpSession, path: &str) -> Result<(String, Vec<FileEntry>), String> {
    let canonical = sftp
        .canonicalize(path.to_string())
        .await
        .map_err(|error| format!("打开目录 {path} 失败：{error}"))?;

    let read_dir = sftp
        .read_dir(canonical.clone())
        .await
        .map_err(|error| format!("读取目录 {canonical} 失败：{error}"))?;

    let mut entries = Vec::new();
    for entry in read_dir {
        let name = entry.file_name();
        if name == "." || name == ".." {
            continue;
        }
        let metadata = entry.metadata();
        let file_type = metadata.file_type();
        let is_dir = file_type.is_dir();
        entries.push(FileEntry {
            hidden: name.starts_with('.'),
            path: join_remote(&canonical, &name),
            name,
            is_dir,
            size: if is_dir { 0 } else { metadata.len() },
            modified: metadata.mtime.map(|time| time as i64),
            permissions: metadata.permissions,
        });
    }
    sort_entries(&mut entries);
    Ok((canonical, entries))
}

/// Resolves a pasted path. A directory is listed as usual; a file path falls
/// back to its parent directory, returning the file name so the UI can say
/// where it jumped to.
async fn locate_remote(
    sftp: &SftpSession,
    path: &str,
) -> Result<(String, Vec<FileEntry>, Option<String>), String> {
    match list_dir(sftp, path).await {
        Ok((directory, entries)) => Ok((directory, entries, None)),
        Err(list_error) => {
            // Listing failed, so the path is most likely a file. `metadata`
            // still works for file paths even when a server refuses to resolve
            // them with `canonicalize`.
            let metadata = sftp
                .metadata(path.to_string())
                .await
                .map_err(|_| list_error.clone())?;
            if metadata.file_type().is_dir() {
                return Err(list_error);
            }

            let trimmed = path.trim_end_matches('/');
            let parent = match trimmed.rfind('/') {
                Some(0) => "/".to_string(),
                Some(index) => trimmed[..index].to_string(),
                None => ".".to_string(),
            };
            let (directory, entries) = list_dir(sftp, &parent).await?;
            let name = trimmed
                .rsplit('/')
                .next()
                .filter(|name| !name.is_empty())
                .unwrap_or(path)
                .to_string();
            Ok((directory, entries, Some(name)))
        }
    }
}

/// Deletes a remote file, or a directory and everything below it.
async fn delete_remote_path(sftp: &SftpSession, root: &str) -> Result<(), String> {
    let metadata = sftp
        .symlink_metadata(root.to_string())
        .await
        .map_err(|error| format!("读取 {root} 失败：{error}"))?;

    if !metadata.file_type().is_dir() {
        return sftp
            .remove_file(root.to_string())
            .await
            .map_err(|error| format!("删除 {root} 失败：{error}"));
    }

    let mut stack = vec![root.to_string()];
    let mut dirs = Vec::new();

    while let Some(path) = stack.pop() {
        let read_dir = sftp
            .read_dir(path.clone())
            .await
            .map_err(|error| format!("读取目录 {path} 失败：{error}"))?;
        for entry in read_dir {
            let name = entry.file_name();
            if name == "." || name == ".." {
                continue;
            }
            let child = join_remote(&path, &name);
            if entry.file_type().is_dir() {
                stack.push(child);
            } else {
                sftp.remove_file(child.clone())
                    .await
                    .map_err(|error| format!("删除 {child} 失败：{error}"))?;
            }
        }
        dirs.push(path);
    }

    for dir in dirs.into_iter().rev() {
        sftp.remove_dir(dir.clone())
            .await
            .map_err(|error| format!("删除目录 {dir} 失败：{error}"))?;
    }

    Ok(())
}

fn local_total(root: &Path, cancel: &AtomicBool) -> Option<u64> {
    let mut total = 0u64;
    let mut stack = vec![root.to_path_buf()];
    while let Some(current) = stack.pop() {
        if cancel.load(Ordering::Relaxed) {
            return None;
        }
        let Ok(metadata) = std::fs::metadata(&current) else {
            continue;
        };
        if metadata.is_dir() {
            let Ok(read_dir) = std::fs::read_dir(&current) else {
                continue;
            };
            for entry in read_dir.flatten() {
                let Ok(metadata) = entry.metadata() else {
                    continue;
                };
                if is_hidden(&entry.file_name().to_string_lossy(), &metadata) {
                    continue;
                }
                stack.push(entry.path());
            }
        } else {
            total = total.saturating_add(metadata.len());
        }
    }
    Some(total)
}

async fn remote_total(sftp: &SftpSession, root: &str, cancel: &AtomicBool) -> Option<u64> {
    let mut total = 0u64;
    let mut stack = VecDeque::from([root.to_string()]);

    while let Some(path) = stack.pop_front() {
        if cancel.load(Ordering::Relaxed) {
            return None;
        }
        let Ok(metadata) = sftp.metadata(path.clone()).await else {
            continue;
        };

        if metadata.file_type().is_dir() {
            let Ok(read_dir) = sftp.read_dir(path).await else {
                continue;
            };
            for entry in read_dir {
                if cancel.load(Ordering::Relaxed) {
                    return None;
                }
                if entry.file_type().is_dir() {
                    stack.push_back(entry.path());
                } else {
                    total = total.saturating_add(entry.metadata().len());
                }
            }
        } else {
            total = total.saturating_add(metadata.len());
        }
    }

    Some(total)
}

async fn upload_path(
    sftp: &SftpSession,
    local: &Path,
    remote: &str,
    events: &Sender<BackendEvent>,
    id: u64,
    transferred: &mut u64,
    last_emit: &mut u64,
    cancel: &AtomicBool,
) -> Result<(), String> {
    if cancel.load(Ordering::Relaxed) {
        return Err("已取消".to_string());
    }

    let metadata = tokio::fs::metadata(local)
        .await
        .map_err(|error| format!("读取本地文件 {} 失败：{error}", local.display()))?;

    if metadata.is_dir() {
        let _ = sftp.create_dir(remote.to_string()).await;
        let mut read_dir = tokio::fs::read_dir(local)
            .await
            .map_err(|error| format!("读取本地目录 {} 失败：{error}", local.display()))?;

        while let Some(entry) = read_dir
            .next_entry()
            .await
            .map_err(|error| format!("读取本地目录 {} 失败：{error}", local.display()))?
        {
            if cancel.load(Ordering::Relaxed) {
                return Err("已取消".to_string());
            }
            let name = entry.file_name().to_string_lossy().to_string();
            let Ok(metadata) = entry.metadata().await else {
                continue;
            };
            if is_hidden(&name, &metadata) {
                continue;
            }
            let child_local = entry.path();
            let child_remote = join_remote(remote, &name);
            Box::pin(upload_path(
                sftp,
                &child_local,
                &child_remote,
                events,
                id,
                &mut *transferred,
                &mut *last_emit,
                cancel,
            ))
            .await?;
        }
    } else {
        let mut source = tokio::fs::File::open(local)
            .await
            .map_err(|error| format!("打开本地文件 {} 失败：{error}", local.display()))?;
        let mut target = sftp
            .open_with_flags(
                remote.to_string(),
                OpenFlags::CREATE | OpenFlags::TRUNCATE | OpenFlags::WRITE,
            )
            .await
            .map_err(|error| format!("创建远程文件 {remote} 失败：{error}"))?;

        let mut buffer = vec![0u8; 64 * 1024];
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err("已取消".to_string());
            }
            let read = source
                .read(&mut buffer)
                .await
                .map_err(|error| format!("读取本地文件 {} 失败：{error}", local.display()))?;
            if read == 0 {
                break;
            }
            target
                .write_all(&buffer[..read])
                .await
                .map_err(|error| format!("写入远程文件 {remote} 失败：{error}"))?;
            *transferred = transferred.saturating_add(read as u64);
            emit_progress_if_needed(events, id, transferred, last_emit).await;
        }

        target
            .shutdown()
            .await
            .map_err(|error| format!("关闭远程文件 {remote} 失败：{error}"))?;
    }

    Ok(())
}

async fn download_path(
    sftp: &SftpSession,
    remote: &str,
    local: &Path,
    events: &Sender<BackendEvent>,
    id: u64,
    transferred: &mut u64,
    last_emit: &mut u64,
    cancel: &AtomicBool,
) -> Result<(), String> {
    if cancel.load(Ordering::Relaxed) {
        return Err("已取消".to_string());
    }

    let metadata = sftp
        .metadata(remote.to_string())
        .await
        .map_err(|error| format!("读取远程路径 {remote} 失败：{error}"))?;

    if metadata.file_type().is_dir() {
        tokio::fs::create_dir_all(local)
            .await
            .map_err(|error| format!("创建本地目录 {} 失败：{error}", local.display()))?;

        let read_dir = sftp
            .read_dir(remote.to_string())
            .await
            .map_err(|error| format!("读取远程目录 {remote} 失败：{error}"))?;

        for entry in read_dir {
            if cancel.load(Ordering::Relaxed) {
                return Err("已取消".to_string());
            }
            let name = entry.file_name();
            if name == "." || name == ".." {
                continue;
            }
            let child_remote = join_remote(remote, &name);
            let child_local = local.join(&name);
            Box::pin(download_path(
                sftp,
                &child_remote,
                &child_local,
                events,
                id,
                &mut *transferred,
                &mut *last_emit,
                cancel,
            ))
            .await?;
        }
    } else {
        if let Some(parent) = local.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|error| format!("创建本地目录 {} 失败：{error}", parent.display()))?;
        }

        let mut source = sftp
            .open_with_flags(remote.to_string(), OpenFlags::READ)
            .await
            .map_err(|error| format!("打开远程文件 {remote} 失败：{error}"))?;
        let mut target = tokio::fs::File::create(local)
            .await
            .map_err(|error| format!("创建本地文件 {} 失败：{error}", local.display()))?;

        let mut buffer = vec![0u8; 64 * 1024];
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err("已取消".to_string());
            }
            let read = source
                .read(&mut buffer)
                .await
                .map_err(|error| format!("读取远程文件 {remote} 失败：{error}"))?;
            if read == 0 {
                break;
            }
            target
                .write_all(&buffer[..read])
                .await
                .map_err(|error| format!("写入本地文件 {} 失败：{error}", local.display()))?;
            *transferred = transferred.saturating_add(read as u64);
            emit_progress_if_needed(events, id, transferred, last_emit).await;
        }

        target
            .flush()
            .await
            .map_err(|error| format!("写入本地文件 {} 失败：{error}", local.display()))?;
    }

    Ok(())
}

async fn emit_progress_if_needed(
    events: &Sender<BackendEvent>,
    id: u64,
    transferred: &mut u64,
    last_emit: &mut u64,
) {
    if transferred.saturating_sub(*last_emit) >= 512 * 1024 {
        *last_emit = *transferred;
        let _ = events
            .send(BackendEvent::TransferProgress {
                id,
                transferred: *transferred,
            })
            .await;
    }
}

async fn emit_transfer_done(
    events: &Sender<BackendEvent>,
    session_id: String,
    id: u64,
    name: String,
    direction: Direction,
    result: Result<(), String>,
    cancelled: bool,
) {
    let (ok, cancelled, message) = if cancelled {
        (false, true, "已取消".to_string())
    } else {
        match result {
            Ok(()) => (true, false, format!("{}完成", direction.label())),
            Err(error) => (false, false, error),
        }
    };
    let _ = events
        .send(BackendEvent::TransferDone {
            session_id,
            id,
            name,
            direction,
            ok,
            cancelled,
            message,
        })
        .await;
}
