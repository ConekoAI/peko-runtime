//! Attachments in direct messages (ADR-088).
//!
//! The local operator sends files by path. The runtime copies each one
//! into the principal's workspace (`attachments/<YYYY-MM-DD>/<8 hex>-<name>`),
//! records it on the inbound DM post, and tells the turn about it with
//! one note line per file. Images, audio, and video also reach a model
//! that takes them as media blocks (`agents::subagent_executor::MediaInput`).

use std::path::{Path, PathBuf};

use peko_protocol::channel::Attachment;

use crate::agents::subagent_executor::{MediaInput, MediaKind};

/// At most this many files per message.
pub(crate) const MAX_FILES: usize = 8;
/// At most this many bytes per file.
pub(crate) const MAX_FILE_BYTES: u64 = 20 * 1024 * 1024;
/// ADR-094: at most this many bytes per file in a conversation with
/// anyone on another runtime or the Hub — the bytes cross PekoHub.
pub(crate) const REMOTE_MAX_FILE_BYTES: u64 = 10 * 1024 * 1024;
/// ADR-094: at most this many bytes per message, all files together,
/// in such a conversation.
pub(crate) const REMOTE_MAX_MESSAGE_BYTES: u64 = 25 * 1024 * 1024;
/// Workspace directory the copies live under.
pub(crate) const DIR: &str = "attachments";

/// What a message may carry (ADR-088 D1, ADR-094).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Limits {
    pub max_file: u64,
    /// All files together; `None` for no limit beyond the per-file one.
    pub max_total: Option<u64>,
}

impl Limits {
    /// Files that stay on this host.
    pub(crate) const LOCAL: Self = Self {
        max_file: MAX_FILE_BYTES,
        max_total: None,
    };
    /// Files that cross PekoHub to another runtime.
    pub(crate) const REMOTE: Self = Self {
        max_file: REMOTE_MAX_FILE_BYTES,
        max_total: Some(REMOTE_MAX_MESSAGE_BYTES),
    };
}

/// Validate every path against `limits`: absolute, a regular file, in
/// size. The error is the user-facing reason.
pub(crate) async fn validate(paths: &[String], limits: Limits) -> Result<Vec<PathBuf>, String> {
    if paths.len() > MAX_FILES {
        return Err(format!(
            "too many attachments ({}); at most {MAX_FILES} per message",
            paths.len()
        ));
    }
    let mut sources = Vec::with_capacity(paths.len());
    let mut total = 0u64;
    for raw in paths {
        let path = PathBuf::from(raw);
        if !path.is_absolute() {
            return Err(format!("attachment path must be absolute: {raw}"));
        }
        let meta = tokio::fs::metadata(&path)
            .await
            .map_err(|e| format!("cannot read attachment {raw}: {e}"))?;
        if !meta.is_file() {
            return Err(format!("attachment is not a regular file: {raw}"));
        }
        if meta.len() > limits.max_file {
            return Err(format!(
                "attachment {raw} is {}; the limit is {}{}",
                human_size(meta.len()),
                human_size(limits.max_file),
                remote_note(limits)
            ));
        }
        total += meta.len();
        sources.push(path);
    }
    if let Some(max_total) = limits.max_total {
        if total > max_total {
            return Err(format!(
                "these attachments are {} together; the limit is {} per message{}",
                human_size(total),
                human_size(max_total),
                remote_note(limits)
            ));
        }
    }
    Ok(sources)
}

fn remote_note(limits: Limits) -> &'static str {
    if limits == Limits::REMOTE {
        " when the conversation includes someone on another computer"
    } else {
        ""
    }
}

/// Every attachment of a message, together, within `limits` — for files
/// already copied (a peko's post to someone on another runtime).
pub(crate) fn check(attachments: &[Attachment], limits: Limits) -> Result<(), String> {
    for a in attachments {
        if a.size > limits.max_file {
            return Err(format!(
                "attachment {} is {}; the limit is {}{}",
                a.name,
                human_size(a.size),
                human_size(limits.max_file),
                remote_note(limits)
            ));
        }
    }
    let total: u64 = attachments.iter().map(|a| a.size).sum();
    match limits.max_total {
        Some(max_total) if total > max_total => Err(format!(
            "these attachments are {} together; the limit is {} per message{}",
            human_size(total),
            human_size(max_total),
            remote_note(limits)
        )),
        _ => Ok(()),
    }
}

/// Validate every path, then copy each file into the workspace. Nothing
/// is copied unless every file passes, so a refused request leaves no
/// trace. The error is the user-facing reason.
pub(crate) async fn ingest(workspace: &Path, paths: &[String]) -> Result<Vec<Attachment>, String> {
    let sources = validate(paths, Limits::LOCAL).await?;
    let named: Vec<(PathBuf, String)> = sources
        .into_iter()
        .map(|source| {
            let name = source
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "file".to_string());
            (source, name)
        })
        .collect();
    copy_into_workspace(workspace, &named).await
}

/// Copy each `(source, name)` into the workspace under `name` (ADR-088
/// D2). Files a remote post brought (ADR-094) keep the name the sender
/// gave them.
pub(crate) async fn copy_into_workspace(
    workspace: &Path,
    files: &[(PathBuf, String)],
) -> Result<Vec<Attachment>, String> {
    let day = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let dir = Path::new(DIR).join(&day);
    tokio::fs::create_dir_all(workspace.join(&dir))
        .await
        .map_err(|e| format!("cannot create the attachments directory: {e}"))?;
    let mut attachments = Vec::with_capacity(files.len());
    for (source, name) in files {
        let id = &uuid::Uuid::new_v4().simple().to_string()[..8];
        let relative = dir.join(format!("{id}-{}", safe_file_name(name)));
        let dest = workspace.join(&relative);
        let size = tokio::fs::copy(source, &dest)
            .await
            .map_err(|e| format!("cannot copy attachment {}: {e}", source.display()))?;
        let mime = crate::common::media::mime_of(&dest).await;
        attachments.push(Attachment {
            name: name.clone(),
            path: relative.to_string_lossy().replace('\\', "/"),
            mime,
            size,
            sha256: None,
        });
    }
    Ok(attachments)
}

// ---------------------------------------------------------------------------
// The channel's file store (ADR-094)
// ---------------------------------------------------------------------------

/// Whether `s` is a lowercase hex SHA-256 digest — the only names the
/// file store holds, so a digest from the wire never escapes it.
pub(crate) fn is_digest(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Where the file store keeps `name` with `digest`:
/// `<digest>/<safe name>`. The name keeps its extension, so clients
/// preview, play, and open the file as what it is. `None` for anything
/// but a digest.
pub(crate) fn stored(files_dir: &Path, digest: &str, name: &str) -> Option<PathBuf> {
    is_digest(digest).then(|| files_dir.join(digest).join(safe_file_name(name)))
}

/// Any copy of `digest` in the store — every name under it holds the same
/// bytes — for serving a runtime that asks by digest alone.
pub(crate) fn any_stored(files_dir: &Path, digest: &str) -> Option<PathBuf> {
    if !is_digest(digest) {
        return None;
    }
    std::fs::read_dir(files_dir.join(digest))
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| path.is_file())
}

/// Put a received file, checked against `digest`, in the store under
/// `name`.
pub(crate) async fn place(
    files_dir: &Path,
    from: &Path,
    digest: &str,
    name: &str,
) -> Result<PathBuf, String> {
    let dest = stored(files_dir, digest, name).ok_or("not a digest")?;
    if let Some(dir) = dest.parent() {
        tokio::fs::create_dir_all(dir)
            .await
            .map_err(|e| format!("cannot create the file store: {e}"))?;
    }
    tokio::fs::rename(from, &dest)
        .await
        .map_err(|e| format!("cannot store the file: {e}"))?;
    Ok(dest)
}

/// SHA-256 of a file, lowercase hex.
pub(crate) async fn sha256_of(path: &Path) -> std::io::Result<String> {
    use sha2::{Digest, Sha256};
    use tokio::io::AsyncReadExt;
    let mut file = tokio::fs::File::open(path).await?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

/// Validate every path against `limits`, then copy each file into the
/// channel's file store under its digest. The posts records have no
/// `path` (ADR-094): readers find the copy by digest. Nothing is copied
/// unless every file passes.
pub(crate) async fn ingest_into_channel(
    files_dir: &Path,
    paths: &[String],
    limits: Limits,
) -> Result<Vec<Attachment>, String> {
    let sources = validate(paths, limits).await?;
    let mut attachments = Vec::with_capacity(sources.len());
    for source in sources {
        let name = source
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "file".to_string());
        let (digest, size) = store_file(files_dir, &source, &name).await?;
        let mime = crate::common::media::mime_of(&source).await;
        attachments.push(Attachment {
            name,
            path: String::new(),
            mime,
            size,
            sha256: Some(digest),
        });
    }
    Ok(attachments)
}

/// Copy `source` into the file store under `name`; its digest and size.
/// A copy already there is kept.
pub(crate) async fn store_file(
    files_dir: &Path,
    source: &Path,
    name: &str,
) -> Result<(String, u64), String> {
    tokio::fs::create_dir_all(files_dir)
        .await
        .map_err(|e| format!("cannot create the channel's file store: {e}"))?;
    let digest = sha256_of(source)
        .await
        .map_err(|e| format!("cannot read attachment {}: {e}", source.display()))?;
    let dest = stored(files_dir, &digest, name).ok_or("not a digest")?;
    if let Ok(meta) = tokio::fs::metadata(&dest).await {
        return Ok((digest, meta.len()));
    }
    let staging = files_dir.join(format!(
        "{digest}.{}.tmp",
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    ));
    let size = tokio::fs::copy(source, &staging)
        .await
        .map_err(|e| format!("cannot copy attachment {}: {e}", source.display()))?;
    place(files_dir, &staging, &digest, name).await?;
    Ok((digest, size))
}

/// Give each of a peko's attachments a copy in the channel's file store
/// and its digest, so a post can cross to another runtime (ADR-094).
pub(crate) async fn share_with_channel(
    files_dir: &Path,
    workspace: &Path,
    attachments: Vec<Attachment>,
) -> Result<Vec<Attachment>, String> {
    let mut shared = Vec::with_capacity(attachments.len());
    for a in attachments {
        let (digest, _) = store_file(files_dir, &absolute(workspace, &a), &a.name).await?;
        shared.push(Attachment {
            sha256: Some(digest),
            ..a
        });
    }
    Ok(shared)
}

/// Whether `channel` includes anyone on another runtime or the Hub, so
/// a file posted there crosses PekoHub (ADR-094 limits apply).
pub(crate) async fn crosses_runtimes(
    port: &dyn peko_channel::ChannelPort,
    channel: &peko_channel::ChannelId,
) -> peko_channel::Result<bool> {
    let members = port.list_members(channel).await?;
    let provenance = port.members_with_attribution(channel).await?;
    Ok(members
        .iter()
        .any(|m| m.is_hub_route() || m.route_runtime().is_some())
        || provenance.iter().any(|m| m.runtime_id.is_some()))
}

/// Where a post's attachment is on this host: the peko's copy when the
/// post names one, else the channel's stored copy once it is here.
/// `None` while a remote file has not arrived.
pub(crate) fn locate(
    workspace: Option<&Path>,
    files_dir: Option<&Path>,
    attachment: &Attachment,
) -> Option<PathBuf> {
    if !attachment.path.is_empty() {
        if let Some(workspace) = workspace {
            return Some(absolute(workspace, attachment));
        }
    }
    let stored = stored(files_dir?, attachment.sha256.as_deref()?, &attachment.name)?;
    stored.is_file().then_some(stored)
}

/// The text a turn sees: the message, then one note per attachment
/// (ADR-088 D4).
pub(crate) fn turn_text(message: &str, workspace: &Path, attachments: &[Attachment]) -> String {
    if attachments.is_empty() {
        return message.to_string();
    }
    let notes = attachments
        .iter()
        .map(|a| {
            format!(
                "[attached: {} ({}, {}) at {}]",
                a.name,
                a.mime,
                human_size(a.size),
                absolute(workspace, a).display()
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    if message.trim().is_empty() {
        notes
    } else {
        format!("{message}\n\n{notes}")
    }
}

/// The attachments a model may take as media blocks: images, audio,
/// and video within the per-kind block limit.
pub(crate) fn media(workspace: &Path, attachments: &[Attachment]) -> Vec<MediaInput> {
    attachments
        .iter()
        .filter_map(|a| {
            let kind = MediaKind::from_mime(&a.mime)?;
            (a.size <= kind.max_block_bytes()).then(|| MediaInput {
                path: absolute(workspace, a),
                mime: a.mime.clone(),
                kind,
            })
        })
        .collect()
}

/// An attachment's absolute path on this host. The stored path is
/// `/`-separated on every platform; joining it part by part gives native
/// separators (Windows got `C:\ws\attachments/2026-10-30/…`).
pub(crate) fn absolute(workspace: &Path, attachment: &Attachment) -> PathBuf {
    let mut path = workspace.to_path_buf();
    path.extend(attachment.path.split('/'));
    path
}

/// A remote post's files for the peko's turn (ADR-094): each one that
/// arrived in the channel's file store is copied into the workspace; the
/// message gains a note for each one that did not. Returns the message
/// and the workspace copies.
pub(crate) async fn received_for_turn(
    workspace: &Path,
    files_dir: Option<&Path>,
    message: &str,
    files: &[Attachment],
) -> (String, Vec<Attachment>) {
    let mut here = Vec::new();
    let mut missing = Vec::new();
    for a in files {
        match locate(None, files_dir, a) {
            Some(path) => here.push((path, a.name.clone())),
            None => missing.push(a),
        }
    }
    let copies = match copy_into_workspace(workspace, &here).await {
        Ok(copies) => copies,
        Err(e) => {
            tracing::warn!("remote attachments not copied into the workspace: {e}");
            Vec::new()
        }
    };
    if missing.is_empty() && copies.len() == here.len() {
        return (message.to_string(), copies);
    }
    let mut notes: Vec<String> = missing
        .iter()
        .map(|a| {
            format!(
                "[attached: {} ({}, {}) — it did not arrive; ask them to send it again if you need it]",
                a.name,
                a.mime,
                human_size(a.size)
            )
        })
        .collect();
    if copies.len() != here.len() {
        notes.push("[some attached files could not be saved to your workspace]".to_string());
    }
    let notes = notes.join("\n");
    let message = if message.trim().is_empty() {
        notes
    } else {
        format!("{message}\n\n{notes}")
    };
    (message, copies)
}

/// Keep letters, digits, `.`, `-`, and `_`; everything else becomes `_`.
fn safe_file_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let cleaned = cleaned.trim_start_matches('.');
    let cleaned: String = cleaned
        .chars()
        .rev()
        .take(100)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    if cleaned.is_empty() {
        "file".to_string()
    } else {
        cleaned
    }
}

fn human_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    let b = bytes as f64;
    if b < KB {
        format!("{bytes} B")
    } else if b < KB * KB {
        format!("{:.1} KB", b / KB)
    } else {
        format!("{:.1} MB", b / (KB * KB))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR";

    #[tokio::test]
    async fn ingest_copies_into_the_workspace_and_sniffs_images() {
        let src = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let photo = src.path().join("my photo.jpg"); // PNG bytes, wrong extension
        std::fs::write(&photo, PNG).unwrap();
        let notes = src.path().join("notes.md");
        std::fs::write(&notes, "# hi").unwrap();

        let got = ingest(
            workspace.path(),
            &[photo.display().to_string(), notes.display().to_string()],
        )
        .await
        .unwrap();

        assert_eq!(got.len(), 2);
        assert_eq!(got[0].name, "my photo.jpg");
        assert_eq!(got[0].mime, "image/png");
        assert_eq!(got[0].size, PNG.len() as u64);
        assert!(got[0].path.starts_with("attachments/"));
        assert!(got[0].path.ends_with("-my_photo.jpg"));
        assert_eq!(
            std::fs::read(workspace.path().join(&got[0].path)).unwrap(),
            PNG
        );
        assert_eq!(got[1].mime, "text/markdown");

        let media = media(workspace.path(), &got);
        assert_eq!(media.len(), 1);
        assert_eq!(media[0].mime, "image/png");
        assert_eq!(media[0].kind, MediaKind::Image);
    }

    #[tokio::test]
    async fn ingest_refuses_before_copying_anything() {
        let src = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let ok = src.path().join("a.txt");
        std::fs::write(&ok, "a").unwrap();

        let relative = ingest(workspace.path(), &["a.txt".into()]).await;
        assert!(relative.unwrap_err().contains("absolute"));

        let dir = ingest(
            workspace.path(),
            &[ok.display().to_string(), src.path().display().to_string()],
        )
        .await;
        assert!(dir.unwrap_err().contains("not a regular file"));

        let gone = src.path().join("no-such-file.png");
        let missing = ingest(workspace.path(), &[gone.display().to_string()]).await;
        assert!(missing.unwrap_err().contains("cannot read"));

        let many = vec![ok.display().to_string(); MAX_FILES + 1];
        assert!(ingest(workspace.path(), &many)
            .await
            .unwrap_err()
            .contains("too many"));

        assert!(!workspace.path().join(DIR).exists());
    }

    #[tokio::test]
    async fn audio_and_video_are_media_too() {
        let src = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let wav = src.path().join("voice.bin"); // sniffed, not named
        std::fs::write(&wav, b"RIFF\x24\0\0\0WAVEfmt ").unwrap();
        let clip = src.path().join("clip.mov");
        std::fs::write(&clip, b"\0\0\0\x14ftypqt  ").unwrap();
        let got = ingest(
            workspace.path(),
            &[wav.display().to_string(), clip.display().to_string()],
        )
        .await
        .unwrap();
        assert_eq!(got[0].mime, "audio/wav");
        assert_eq!(got[1].mime, "video/quicktime");
        let kinds: Vec<_> = media(workspace.path(), &got)
            .iter()
            .map(|m| m.kind)
            .collect();
        assert_eq!(kinds, vec![MediaKind::Audio, MediaKind::Video]);
    }

    #[test]
    fn turn_text_adds_one_note_per_file() {
        let workspace = Path::new("/w");
        let a = Attachment {
            name: "photo.png".into(),
            path: "attachments/2026-10-30/ab12cd34-photo.png".into(),
            mime: "image/png".into(),
            size: 1_258_291,
            sha256: None,
        };
        let at = ["attachments", "2026-10-30", "ab12cd34-photo.png"]
            .iter()
            .fold(workspace.to_path_buf(), |p, part| p.join(part));
        assert_eq!(
            turn_text("look", workspace, std::slice::from_ref(&a)),
            format!(
                "look\n\n[attached: photo.png (image/png, 1.2 MB) at {}]",
                at.display()
            )
        );
        #[cfg(unix)]
        assert_eq!(
            at,
            Path::new("/w/attachments/2026-10-30/ab12cd34-photo.png")
        );
        assert!(turn_text("", workspace, &[a]).starts_with("[attached: "));
        assert_eq!(turn_text("plain", workspace, &[]), "plain");
    }

    #[test]
    fn file_names_are_made_safe() {
        assert_eq!(safe_file_name("../../etc/passwd"), "_.._etc_passwd");
        assert_eq!(
            safe_file_name("Q3 report (final).pdf"),
            "Q3_report__final_.pdf"
        );
        assert_eq!(safe_file_name(".env"), "env");
        assert_eq!(safe_file_name(""), "file");
    }

    /// ADR-094: files that cross PekoHub are capped per file and per
    /// message; local files keep the ADR-088 limit.
    #[tokio::test]
    async fn remote_limits_cap_each_file_and_the_message() {
        let src = tempfile::tempdir().unwrap();
        let sized = |name: &str, len: u64| {
            let path = src.path().join(name);
            std::fs::File::create(&path).unwrap().set_len(len).unwrap();
            path.display().to_string()
        };
        let eleven = sized("eleven.bin", 11 * 1024 * 1024);
        assert!(validate(std::slice::from_ref(&eleven), Limits::LOCAL)
            .await
            .is_ok());
        let err = validate(std::slice::from_ref(&eleven), Limits::REMOTE)
            .await
            .unwrap_err();
        assert!(
            err.contains("10.0 MB") && err.contains("another computer"),
            "{err}"
        );

        let nine: Vec<String> = (0..3)
            .map(|i| sized(&format!("{i}.bin"), 9 * 1024 * 1024))
            .collect();
        assert!(validate(&nine[..2], Limits::REMOTE).await.is_ok());
        let err = validate(&nine, Limits::REMOTE).await.unwrap_err();
        assert!(err.contains("together") && err.contains("25.0 MB"), "{err}");

        let posted = |size| Attachment {
            name: "a".into(),
            path: String::new(),
            mime: "text/plain".into(),
            size,
            sha256: None,
        };
        assert!(check(&[posted(9 << 20), posted(9 << 20)], Limits::REMOTE).is_ok());
        assert!(check(&vec![posted(9 << 20); 3], Limits::REMOTE).is_err());
        assert!(check(&[posted(11 << 20)], Limits::REMOTE).is_err());
    }

    /// ADR-094: the channel's store keeps files under their digest, each
    /// by its (safe) name, so clients see what kind of file it is; the
    /// post records it by digest with no path, and only digests name
    /// folders.
    #[tokio::test]
    async fn the_channel_store_keeps_files_by_digest_and_name() {
        let src = tempfile::tempdir().unwrap();
        let store = tempfile::tempdir().unwrap();
        let a = src.path().join("a.png");
        std::fs::write(&a, PNG).unwrap();
        let b = src.path().join("copy of a.png");
        std::fs::write(&b, PNG).unwrap();
        let got = ingest_into_channel(
            store.path(),
            &[a.display().to_string(), b.display().to_string()],
            Limits::REMOTE,
        )
        .await
        .unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].sha256, got[1].sha256);
        assert_eq!(got[0].path, "");
        assert_eq!(got[0].mime, "image/png");
        assert_eq!(got[1].name, "copy of a.png");
        let digest = got[0].sha256.clone().unwrap();
        assert!(is_digest(&digest));
        let entries: Vec<_> = std::fs::read_dir(store.path()).unwrap().collect();
        assert_eq!(
            entries.len(),
            1,
            "one folder per digest, no staging leftovers"
        );
        let mut names: Vec<_> = std::fs::read_dir(store.path().join(&digest))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names, ["a.png", "copy_of_a.png"]);
        assert_eq!(
            locate(None, Some(store.path()), &got[1]),
            Some(store.path().join(&digest).join("copy_of_a.png"))
        );
        assert!(any_stored(store.path(), &digest).is_some());

        assert!(stored(store.path(), "../secret", "a.png").is_none());
        assert!(stored(store.path(), &"A".repeat(64), "a.png").is_none());
        assert!(any_stored(store.path(), "..").is_none());
        // A name cannot climb out of its digest's folder.
        assert_eq!(
            stored(store.path(), &digest, "../../etc/passwd"),
            Some(store.path().join(&digest).join("_.._etc_passwd"))
        );
        let missing = Attachment {
            sha256: Some("0".repeat(64)),
            ..got[0].clone()
        };
        assert_eq!(locate(None, Some(store.path()), &missing), None);
    }

    /// ADR-094: a remote post's files reach the turn as the peko's own
    /// copies; one that did not arrive becomes a note.
    #[tokio::test]
    async fn received_files_become_workspace_copies_or_notes() {
        let src = tempfile::tempdir().unwrap();
        let store = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let photo = src.path().join("photo.png");
        std::fs::write(&photo, PNG).unwrap();
        let mut files =
            ingest_into_channel(store.path(), &[photo.display().to_string()], Limits::REMOTE)
                .await
                .unwrap();
        files.push(Attachment {
            name: "clip.mp4".into(),
            path: String::new(),
            mime: "video/mp4".into(),
            size: 2048,
            sha256: Some("1".repeat(64)),
        });
        let (message, copies) =
            received_for_turn(workspace.path(), Some(store.path()), "look", &files).await;
        assert_eq!(copies.len(), 1);
        assert_eq!(copies[0].name, "photo.png");
        assert!(copies[0].path.starts_with("attachments/"));
        assert_eq!(
            std::fs::read(absolute(workspace.path(), &copies[0])).unwrap(),
            PNG
        );
        assert!(message.starts_with("look\n\n"), "{message}");
        assert!(
            message.contains("clip.mp4 (video/mp4, 2.0 KB) — it did not arrive"),
            "{message}"
        );
    }
}
