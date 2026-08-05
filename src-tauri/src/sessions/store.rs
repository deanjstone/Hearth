// Session persistence: an append-only JSONL transcript per session + a JSON
// index of metadata. Ported from electron/main/sessions/store.ts, closing a
// gap in Phase 3 (spec deanjstone/Hearth#48) — the renderer's chat flow
// (ChatView.tsx's `ensureActiveSession`/`sessions.get`) needs this before
// `agent_prompt` (agent_commands.rs) is reachable at all.
//
// One real behavioral simplification from the TS source: TS debounces the
// index-file rewrite by 1000ms after a session's first touch (U18's own
// comment: "rewriting the whole pretty-printed index.json per streamed token
// is pure amplification"), keeping an in-memory `pendingBumps` map + timers.
// This port always writes the index through immediately instead — `index.json`
// is a small JSON array (session metadata only, not transcript content), so
// the extra disk writes during a streamed turn are cheap for a desktop app;
// skipping the debounce avoids inventing a background-timer mechanism for a
// pure perf optimization that doesn't change observable behavior (the state
// on disk converges to the same thing either way, just sooner here).
//
// Guarded by one `Mutex<()>` around every read-modify-write sequence — TS's
// single-threaded-interleaved Node event loop doesn't need this, but multiple
// Tauri commands CAN run concurrently across worker threads, and a naive
// read-index/mutate/write-index without a lock would lose updates under
// concurrent calls (e.g. two streamed `sessions_append` calls racing).
// Mirrors the "one Mutex, held briefly per operation" convention
// `AgentHostEngine` already established for this project.
//
// Every write (`create`/`append`/`patch`/`remove`) returns `Result<_, String>`
// and propagates real I/O failures — a disk-full or permissions error must
// surface to the renderer as a rejected `sessions_*` command, not vanish
// silently behind a "successful" response whose data was never actually
// persisted. Reads (`list`/`get`/`search`) still fail open to an empty
// result on a missing/corrupt file — that's the intentional "no session
// history yet" case, not an error to report.
//
// `SessionMeta`/`SessionDetail`/`TranscriptEntry`/etc. carry `serde` derives
// directly (no separate `*Dto` layer, unlike `selfmod_commands.rs`'s
// `StepResultDto`/`SelfModLogEntryDto`) — these are wire-shaped 1:1 mirrors
// of `store.ts`'s own exported types with no internal-vs-external shape
// divergence, the same rationale `agents/agent.rs`'s header comment gives
// for its own wire-contract types.

use crate::agents::agent::SessionUpdate;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WorkspaceKind {
    Code,
    Knowledge,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum TranscriptEntry {
    User { text: String },
    Update { update: SessionUpdate },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionMeta {
    pub id: String,
    pub title: String,
    pub workspace_id: String,
    pub cwd: String,
    /// True when this session targets the Hearth repo itself.
    #[serde(rename = "self")]
    pub is_self: bool,
    /// Developer ('code') vs knowledge-worker ('knowledge') framing.
    pub kind: WorkspaceKind,
    /// The ACP adapter's session id for this conversation, captured on first
    /// prompt. Absent until the first turn.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub acp_session_id: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub archived: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionDetail {
    pub meta: SessionMeta,
    pub entries: Vec<TranscriptEntry>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSearchHit {
    pub meta: SessionMeta,
    /// A text excerpt around the first content match; absent when the hit
    /// was only in the title/cwd.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snippet: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateSessionInput {
    pub title: Option<String>,
    pub workspace_id: String,
    pub cwd: String,
    #[serde(rename = "self")]
    pub is_self: bool,
    pub kind: Option<WorkspaceKind>,
}

/// The searchable text of a transcript entry — matches `entryText` (store.ts).
fn entry_text(e: &TranscriptEntry) -> &str {
    match e {
        TranscriptEntry::User { text } => text,
        TranscriptEntry::Update { update } => match update {
            SessionUpdate::Message { text, .. } | SessionUpdate::Thought { text, .. } => text,
            _ => "",
        },
    }
}

/// Nearest char boundary at or before `idx` — `str::floor_char_boundary` is
/// still unstable, so this walks back by hand.
fn floor_char_boundary(text: &str, mut idx: usize) -> usize {
    while idx > 0 && !text.is_char_boundary(idx) {
        idx -= 1;
    }
    idx
}

/// Nearest char boundary at or after `idx`.
fn ceil_char_boundary(text: &str, mut idx: usize) -> usize {
    while idx < text.len() && !text.is_char_boundary(idx) {
        idx += 1;
    }
    idx
}

/// A whitespace-collapsed window around a match, with ellipses when clipped.
/// Matches `excerpt` (store.ts) — byte-offset based like TS's UTF-16-index
/// based version, but snapped to real char boundaries before slicing (TS
/// string indexing never panics on a mid-codepoint index; Rust byte slicing
/// does).
fn excerpt(text: &str, idx: usize, len: usize, radius: usize) -> String {
    // `idx`/`len` come from a byte-offset `str::find`, but `idx ± radius` can
    // land mid-codepoint for non-ASCII content — snap both ends to real char
    // boundaries before slicing, or this panics (a real crash risk `search`
    // would otherwise hit on non-ASCII transcript content).
    let start = floor_char_boundary(text, idx.saturating_sub(radius));
    let end = ceil_char_boundary(text, (idx + len + radius).min(text.len()));
    let body: String = text[start..end]
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let mut out = String::new();
    if start > 0 {
        out.push_str("… ");
    }
    out.push_str(body.trim());
    if end < text.len() {
        out.push_str(" …");
    }
    out
}

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// `t.toString(36)` (store.ts) — base-36 encoding of a millisecond timestamp,
/// for a compact, roughly-sortable session id.
fn to_base36(mut n: i64) -> String {
    if n == 0 {
        return "0".to_string();
    }
    const DIGITS: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let negative = n < 0;
    if negative {
        n = -n;
    }
    let mut out = Vec::new();
    while n > 0 {
        out.push(DIGITS[(n % 36) as usize]);
        n /= 36;
    }
    if negative {
        out.push(b'-');
    }
    out.reverse();
    String::from_utf8(out).expect("base36 digits are ASCII")
}

pub struct SessionStore {
    base_dir: PathBuf,
    /// Injectable clock so tests are deterministic (same principle as
    /// `boot_watchdog.rs`'s injected `now`, though that one takes it per
    /// call rather than baking it in at construction — `SessionStore`'s own
    /// clock reads happen deep inside several methods, so threading a `now`
    /// parameter through each call site would be far more invasive here).
    /// Defaults to system time.
    now: Box<dyn Fn() -> i64 + Send + Sync>,
    counter: AtomicU64,
    /// Guards every read-index/mutate/write-index sequence — see this
    /// module's header comment.
    write_lock: Mutex<()>,
}

impl SessionStore {
    pub fn new(base_dir: impl Into<PathBuf>) -> Self {
        Self::with_clock(base_dir, now_millis)
    }

    pub fn with_clock(
        base_dir: impl Into<PathBuf>,
        now: impl Fn() -> i64 + Send + Sync + 'static,
    ) -> Self {
        Self {
            base_dir: base_dir.into(),
            now: Box::new(now),
            counter: AtomicU64::new(0),
            write_lock: Mutex::new(()),
        }
    }

    fn index_path(&self) -> PathBuf {
        self.base_dir.join("index.json")
    }
    fn transcripts_dir(&self) -> PathBuf {
        self.base_dir.join("transcripts")
    }
    fn transcript_path(&self, id: &str) -> PathBuf {
        self.transcripts_dir().join(format!("{id}.jsonl"))
    }

    fn read_index(&self) -> Vec<SessionMeta> {
        fs::read_to_string(self.index_path())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// Write-through-temp-then-rename: a concurrent reader always sees either
    /// the old or the new file whole, never a partial write.
    fn write_index(&self, index: &[SessionMeta]) -> std::io::Result<()> {
        fs::create_dir_all(&self.base_dir)?;
        let json = serde_json::to_string_pretty(index).map_err(std::io::Error::other)?;
        let tmp = self.base_dir.join("index.json.tmp");
        fs::write(&tmp, json)?;
        fs::rename(&tmp, self.index_path())
    }

    /// `Ok(None)` when `id` isn't found (not an error); `Err` only for a
    /// real write failure, so a disk error surfaces to the renderer instead
    /// of silently vanishing.
    fn patch(
        &self,
        id: &str,
        f: impl FnOnce(&mut SessionMeta),
    ) -> Result<Option<SessionMeta>, String> {
        let _guard = self.write_lock.lock().unwrap();
        let mut index = self.read_index();
        let Some(meta) = index.iter_mut().find(|m| m.id == id) else {
            return Ok(None);
        };
        f(meta);
        let updated = meta.clone();
        self.write_index(&index).map_err(|e| e.to_string())?;
        Ok(Some(updated))
    }

    /// A session created but never used: `append`/`rename` both bump
    /// `updated_at` strictly past `created_at` (see `bumped`), so "never
    /// touched" ⟺ `updated_at == created_at`. Hidden from lists, swept on
    /// the next `create`.
    fn untouched(m: &SessionMeta) -> bool {
        m.updated_at == m.created_at
    }

    /// Always strictly greater than `created_at`, even when the clock hasn't
    /// advanced a millisecond since create — so `untouched` can never
    /// mis-flag a session that actually has content.
    fn bumped(&self, m: &SessionMeta) -> i64 {
        (self.now)().max(m.created_at + 1)
    }

    /// Non-archived, used sessions — newest activity first.
    pub fn list(&self) -> Vec<SessionMeta> {
        let mut metas: Vec<SessionMeta> = self
            .read_index()
            .into_iter()
            .filter(|m| !m.archived && !Self::untouched(m))
            .collect();
        metas.sort_by_key(|m| std::cmp::Reverse(m.updated_at));
        metas
    }

    pub fn create(&self, input: CreateSessionInput) -> Result<SessionMeta, String> {
        let _guard = self.write_lock.lock().unwrap();
        let t = (self.now)();
        let meta = SessionMeta {
            id: format!(
                "s_{}_{}",
                to_base36(t),
                self.counter.fetch_add(1, Ordering::SeqCst)
            ),
            title: input
                .title
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty())
                .unwrap_or_else(|| "New session".to_string()),
            workspace_id: input.workspace_id,
            cwd: input.cwd,
            is_self: input.is_self,
            kind: input.kind.unwrap_or(if input.is_self {
                WorkspaceKind::Code
            } else {
                WorkspaceKind::Knowledge
            }),
            acp_session_id: None,
            created_at: t,
            updated_at: t,
            archived: false,
        };
        // Sweep previously-untouched sessions so abandoned empties never
        // accumulate.
        let existing = self.read_index();
        let (stale, keep): (Vec<_>, Vec<_>) = existing.into_iter().partition(Self::untouched);
        let mut next = vec![meta.clone()];
        next.extend(keep);
        fs::create_dir_all(self.transcripts_dir()).map_err(|e| e.to_string())?;
        self.write_index(&next).map_err(|e| e.to_string())?;
        fs::write(self.transcript_path(&meta.id), "").map_err(|e| e.to_string())?;
        for m in &stale {
            let _ = fs::remove_file(self.transcript_path(&m.id));
        }
        Ok(meta)
    }

    /// Metadata only (no transcript read) — used by `TurnCoordinator` to
    /// look up the ACP session id (via the `SessionMetaStore` impl below).
    pub fn read_meta(&self, id: &str) -> Option<SessionMeta> {
        self.read_index().into_iter().find(|m| m.id == id)
    }

    /// Record the ACP adapter's session id so the session can later be
    /// resumed.
    pub fn set_acp_session_id(
        &self,
        id: &str,
        acp_session_id: &str,
    ) -> Result<Option<SessionMeta>, String> {
        let acp_session_id = acp_session_id.to_string();
        self.patch(id, |m| m.acp_session_id = Some(acp_session_id))
    }

    fn read_entries(&self, id: &str) -> Vec<TranscriptEntry> {
        fs::read_to_string(self.transcript_path(id))
            .ok()
            .map(|raw| {
                raw.lines()
                    .filter(|l| !l.is_empty())
                    .filter_map(|l| serde_json::from_str(l).ok())
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn get(&self, id: &str) -> Option<SessionDetail> {
        let meta = self.read_index().into_iter().find(|m| m.id == id)?;
        let entries = self.read_entries(&meta.id);
        Some(SessionDetail { meta, entries })
    }

    /// Search titles, cwds, and transcript content. Empty query returns the
    /// full list (newest-first, same exclusions as `list`).
    pub fn search(&self, query: &str) -> Vec<SessionSearchHit> {
        let metas = self.list();
        let q = query.trim().to_lowercase();
        if q.is_empty() {
            return metas
                .into_iter()
                .map(|meta| SessionSearchHit {
                    meta,
                    snippet: None,
                })
                .collect();
        }
        let mut hits = Vec::new();
        for meta in metas {
            let text: String = self
                .read_entries(&meta.id)
                .iter()
                .map(entry_text)
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join("\n");
            if let Some(idx) = text.to_lowercase().find(&q) {
                let snippet = excerpt(&text, idx, q.len(), 64);
                hits.push(SessionSearchHit {
                    meta,
                    snippet: Some(snippet),
                });
            } else if format!("{} {}", meta.title, meta.cwd)
                .to_lowercase()
                .contains(&q)
            {
                hits.push(SessionSearchHit {
                    meta,
                    snippet: None,
                });
            }
        }
        hits
    }

    /// Append transcript entries and bump `updated_at` immediately (no
    /// debounce — see this module's header comment). Auto-titles from the
    /// first user entry's text (60 chars), only while the title is still the
    /// "New session" placeholder.
    pub fn append(&self, id: &str, entries: &[TranscriptEntry]) -> Result<(), String> {
        if entries.is_empty() {
            return Ok(());
        }
        fs::create_dir_all(self.transcripts_dir()).map_err(|e| e.to_string())?;
        let jsonl: String = entries
            .iter()
            .filter_map(|e| serde_json::to_string(e).ok())
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        {
            use std::io::Write;
            fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(self.transcript_path(id))
                .and_then(|mut f| f.write_all(jsonl.as_bytes()))
                .map_err(|e| e.to_string())?;
        }
        let first_user_text = entries.iter().find_map(|e| match e {
            TranscriptEntry::User { text } => Some(text.clone()),
            TranscriptEntry::Update { .. } => None,
        });
        self.patch(id, |m| {
            let bumped = self.bumped(m);
            if m.title == "New session" {
                if let Some(text) = &first_user_text {
                    m.title = text.chars().take(60).collect();
                }
            }
            m.updated_at = bumped;
        })
        .map(|_| ())
    }

    pub fn rename(&self, id: &str, title: &str) -> Result<Option<SessionMeta>, String> {
        let title = title.trim().to_string();
        self.patch(id, |m| {
            let bumped = self.bumped(m);
            if !title.is_empty() {
                m.title = title;
            }
            m.updated_at = bumped;
        })
    }

    pub fn set_kind(&self, id: &str, kind: WorkspaceKind) -> Result<Option<SessionMeta>, String> {
        self.patch(id, |m| m.kind = kind)
    }

    pub fn archive(&self, id: &str) -> Result<(), String> {
        self.patch(id, |m| m.archived = true).map(|_| ())
    }

    pub fn remove(&self, id: &str) -> Result<(), String> {
        let _guard = self.write_lock.lock().unwrap();
        let remaining: Vec<SessionMeta> = self
            .read_index()
            .into_iter()
            .filter(|m| m.id != id)
            .collect();
        self.write_index(&remaining).map_err(|e| e.to_string())?;
        let _ = fs::remove_file(self.transcript_path(id));
        Ok(())
    }

    pub fn duplicate(&self, id: &str) -> Result<Option<SessionMeta>, String> {
        let Some(detail) = self.get(id) else {
            return Ok(None);
        };
        let copy = self.create(CreateSessionInput {
            title: Some(format!("{} (copy)", detail.meta.title)),
            workspace_id: detail.meta.workspace_id,
            cwd: detail.meta.cwd,
            is_self: detail.meta.is_self,
            kind: Some(detail.meta.kind),
        })?;
        self.append(&copy.id, &detail.entries)?;
        Ok(Some(copy))
    }

    /// All transcript ids on disk (diagnostic / cleanup) — not currently
    /// exposed via a Tauri command, kept for parity with `transcriptIds`.
    #[allow(dead_code)]
    pub fn transcript_ids(&self) -> Vec<String> {
        fs::read_dir(self.transcripts_dir())
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .filter_map(|e| {
                        e.file_name()
                            .to_str()
                            .map(|s| s.trim_end_matches(".jsonl").to_string())
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

impl crate::turn_coordinator::SessionMetaStore for SessionStore {
    fn get_meta(&self, key: &str) -> Option<crate::turn_coordinator::SessionMeta> {
        self.read_meta(key)
            .map(|m| crate::turn_coordinator::SessionMeta {
                acp_session_id: m.acp_session_id,
            })
    }

    fn set_acp_session_id(&self, key: &str, acp_session_id: &str) {
        // Fire-and-forget, matching the trait's own signature (no Result to
        // return here) — a failed write just means the next resume attempt
        // falls back to a fresh session instead of resuming, not data loss.
        let _ = SessionStore::set_acp_session_id(self, key, acp_session_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn store(dir: &Path) -> SessionStore {
        SessionStore::with_clock(dir, || 1_000)
    }

    fn store_at(dir: &Path, t: i64) -> SessionStore {
        SessionStore::with_clock(dir, move || t)
    }

    fn input(cwd: &str) -> CreateSessionInput {
        CreateSessionInput {
            title: None,
            workspace_id: "hearth".to_string(),
            cwd: cwd.to_string(),
            is_self: true,
            kind: None,
        }
    }

    #[test]
    fn create_defaults_title_and_kind_from_self() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        let meta = s.create(input("/repo")).unwrap();
        assert_eq!(meta.title, "New session");
        assert_eq!(meta.kind, WorkspaceKind::Code);
        assert!(meta.acp_session_id.is_none());
        assert_eq!(meta.created_at, meta.updated_at);
    }

    #[test]
    fn knowledge_kind_defaults_when_not_self() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        let mut i = input("/notes");
        i.is_self = false;
        let meta = s.create(i).unwrap();
        assert_eq!(meta.kind, WorkspaceKind::Knowledge);
    }

    #[test]
    fn list_excludes_archived_and_untouched_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        let untouched = s.create(input("/a")).unwrap();
        let touched = s.create(input("/b")).unwrap();
        s.append(
            &touched.id,
            &[TranscriptEntry::User {
                text: "hi".to_string(),
            }],
        )
        .unwrap();
        let archived = s.create(input("/c")).unwrap();
        s.append(
            &archived.id,
            &[TranscriptEntry::User {
                text: "hi".to_string(),
            }],
        )
        .unwrap();
        s.archive(&archived.id).unwrap();

        let listed = s.list();
        let ids: Vec<&str> = listed.iter().map(|m| m.id.as_str()).collect();
        assert!(ids.contains(&touched.id.as_str()));
        assert!(!ids.contains(&untouched.id.as_str()));
        assert!(!ids.contains(&archived.id.as_str()));
    }

    #[test]
    fn create_sweeps_prior_untouched_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        let stale = s.create(input("/a")).unwrap();
        assert!(s.get(&stale.id).is_some());
        let _second = s.create(input("/b")).unwrap();
        // The stale, never-touched session's transcript file is gone and it's
        // dropped from the index (get() reads the index first).
        assert!(s.get(&stale.id).is_none());
    }

    #[test]
    fn append_bumps_updated_at_and_auto_titles_from_first_user_text() {
        let dir = tempfile::tempdir().unwrap();
        let s = store_at(dir.path(), 1_000);
        let meta = s.create(input("/a")).unwrap();
        let s2 = store_at(dir.path(), 2_000);
        s2.append(
            &meta.id,
            &[TranscriptEntry::User {
                text: "hello there".to_string(),
            }],
        )
        .unwrap();
        let detail = s2.get(&meta.id).unwrap();
        assert_eq!(detail.meta.title, "hello there");
        assert_eq!(detail.meta.updated_at, 2_000);
        assert_eq!(detail.entries.len(), 1);
    }

    #[test]
    fn append_does_not_overwrite_a_title_the_user_already_set() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        let meta = s.create(input("/a")).unwrap();
        s.rename(&meta.id, "My title").unwrap();
        s.append(
            &meta.id,
            &[TranscriptEntry::User {
                text: "hello".to_string(),
            }],
        )
        .unwrap();
        assert_eq!(s.get(&meta.id).unwrap().meta.title, "My title");
    }

    #[test]
    fn rename_trims_and_bumps_but_ignores_blank_titles() {
        let dir = tempfile::tempdir().unwrap();
        let s = store_at(dir.path(), 1_000);
        let meta = s.create(input("/a")).unwrap();
        let s2 = store_at(dir.path(), 5_000);
        let renamed = s2.rename(&meta.id, "  Renamed  ").unwrap().unwrap();
        assert_eq!(renamed.title, "Renamed");
        assert_eq!(renamed.updated_at, 5_000);
        let blanked = s2.rename(&meta.id, "   ").unwrap().unwrap();
        assert_eq!(blanked.title, "Renamed");
    }

    #[test]
    fn set_acp_session_id_round_trips_through_session_meta_store() {
        use crate::turn_coordinator::SessionMetaStore;
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        let meta = s.create(input("/a")).unwrap();
        assert!(SessionMetaStore::get_meta(&s, &meta.id)
            .unwrap()
            .acp_session_id
            .is_none());
        SessionMetaStore::set_acp_session_id(&s, &meta.id, "acp-123");
        assert_eq!(
            SessionMetaStore::get_meta(&s, &meta.id)
                .unwrap()
                .acp_session_id
                .as_deref(),
            Some("acp-123")
        );
    }

    #[test]
    fn get_meta_for_an_unknown_session_returns_none() {
        use crate::turn_coordinator::SessionMetaStore;
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        assert!(SessionMetaStore::get_meta(&s, "nope").is_none());
    }

    #[test]
    fn archive_hides_but_does_not_delete() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        let meta = s.create(input("/a")).unwrap();
        s.append(
            &meta.id,
            &[TranscriptEntry::User {
                text: "hi".to_string(),
            }],
        )
        .unwrap();
        s.archive(&meta.id).unwrap();
        assert!(s.list().is_empty());
        assert!(s.get(&meta.id).is_some());
    }

    #[test]
    fn remove_deletes_from_index_and_transcript() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        let meta = s.create(input("/a")).unwrap();
        s.append(
            &meta.id,
            &[TranscriptEntry::User {
                text: "hi".to_string(),
            }],
        )
        .unwrap();
        s.remove(&meta.id).unwrap();
        assert!(s.get(&meta.id).is_none());
        assert!(!s.transcript_path(&meta.id).exists());
    }

    #[test]
    fn duplicate_copies_entries_and_titles_with_a_copy_suffix() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        let meta = s.create(input("/a")).unwrap();
        s.rename(&meta.id, "Original").unwrap();
        s.append(
            &meta.id,
            &[TranscriptEntry::User {
                text: "hi".to_string(),
            }],
        )
        .unwrap();
        let copy = s.duplicate(&meta.id).unwrap().unwrap();
        assert_eq!(copy.title, "Original (copy)");
        assert_ne!(copy.id, meta.id);
        let detail = s.get(&copy.id).unwrap();
        assert_eq!(detail.entries.len(), 1);
    }

    #[test]
    fn duplicate_of_an_unknown_session_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        assert!(s.duplicate("nope").unwrap().is_none());
    }

    #[test]
    fn search_matches_title_cwd_and_transcript_content() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        let a = s.create(input("/repo-a")).unwrap();
        s.rename(&a.id, "Fix the flaky test").unwrap();
        s.append(
            &a.id,
            &[TranscriptEntry::User {
                text: "please help with widgets".to_string(),
            }],
        )
        .unwrap();
        let b = s.create(input("/repo-b")).unwrap();
        s.rename(&b.id, "Unrelated").unwrap();
        s.append(
            &b.id,
            &[TranscriptEntry::User {
                text: "totally different topic".to_string(),
            }],
        )
        .unwrap();

        let by_title = s.search("flaky");
        assert_eq!(by_title.len(), 1);
        assert_eq!(by_title[0].meta.id, a.id);
        assert!(by_title[0].snippet.is_none());

        let by_content = s.search("widgets");
        assert_eq!(by_content.len(), 1);
        assert_eq!(by_content[0].meta.id, a.id);
        assert!(by_content[0].snippet.is_some());

        let empty_query = s.search("");
        assert_eq!(empty_query.len(), 2);
    }

    #[test]
    fn search_does_not_panic_on_non_ascii_content_near_the_match_window() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        let meta = s.create(input("/a")).unwrap();
        // Multi-byte UTF-8 padding on both sides of the match, well within
        // the excerpt radius — a byte-index slice that isn't char-boundary
        // snapped would panic here.
        let text = "日本語のテキストです widgets 日本語のテキストです".to_string();
        s.append(&meta.id, &[TranscriptEntry::User { text }])
            .unwrap();
        let hits = s.search("widgets");
        assert_eq!(hits.len(), 1);
        assert!(hits[0].snippet.is_some());
    }

    #[test]
    fn create_surfaces_a_write_failure_instead_of_silently_swallowing_it() {
        // base_dir itself is a file, not a directory: every write inside it
        // must fail, and create() must report that instead of returning an
        // Ok(SessionMeta) whose transcript was never actually written.
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("not-a-dir");
        std::fs::write(&base, "").unwrap();
        let s = store(&base);
        assert!(s.create(input("/a")).is_err());
    }

    #[test]
    fn to_base36_matches_js_number_tostring_36() {
        assert_eq!(to_base36(0), "0");
        assert_eq!(to_base36(35), "z");
        assert_eq!(to_base36(36), "10");
        assert_eq!(to_base36(1_000_000), "lfls");
    }
}
