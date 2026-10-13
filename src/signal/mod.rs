//! Signal connectivity via presage, running on its own thread.
//!
//! presage is async-on-tokio and its futures aren't `Send`, so it gets a
//! dedicated thread with a single-threaded runtime. The GLib side talks to it
//! through [`Backend`] (commands in) and a stream of [`Event`]s (out); tokio's
//! sync primitives work from any thread, so neither side blocks the other.
//!
//! presage keeps protocol state (keys, sessions, contacts, groups) in its
//! own SQLite store and writes every message there too. The app's
//! [`crate::store`] is where messages live for the UI; presage's copies are
//! handed over as [`Origin::Stored`] events on start-up and then deleted, so
//! there is one long-lived copy of each message.

mod convert;

use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use futures::{StreamExt as _, future, pin_mut};
use presage::Manager;
use presage::libsignal_service::backup::MessageBackupKey;
use presage::libsignal_service::configuration::SignalServers;
use presage::libsignal_service::prelude::{ProfileKey, ProtobufMessage as _, Uuid};
use presage::libsignal_service::protocol::{Aci, ServiceId};
use presage::libsignal_service::sender::AttachmentSpec;
use presage::manager::Registered;
use presage::model::identity::OnNewIdentity;
use presage::model::messages::Received;
use presage::proto::receipt_message::Type as ReceiptType;
use presage::proto::{AttachmentPointer, DataMessage, GroupContextV2, ReceiptMessage};
use presage::store::{ContentsStore as _, Thread};
use presage_store_sqlite::SqliteStore;
use tokio::sync::{Semaphore, mpsc, oneshot};

use crate::model::{ConversationId, OutgoingImage, PeerId, SignalEvent, Timestamp};
pub use convert::Origin;
use convert::{
    MasterKey, Names, Source, content_events, content_group, group_conversation, group_master_key,
    peer_id, service_id,
};

/// Messages read from presage's store are handed over in batches of this size.
const HISTORY_BATCH: usize = 500;
/// presage's futures are large (several MiB unoptimized); the default 2 MiB
/// thread stack overflows while linking.
const THREAD_STACK_SIZE: usize = 64 * 1024 * 1024;
/// Upper bound for the reconnect backoff.
const MAX_RECONNECT_DELAY: Duration = Duration::from_secs(60);
/// Images downloaded at the same time.
const PARALLEL_DOWNLOADS: usize = 3;

pub struct SpawnOptions {
    /// Names the thread and tags its logs.
    pub label: String,
    /// presage's SQLite store (keys, sessions, contacts, groups).
    pub store_path: PathBuf,
    /// Exists from linking until the history import has finished, so an
    /// import interrupted by quitting is resumed on the next start.
    pub history_marker: PathBuf,
    /// Name shown in the phone's "Linked devices" list.
    pub device_name: String,
}

#[derive(Debug, thiserror::Error)]
pub enum SendError {
    #[error("not connected to Signal")]
    Offline,
    #[error("invalid recipient")]
    InvalidRecipient,
    #[error("{0}")]
    Signal(String),
}

#[derive(Debug, thiserror::Error)]
pub enum DownloadError {
    #[error("not connected to Signal")]
    Offline,
    #[error("{0}")]
    Signal(String),
    #[error("saving failed: {0}")]
    Io(#[from] std::io::Error),
}

pub enum HistoryStatus {
    /// The phone must confirm the transfer (or skip it).
    WaitingForPhone,
    Downloading {
        received: u64,
        total: u64,
    },
    Importing,
}

pub enum Event {
    /// Show this as a QR code for the phone to scan.
    LinkCode(String),
    /// The code expired or linking failed; [`Backend::retry_link`] starts over.
    LinkFailed(String),
    /// The phone accepted the link; history and [`Event::Ready`] follow.
    Linked,
    History(HistoryStatus),
    /// The account is usable; sending works from here on.
    Ready {
        account: String,
    },
    Messages {
        events: Vec<SignalEvent>,
        origin: Origin,
        /// For [`Origin::Stored`] batches: completed once the events are
        /// committed, after which presage's copies are deleted.
        committed: Option<oneshot::Sender<()>>,
    },
    /// Display names of contacts and groups.
    Names(Vec<(ConversationId, String)>),
    Connection(Connection),
    /// Unrecoverable; the backend thread has stopped.
    Fatal(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Connection {
    Online,
    Offline,
}

enum Command {
    RetryLink,
    Send {
        conversation: ConversationId,
        text: String,
        images: Vec<OutgoingImage>,
        timestamp: Timestamp,
        done: oneshot::Sender<Result<Vec<Vec<u8>>, SendError>>,
    },
    Download {
        pointer: Vec<u8>,
        dest: PathBuf,
        done: oneshot::Sender<Result<(), DownloadError>>,
    },
    FetchAvatar {
        conversation: ConversationId,
        dest: PathBuf,
        done: oneshot::Sender<Result<bool, DownloadError>>,
    },
    ReadReceipts {
        sender: PeerId,
        timestamps: Vec<Timestamp>,
    },
}

/// Handle to the backend thread. Dropping it stops the backend, and its
/// event stream ends.
pub struct Backend {
    commands: mpsc::UnboundedSender<Command>,
}

impl Backend {
    pub fn spawn(opts: SpawnOptions) -> std::io::Result<(Self, async_channel::Receiver<Event>)> {
        let (commands_tx, commands_rx) = mpsc::unbounded_channel();
        let (events_tx, events_rx) = async_channel::unbounded();
        std::thread::Builder::new()
            .name(format!("signal-{}", opts.label))
            .stack_size(THREAD_STACK_SIZE)
            .spawn(move || {
                // Everything on this thread belongs to the one account.
                let span = tracing::info_span!("account", id = %opts.label);
                let _entered = span.enter();
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("building a current-thread tokio runtime cannot fail");
                let local = tokio::task::LocalSet::new();
                let result = local.block_on(&runtime, run(opts, events_tx.clone(), commands_rx));
                if let Err(err) = result {
                    tracing::error!(err = %format!("{err:#}"), "signal backend stopped");
                    let _ = events_tx.send_blocking(Event::Fatal(format!("{err:#}")));
                }
            })?;
        Ok((
            Self {
                commands: commands_tx,
            },
            events_rx,
        ))
    }

    pub fn retry_link(&self) {
        let _ = self.commands.send(Command::RetryLink);
    }

    /// Sends text with images, either of which may be empty. Resolves to
    /// the uploaded images' pointers (see [`crate::model::NewAttachment`]), in
    /// order.
    ///
    /// The returned future doesn't borrow the backend, which may be
    /// dropped (its account removed) while the send is in flight.
    pub fn send(
        &self,
        conversation: &ConversationId,
        text: &str,
        images: Vec<OutgoingImage>,
        timestamp: Timestamp,
    ) -> impl Future<Output = Result<Vec<Vec<u8>>, SendError>> + use<> {
        let (done, result) = oneshot::channel();
        let queued = self
            .commands
            .send(Command::Send {
                conversation: conversation.clone(),
                text: text.to_owned(),
                images,
                timestamp,
                done,
            })
            .is_ok();
        async move {
            if !queued {
                return Err(SendError::Offline);
            }
            result.await.map_err(|_| SendError::Offline)?
        }
    }

    /// Downloads and decrypts the attachment `pointer` describes (see
    /// [`crate::model::NewAttachment`]) into `dest`. Fails while offline.
    pub fn download(
        &self,
        pointer: Vec<u8>,
        dest: PathBuf,
    ) -> impl Future<Output = Result<(), DownloadError>> + use<> {
        let (done, result) = oneshot::channel();
        let queued = self
            .commands
            .send(Command::Download {
                pointer,
                dest,
                done,
            })
            .is_ok();
        async move {
            if !queued {
                return Err(DownloadError::Offline);
            }
            // Commands are dropped, not answered, while offline.
            result.await.map_err(|_| DownloadError::Offline)?
        }
    }

    /// Saves the picture of a contact or group to `dest`. Resolves to
    /// whether there is one. Fails while offline.
    pub fn fetch_avatar(
        &self,
        conversation: &ConversationId,
        dest: PathBuf,
    ) -> impl Future<Output = Result<bool, DownloadError>> + use<> {
        let (done, result) = oneshot::channel();
        let queued = self
            .commands
            .send(Command::FetchAvatar {
                conversation: conversation.clone(),
                dest,
                done,
            })
            .is_ok();
        async move {
            if !queued {
                return Err(DownloadError::Offline);
            }
            result.await.map_err(|_| DownloadError::Offline)?
        }
    }

    /// Fire and forget: failures are logged by the backend.
    pub fn send_read_receipts(&self, sender: PeerId, timestamps: Vec<Timestamp>) {
        let _ = self
            .commands
            .send(Command::ReadReceipts { sender, timestamps });
    }
}

type Events = async_channel::Sender<Event>;
type Commands = mpsc::UnboundedReceiver<Command>;

async fn emit(events: &Events, event: Event) -> anyhow::Result<()> {
    events
        .send(event)
        .await
        .map_err(|_| anyhow::anyhow!("the app went away"))
}

async fn run(opts: SpawnOptions, events: Events, mut commands: Commands) -> anyhow::Result<()> {
    if let Some(dir) = opts.store_path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let url = format!("sqlite://{}", opts.store_path.display());
    let store = SqliteStore::open_with_passphrase(&url, None, OnNewIdentity::Trust).await?;

    let mut manager = match Manager::load_registered(store.clone()).await {
        Ok(manager) => manager,
        Err(presage::Error::NotYetRegisteredError) => {
            let Some(manager) = link(store.clone(), &opts, &events, &mut commands).await? else {
                tracing::info!("linking cancelled");
                return Ok(());
            };
            std::fs::write(&opts.history_marker, b"")?;
            emit(&events, Event::Linked).await?;
            manager
        }
        Err(err) => return Err(err.into()),
    };

    if opts.history_marker.exists() {
        let finished = match manager.backup_message_key().await? {
            Some(key) => import_history(&mut manager, key, &events).await,
            None => {
                tracing::info!("the phone offered no message history");
                true
            }
        };
        if finished {
            std::fs::remove_file(&opts.history_marker)?;
        }
    }

    let account = manager.registration_data().phone_number.to_string();
    tracing::info!(%account, "signal account ready");
    emit(&events, Event::Ready { account }).await?;

    let names = load_names(&manager).await?;
    emit(&events, Event::Names(names.clone())).await?;
    let mention_names = mention_names(&names);
    hand_over_stored_messages(manager.store(), &opts.store_path, &mention_names, &events).await?;

    let group_titles = group_titles_of(&names);
    receive_loop(manager, mention_names, group_titles, events, commands).await
}

fn mention_names(names: &[(ConversationId, String)]) -> Names {
    names
        .iter()
        .filter_map(|(id, name)| match id {
            ConversationId::Direct(peer) => Some((peer.clone(), name.clone())),
            ConversationId::Group(_) => None,
        })
        .collect()
}

/// Group titles as last sent to the app.
type GroupTitles = HashMap<ConversationId, String>;

fn group_titles_of(names: &[(ConversationId, String)]) -> GroupTitles {
    names
        .iter()
        .filter(|(id, _)| matches!(id, ConversationId::Group(_)))
        .cloned()
        .collect()
}

/// Shows QR codes until the phone links us. Returns `None` if the app
/// dropped the backend, i.e. cancelled linking.
async fn link(
    store: SqliteStore,
    opts: &SpawnOptions,
    events: &Events,
    commands: &mut Commands,
) -> anyhow::Result<Option<Manager<SqliteStore, Registered>>> {
    loop {
        let (url_tx, url_rx) = futures::channel::oneshot::channel::<url::Url>();
        let show_code = async {
            if let Ok(url) = url_rx.await {
                let _ = emit(events, Event::LinkCode(url.to_string())).await;
            }
        };
        let linking = future::join(
            Manager::link_secondary_device(
                store.clone(),
                SignalServers::Production,
                opts.device_name.clone(),
                url_tx,
            ),
            show_code,
        );
        pin_mut!(linking);
        let result = loop {
            tokio::select! {
                (result, ()) = &mut linking => break result,
                command = commands.recv() => match command {
                    None => return Ok(None),
                    Some(command) => reject_while_linking(command),
                },
            }
        };
        let err = match result {
            Ok(manager) => return Ok(Some(manager)),
            Err(err) => err,
        };
        tracing::warn!(%err, "linking did not complete");
        emit(events, Event::LinkFailed(err.to_string())).await?;
        // Nothing else can be done until the user asks for a new code.
        loop {
            match commands.recv().await {
                None => return Ok(None),
                Some(Command::RetryLink) => break,
                Some(command) => reject_while_linking(command),
            }
        }
    }
}

fn reject_while_linking(command: Command) {
    match command {
        Command::Send { done, .. } => {
            let _ = done.send(Err(SendError::Offline));
        }
        Command::Download { done, .. } => {
            let _ = done.send(Err(DownloadError::Offline));
        }
        Command::FetchAvatar { done, .. } => {
            let _ = done.send(Err(DownloadError::Offline));
        }
        Command::RetryLink | Command::ReadReceipts { .. } => {}
    }
}

/// Link & Sync: download the history archive the phone uploads for us.
/// Failure is not fatal; the account works without history. Returns false
/// if the import should be retried on the next start.
async fn import_history(
    manager: &mut Manager<SqliteStore, Registered>,
    key: MessageBackupKey,
    events: &Events,
) -> bool {
    let progress = |p: presage::backup::BackupImportProgress| {
        use presage::backup::BackupImportProgress as P;
        let status = match p {
            P::WaitingForUpload => HistoryStatus::WaitingForPhone,
            P::Downloading {
                bytes_received,
                total,
            } => HistoryStatus::Downloading {
                received: bytes_received,
                total,
            },
            P::Processing => HistoryStatus::Importing,
            P::Done => return,
        };
        let _ = events.try_send(Event::History(status));
    };
    match manager
        .download_and_import_backup(Some(key), progress)
        .await
    {
        Ok(true) => {
            tracing::info!("imported message history");
            true
        }
        Ok(false) => {
            tracing::info!("the phone did not send message history");
            true
        }
        Err(err) => {
            tracing::warn!(%err, "importing message history failed, retrying on next start");
            false
        }
    }
}

/// Display names from presage's store: address book name, then profile
/// name, then phone number.
async fn load_names(
    manager: &Manager<SqliteStore, Registered>,
) -> anyhow::Result<Vec<(ConversationId, String)>> {
    let store = manager.store();
    let mut names = Vec::new();
    for contact in store.contacts().await? {
        let contact = contact?;
        if contact.blocked {
            continue;
        }
        let peer = peer_id(&ServiceId::Aci(Aci::from(contact.uuid)));
        let name = match contact.name.trim() {
            "" => profile_name(manager, contact.uuid, &contact.profile_key)
                .await
                .or_else(|| contact.phone_number.as_ref().map(ToString::to_string)),
            name => Some(name.to_owned()),
        };
        if let Some(name) = name {
            names.push((ConversationId::Direct(peer), name));
        }
    }
    for group in store.groups().await? {
        let (master_key, group) = group?;
        if let Some(title) = group.title.filter(|t| !t.trim().is_empty()) {
            names.push((group_conversation(&master_key), title));
        }
    }
    let me = peer_id(&ServiceId::Aci(Aci::from(
        manager.registration_data().service_ids.aci,
    )));
    names.push((ConversationId::Direct(me), "Note to Self".to_owned()));
    Ok(names)
}

async fn profile_name(
    manager: &Manager<SqliteStore, Registered>,
    uuid: Uuid,
    profile_key: &[u8],
) -> Option<String> {
    let key = ProfileKey::create(profile_key.try_into().ok()?);
    let profile = manager.store().profile(uuid, key).await.ok()??;
    let name = profile.name?;
    let full = match name.family_name {
        Some(family) if !family.is_empty() => format!("{} {family}", name.given_name),
        _ => name.given_name,
    };
    (!full.trim().is_empty()).then_some(full)
}

/// Passes every message presage has stored (history imports and anything
/// received in earlier sessions) to the app, then deletes presage's copies.
async fn hand_over_stored_messages(
    store: &SqliteStore,
    store_path: &Path,
    mention_names: &Names,
    events: &Events,
) -> anyhow::Result<()> {
    let mut batch = Vec::new();
    let mut total = 0usize;
    for thread in stored_threads(store_path)? {
        let chat = match &thread {
            Thread::Contact(id) => ConversationId::Direct(peer_id(id)),
            Thread::Group(master_key) => group_conversation(master_key),
        };
        let source = Source::Stored { chat: &chat };
        for content in store.messages(&thread, ..).await? {
            batch.extend(content_events(&content?, source, mention_names));
            if batch.len() >= HISTORY_BATCH {
                total += batch.len();
                commit_batch(std::mem::take(&mut batch), events).await?;
            }
        }
    }
    total += batch.len();
    commit_batch(batch, events).await?;
    // The store is a handle to a shared connection pool.
    store.clone().clear_messages().await?;
    tracing::info!(events = total, "handed over stored messages");
    Ok(())
}

/// Every thread presage holds messages for. presage has no API to list
/// them, so this reads its `threads` table. Contacts and groups are not
/// enough: people outside the address book have threads too, and presage's
/// copies are deleted after the hand-over, so a thread missing here would
/// lose its messages.
fn stored_threads(store_path: &Path) -> anyhow::Result<Vec<Thread>> {
    let conn = rusqlite::Connection::open_with_flags(
        store_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    let mut stmt = conn.prepare("SELECT group_master_key, recipient_id FROM threads")?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, Option<Vec<u8>>>(0)?,
            // sqlx writes uuids as 16-byte blobs.
            row.get::<_, Option<Vec<u8>>>(1)?,
        ))
    })?;
    rows.map(|row| match row? {
        (Some(master_key), _) => {
            Ok(Thread::Group(master_key.try_into().map_err(|_| {
                anyhow::anyhow!("group master key is not 32 bytes")
            })?))
        }
        (None, Some(uuid)) => Ok(Thread::Contact(ServiceId::Aci(Aci::from(
            Uuid::from_slice(&uuid)?,
        )))),
        (None, None) => anyhow::bail!("presage thread without a recipient"),
    })
    .collect()
}

async fn commit_batch(batch: Vec<SignalEvent>, events: &Events) -> anyhow::Result<()> {
    if batch.is_empty() {
        return Ok(());
    }
    let (committed, done) = oneshot::channel();
    emit(
        events,
        Event::Messages {
            events: batch,
            origin: Origin::Stored,
            committed: Some(committed),
        },
    )
    .await?;
    done.await
        .map_err(|_| anyhow::anyhow!("the app dropped a history batch"))
}

/// Receives until the app goes away, reconnecting with capped backoff.
async fn receive_loop(
    manager: Manager<SqliteStore, Registered>,
    mut names: Names,
    mut group_titles: GroupTitles,
    events: Events,
    mut commands: Commands,
) -> anyhow::Result<()> {
    let mut delay = Duration::from_secs(1);
    let downloads = Rc::new(Semaphore::new(PARALLEL_DOWNLOADS));
    loop {
        let mut receiver = manager.clone();
        match receiver.receive_messages().await {
            Ok(stream) => {
                emit(&events, Event::Connection(Connection::Online)).await?;
                delay = Duration::from_secs(1);
                pin_mut!(stream);
                loop {
                    // Both branches are cancel-safe: `next` and `recv` lose
                    // nothing when the other branch wins.
                    tokio::select! {
                        received = stream.next() => match received {
                            Some(received) => {
                                on_received(received, &manager, &mut names, &mut group_titles, &events)
                                    .await?;
                            }
                            None => break,
                        },
                        command = commands.recv() => match command {
                            Some(command) => handle_command(command, &manager, &downloads),
                            None => return Ok(()),
                        },
                    }
                }
            }
            Err(err) => tracing::warn!(%err, "connecting to Signal failed"),
        }
        emit(&events, Event::Connection(Connection::Offline)).await?;
        tracing::info!(?delay, "reconnecting");
        // Keep answering commands while offline so sends fail fast.
        let sleep = tokio::time::sleep(delay);
        pin_mut!(sleep);
        loop {
            tokio::select! {
                () = &mut sleep => break,
                command = commands.recv() => match command {
                    Some(Command::Send { done, .. }) => {
                        let _ = done.send(Err(SendError::Offline));
                    }
                    Some(Command::Download { done, .. }) => {
                        let _ = done.send(Err(DownloadError::Offline));
                    }
                    Some(Command::FetchAvatar { done, .. }) => {
                        let _ = done.send(Err(DownloadError::Offline));
                    }
                    Some(_) => {}
                    None => return Ok(()),
                },
            }
        }
        delay = (delay * 2).min(MAX_RECONNECT_DELAY);
    }
}

async fn on_received(
    received: Received,
    manager: &Manager<SqliteStore, Registered>,
    names: &mut Names,
    group_titles: &mut GroupTitles,
    events: &Events,
) -> anyhow::Result<()> {
    match received {
        Received::Content(content) => {
            if let Some(group) = content_group(&content) {
                update_group_title(manager, group, group_titles, events).await?;
            }
            let converted = content_events(&content, Source::Live, names);
            if !converted.is_empty() {
                emit(
                    events,
                    Event::Messages {
                        events: converted,
                        origin: Origin::Live,
                        committed: None,
                    },
                )
                .await?;
            }
        }
        Received::Contacts => {
            let all = load_names(manager).await?;
            *names = mention_names(&all);
            *group_titles = group_titles_of(&all);
            emit(events, Event::Names(all)).await?;
        }
        Received::QueueEmpty => tracing::debug!("caught up with queued messages"),
        Received::DecryptionError { sender, .. } => {
            tracing::warn!(sender = %sender.service_id_string(), "could not decrypt a message");
        }
        Received::PeerDecryptionError { sender } => {
            tracing::warn!(sender = %sender.service_id_string(), "a contact could not decrypt our message");
        }
        Received::Disconnected(reason) => tracing::info!(?reason, "disconnected"),
    }
    Ok(())
}

/// Sends a group's title if it is new or changed. presage fetches a group
/// it has not seen, or whose revision moved on (e.g. a rename), before it
/// hands over the message, so the store is up to date here.
async fn update_group_title(
    manager: &Manager<SqliteStore, Registered>,
    group: MasterKey,
    group_titles: &mut GroupTitles,
    events: &Events,
) -> anyhow::Result<()> {
    let title = match manager.store().group(group).await {
        Ok(stored) => stored
            .and_then(|g| g.title)
            .filter(|t| !t.trim().is_empty()),
        Err(err) => {
            tracing::warn!(%err, "reading group failed");
            return Ok(());
        }
    };
    let Some(title) = title else {
        return Ok(());
    };
    let conversation = group_conversation(&group);
    if group_titles.get(&conversation) == Some(&title) {
        return Ok(());
    }
    group_titles.insert(conversation.clone(), title.clone());
    emit(events, Event::Names(vec![(conversation, title)])).await
}

fn handle_command(
    command: Command,
    manager: &Manager<SqliteStore, Registered>,
    downloads: &Rc<Semaphore>,
) {
    let mut manager = manager.clone();
    match command {
        // Only meaningful while linking.
        Command::RetryLink => {}
        Command::Send {
            conversation,
            text,
            images,
            timestamp,
            done,
        } => {
            tokio::task::spawn_local(async move {
                let result = send(&mut manager, &conversation, text, images, timestamp).await;
                if let Err(err) = &result {
                    tracing::warn!(%err, %conversation, "sending failed");
                }
                let _ = done.send(result);
            });
        }
        Command::Download {
            pointer,
            dest,
            done,
        } => {
            let downloads = downloads.clone();
            tokio::task::spawn_local(async move {
                let result = match downloads.acquire().await {
                    Ok(_permit) => download(&manager, &pointer, &dest).await,
                    Err(_) => Err(DownloadError::Offline),
                };
                if let Err(err) = &result {
                    tracing::warn!(%err, "downloading an attachment failed");
                }
                let _ = done.send(result);
            });
        }
        Command::FetchAvatar {
            conversation,
            dest,
            done,
        } => {
            let downloads = downloads.clone();
            tokio::task::spawn_local(async move {
                let result = match downloads.acquire().await {
                    Ok(_permit) => fetch_avatar(&mut manager, &conversation, &dest).await,
                    Err(_) => Err(DownloadError::Offline),
                };
                if let Err(err) = &result {
                    tracing::warn!(%err, %conversation, "fetching a picture failed");
                }
                let _ = done.send(result);
            });
        }
        Command::ReadReceipts { sender, timestamps } => {
            tokio::task::spawn_local(async move {
                let Some(recipient) = service_id(&sender) else {
                    return;
                };
                let receipt = ReceiptMessage {
                    r#type: Some(ReceiptType::Read as i32),
                    timestamp: timestamps
                        .iter()
                        .filter_map(|t| u64::try_from(t.0).ok())
                        .collect(),
                };
                let now = u64::try_from(now_ms()).unwrap_or_default();
                if let Err(err) = manager.send_message(recipient, receipt, now).await {
                    tracing::warn!(%err, %sender, "sending read receipt failed");
                }
            });
        }
    }
}

async fn send(
    manager: &mut Manager<SqliteStore, Registered>,
    conversation: &ConversationId,
    text: String,
    images: Vec<OutgoingImage>,
    timestamp: Timestamp,
) -> Result<Vec<Vec<u8>>, SendError> {
    let ts = u64::try_from(timestamp.0).map_err(|_| SendError::InvalidRecipient)?;
    let attachments = upload(manager, images).await?;
    let message = DataMessage {
        body: (!text.is_empty()).then_some(text),
        attachments: attachments.clone(),
        timestamp: Some(ts),
        ..Default::default()
    };
    let result = match conversation {
        ConversationId::Direct(peer) => {
            let recipient = service_id(peer).ok_or(SendError::InvalidRecipient)?;
            manager.send_message(recipient, message, ts).await
        }
        ConversationId::Group(key) => {
            let master_key = group_master_key(key).ok_or(SendError::InvalidRecipient)?;
            manager
                .send_message_to_group(&master_key, message, ts)
                .await
        }
    };
    result.map_err(|err| SendError::Signal(err.to_string()))?;
    Ok(attachments.iter().map(|a| a.encode_to_vec()).collect())
}

async fn upload(
    manager: &Manager<SqliteStore, Registered>,
    images: Vec<OutgoingImage>,
) -> Result<Vec<AttachmentPointer>, SendError> {
    let specs = images
        .into_iter()
        .map(|image| {
            let spec = AttachmentSpec {
                content_type: image.content_type,
                length: image.data.len(),
                file_name: image.file_name,
                width: image.width,
                height: image.height,
                ..Default::default()
            };
            (spec, image.data.to_vec())
        })
        .collect();
    manager
        .upload_attachments(specs)
        .await
        .map_err(|err| SendError::Signal(err.to_string()))?
        .into_iter()
        .map(|uploaded| uploaded.map_err(|err| SendError::Signal(err.to_string())))
        .collect()
}

async fn download(
    manager: &Manager<SqliteStore, Registered>,
    pointer: &[u8],
    dest: &Path,
) -> Result<(), DownloadError> {
    let pointer = AttachmentPointer::decode(pointer)
        .map_err(|err| DownloadError::Signal(format!("invalid attachment pointer: {err}")))?;
    let data = manager
        .get_attachment(&pointer)
        .await
        .map_err(|err| DownloadError::Signal(err.to_string()))?;
    write_file(dest, &data)?;
    Ok(())
}

/// Returns whether the contact or group has a picture.
async fn fetch_avatar(
    manager: &mut Manager<SqliteStore, Registered>,
    conversation: &ConversationId,
    dest: &Path,
) -> Result<bool, DownloadError> {
    let signal = |err: presage::Error<_>| DownloadError::Signal(err.to_string());
    let avatar = match conversation {
        ConversationId::Direct(peer) => {
            // Profiles are looked up by ACI; PNI-only contacts have none.
            let Some(ServiceId::Aci(aci)) = service_id(peer) else {
                return Ok(false);
            };
            let key = manager
                .store()
                .profile_key(&ServiceId::Aci(aci))
                .await
                .map_err(|err| DownloadError::Signal(err.to_string()))?;
            let Some(key) = key else {
                return Ok(false);
            };
            manager
                .retrieve_profile_avatar_by_uuid(Uuid::from(aci), key)
                .await
                .map_err(signal)?
        }
        ConversationId::Group(key) => {
            let master_key = group_master_key(key)
                .ok_or_else(|| DownloadError::Signal("invalid group".to_owned()))?;
            // Revision 0: the locally known group is recent enough.
            let context = GroupContextV2 {
                master_key: Some(master_key.to_vec()),
                revision: Some(0),
                ..Default::default()
            };
            manager
                .retrieve_group_avatar(context)
                .await
                .map_err(signal)?
        }
    };
    match avatar.filter(|bytes| !bytes.is_empty()) {
        Some(bytes) => {
            write_file(dest, &bytes)?;
            Ok(true)
        }
        None => Ok(false),
    }
}

/// Writes through a temporary file, so `dest` is either complete or absent.
fn write_file(dest: &Path, data: &[u8]) -> std::io::Result<()> {
    let partial = dest.with_extension("part");
    std::fs::write(&partial, data)?;
    std::fs::rename(&partial, dest)
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

#[cfg(test)]
mod tests {
    use presage::libsignal_service::content::{Content, ContentBody, Metadata};
    use presage::libsignal_service::protocol::DeviceId;
    use presage::model::groups::Group;
    use presage::proto::SyncMessage;
    use presage::proto::sync_message::{self, Sent};

    use super::*;
    use crate::model::{Author, NewMessage};

    const GROUP_KEY: [u8; 32] = [7; 32];

    fn aci(n: u8) -> Aci {
        Aci::from(Uuid::from_bytes([n; 16]))
    }

    /// A row the way presage's history import writes it: plain delivery
    /// metadata and no group context in the message itself.
    fn imported(sender: Aci, me: Aci, ts: u64, body: ContentBody) -> Content {
        let at = chrono::DateTime::from_timestamp_millis(ts as i64).expect("valid timestamp");
        Content {
            metadata: Metadata {
                sender: ServiceId::Aci(sender),
                destination: ServiceId::Aci(me),
                sender_device: DeviceId::new(1).expect("valid device id"),
                server_guid: None,
                client_timestamp: at,
                server_timestamp: at,
                needs_receipt: false,
                unidentified_sender: false,
                was_plaintext: false,
                report_spam_token: None,
                pni_verified: None,
            },
            body,
        }
    }

    fn text(body: &str, ts: u64) -> DataMessage {
        DataMessage {
            body: Some(body.to_owned()),
            timestamp: Some(ts),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn stored_history_lands_in_the_chats_it_was_filed_under() {
        let dir = std::env::temp_dir().join(format!("signal-tray-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir is writable");
        let path = dir.join("presage.sqlite");
        let url = format!("sqlite://{}", path.display());
        let store = SqliteStore::open_with_passphrase(&url, None, OnNewIdentity::Trust)
            .await
            .expect("store opens");

        let (me, alice, bob) = (aci(1), aci(2), aci(3));
        let group = Thread::Group(GROUP_KEY);
        store
            .save_group(GROUP_KEY, Group::default())
            .await
            .expect("group saved");
        for (sender, ts, body) in [
            (alice, 10, ContentBody::DataMessage(text("from alice", 10))),
            (bob, 11, ContentBody::DataMessage(text("from bob", 11))),
            (
                me,
                12,
                ContentBody::SynchronizeMessage(SyncMessage {
                    content: Some(sync_message::Content::Sent(Sent {
                        timestamp: Some(12),
                        message: Some(text("from me", 12)),
                        ..Default::default()
                    })),
                    ..Default::default()
                }),
            ),
        ] {
            store
                .save_message(&group, imported(sender, me, ts, body))
                .await
                .expect("message saved");
        }
        store
            .save_message(
                &Thread::Contact(ServiceId::Aci(alice)),
                imported(alice, me, 13, ContentBody::DataMessage(text("just us", 13))),
            )
            .await
            .expect("message saved");

        let (tx, rx) = async_channel::unbounded();
        let collect = async {
            let mut messages = Vec::new();
            while let Ok(Event::Messages {
                events, committed, ..
            }) = rx.recv().await
            {
                messages.extend(events);
                if let Some(committed) = committed {
                    let _ = committed.send(());
                }
            }
            messages
        };
        let hand_over = async {
            let result = hand_over_stored_messages(&store, &path, &Names::new(), &tx).await;
            tx.close();
            result
        };
        let (result, events) = future::join(hand_over, collect).await;
        result.expect("hand-over succeeds");

        let mut placed: Vec<_> = events
            .iter()
            .map(|event| match event {
                SignalEvent::Message(NewMessage {
                    conversation,
                    author,
                    body,
                    ..
                }) => (body.as_str(), conversation.clone(), author.clone()),
                other => panic!("unexpected event {other:?}"),
            })
            .collect();
        placed.sort_by_key(|(body, ..)| *body);
        let group = group_conversation(&GROUP_KEY);
        let peer = |id: Aci| peer_id(&ServiceId::Aci(id));
        assert_eq!(
            placed,
            [
                ("from alice", group.clone(), Author::Peer(peer(alice))),
                ("from bob", group.clone(), Author::Peer(peer(bob))),
                ("from me", group, Author::Me),
                (
                    "just us",
                    ConversationId::Direct(peer(alice)),
                    Author::Peer(peer(alice))
                ),
            ],
            "group messages belong to the group chat, and 1:1 threads with \
             people outside the address book are handed over too"
        );
        assert_eq!(
            stored_threads(&path).expect("threads readable"),
            [],
            "handed-over messages are removed from presage's store"
        );
        std::fs::remove_dir_all(&dir).expect("temp dir is removable");
    }
}
