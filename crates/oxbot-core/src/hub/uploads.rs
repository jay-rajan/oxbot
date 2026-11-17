//! Files uploaded for a message, from upload until the message is sent
//! (see [`crate::attach`]). An upload belongs to the person who sent it and
//! the bot it was sent for; one that isn't sent within an hour is deleted.
//!
//! An upload waits in the bot's `private/` folder, outside `work/`, where
//! the bot's tools can't reach. When the message is sent, the files the
//! person let the AI read move into `work/` (same place below it); the
//! rest stay private for good.

use std::path::{Path, PathBuf};
use std::time::Instant;

use oxbot_protocol::{Attachment, EventKind};

use super::{Hub, HubError, lock};
use crate::attach::{self, AttachError, Inbox};
use crate::bot::{Bot, Viewer};
use crate::id::{BotId, PersonId};

/// The most files one person may have waiting to be sent, across bots.
const MAX_WAITING: usize = 30;

/// An uploaded file not sent yet.
pub(super) struct Pending {
    bot: BotId,
    uploader: Option<PersonId>,
    accepted: attach::Accepted,
    at: Instant,
    /// Sends of it under way (waiting for a turn, or starting its session): it
    /// isn't deleted meanwhile.
    sending: u32,
}

impl Hub {
    /// Where a file `viewer` uploads for `bot` goes, if they may send it
    /// messages.
    ///
    /// # Errors
    /// Fails if they can't see the bot.
    pub fn upload_inbox(&self, bot: &str, viewer: &Viewer) -> Result<(BotId, Inbox), HubError> {
        let bot = self.bot_for(bot, viewer)?;
        let viewer = self.resolve(viewer);
        let waiting = lock(&self.uploads)
            .values()
            .filter(|p| p.uploader == viewer.id)
            .count();
        if waiting >= MAX_WAITING {
            return Err(HubError::NotAllowed(format!(
                "You have {waiting} files waiting to be sent. Send or remove some first."
            )));
        }
        let inbox = Inbox::new(private_root(&bot));
        Ok((bot.id, inbox))
    }

    /// Puts files just claimed for `bot` where the person chose: those in
    /// `ai` (upload ids) in its work folder for the AI to read, the rest
    /// in its private folder, marked private. Moving back and forth is fine
    /// (a send that failed may be tried again, chosen otherwise).
    ///
    /// # Errors
    /// Fails if a file can't be moved.
    pub(super) fn place_uploads(
        &self,
        bot: &Bot,
        files: &mut [Attachment],
        ai: &[String],
    ) -> Result<(), HubError> {
        let moved =
            |e: std::io::Error| HubError::NotAllowed(format!("Couldn't put a file in place: {e}"));
        let work = bot.work_dir();
        let private = private_root(bot);
        let mut uploads = lock(&self.uploads);
        for f in files.iter_mut() {
            f.private = !ai.contains(&f.id);
            let (from, to) = if f.private {
                (&work, &private)
            } else {
                (&private, &work)
            };
            let Some(p) = uploads.get_mut(&f.id) else {
                continue;
            };
            for path in &mut p.accepted.saved {
                let Ok(below) = path.strip_prefix(from) else {
                    continue;
                };
                let dest = to.join(below);
                if path.exists() {
                    if let Some(parent) = dest.parent() {
                        attach::make_dir_in(to, parent).map_err(moved)?;
                    }
                    std::fs::rename(&*path, &dest).map_err(moved)?;
                }
                // (A file inside a folder moved just before went with it.)
                *path = dest;
            }
            p.accepted.attachment.private = f.private;
        }
        Ok(())
    }

    /// Takes a file `viewer` uploaded for `bot` (saved at `incoming`), to
    /// send with their next message: checks it, converts what the bot can't
    /// read as it is, and keeps it until then.
    ///
    /// # Errors
    /// Says why the file wasn't taken.
    pub async fn accept_upload(
        &self,
        bot: &BotId,
        inbox: &Inbox,
        viewer: &Viewer,
        incoming: &Path,
        id: &str,
        name: &str,
    ) -> Result<Attachment, AttachError> {
        // Files for a bot the sender may not give tasks are refused first.
        if let Err(err) = self.may_send(bot.as_str(), viewer) {
            return Err(AttachError::Unsupported(err.to_string()));
        }
        // A few at a time: converting can be heavy.
        let Ok(_slot) = self.conversions.acquire().await else {
            return Err(AttachError::Unreadable(
                "oxbot is shutting down.".to_owned(),
            ));
        };
        let accepted = attach::accept(inbox, incoming, id, name, &self.config.mac).await?;
        let attachment = accepted.attachment.clone();
        lock(&self.uploads).insert(
            id.to_owned(),
            Pending {
                bot: bot.clone(),
                uploader: self.resolve(viewer).id.clone(),
                accepted,
                at: Instant::now(),
                sending: 0,
            },
        );
        Ok(attachment)
    }

    /// Deletes a file `viewer` uploaded for `bot` that won't be sent.
    ///
    /// # Errors
    /// Fails if it isn't theirs (or is gone already).
    pub fn drop_upload(&self, bot: &str, id: &str, viewer: &Viewer) -> Result<(), HubError> {
        let bot = self.bot_for(bot, viewer)?.id;
        let uploader = self.resolve(viewer).id.clone();
        let mut uploads = lock(&self.uploads);
        match uploads.get(id) {
            Some(p) if p.bot == bot && p.uploader == uploader && p.sending > 0 => Err(
                HubError::NotAllowed("That file is being sent right now.".to_owned()),
            ),
            Some(p) if p.bot == bot && p.uploader == uploader => {
                if let Some(p) = uploads.remove(id) {
                    attach::remove_all(&p.accepted.saved);
                }
                Ok(())
            }
            _ => Err(gone()),
        }
    }

    /// The files with `ids`, if each was uploaded by `uploader` for `bot`
    /// and is still waiting, marked as being sent: they aren't deleted
    /// until the message is in the chat ([`Hub::uploads_sent`]), or the
    /// send failed and they wait again ([`Hub::uploads_not_sent`]).
    pub(super) fn claim_uploads(
        &self,
        bot: &BotId,
        uploader: Option<&PersonId>,
        ids: &[String],
    ) -> Result<Vec<Attachment>, HubError> {
        if ids.len() > attach::MAX_FILES {
            return Err(HubError::NotAllowed(format!(
                "oxbot takes up to {} files per message.",
                attach::MAX_FILES
            )));
        }
        let mut uploads = lock(&self.uploads);
        let mut out: Vec<Attachment> = Vec::with_capacity(ids.len());
        for id in ids {
            match uploads.get(id) {
                Some(p) if &p.bot == bot && p.uploader.as_ref() == uploader => {
                    if !out.iter().any(|a| &a.id == id) {
                        out.push(p.accepted.attachment.clone());
                    }
                }
                _ => return Err(gone()),
            }
        }
        for f in &out {
            if let Some(p) = uploads.get_mut(&f.id) {
                p.sending += 1;
            }
        }
        Ok(out)
    }

    /// A send of `files` failed: they wait to be sent again, for a fresh
    /// hour.
    pub(super) fn uploads_not_sent(&self, files: &[Attachment]) {
        let mut uploads = lock(&self.uploads);
        for f in files {
            if let Some(p) = uploads.get_mut(&f.id) {
                p.sending = p.sending.saturating_sub(1);
                p.at = Instant::now();
            }
        }
    }

    /// The files are in the chat now: they're no longer waiting (and are
    /// kept in the bot's folder).
    pub(super) fn uploads_sent(&self, files: &[Attachment]) {
        let mut uploads = lock(&self.uploads);
        for f in files {
            uploads.remove(&f.id);
        }
    }

    /// Deletes uploads nobody sent within [`HubConfig::keep_unsent`]
    /// (oxbot does this every minute).
    ///
    /// [`HubConfig::keep_unsent`]: super::HubConfig::keep_unsent
    pub fn sweep_uploads(&self) {
        let old: Vec<Pending> = {
            let mut uploads = lock(&self.uploads);
            let ids: Vec<String> = uploads
                .iter()
                .filter(|(_, p)| p.sending == 0 && p.at.elapsed() >= self.config.keep_unsent)
                .map(|(id, _)| id.clone())
                .collect();
            ids.iter().filter_map(|id| uploads.remove(id)).collect()
        };
        for p in old {
            tracing::info!(bot = %p.bot, "deleting a file that was never sent");
            attach::remove_all(&p.accepted.saved);
        }
        self.sweep_incoming();
    }

    /// Deletes partial uploads left in bots' `work/inbox/.incoming/` (by a
    /// crash, say) that haven't been written to for
    /// [`HubConfig::keep_unsent`]. Only there: nothing else is touched.
    ///
    /// [`HubConfig::keep_unsent`]: super::HubConfig::keep_unsent
    pub fn sweep_incoming(&self) {
        let dirs: Vec<PathBuf> = self
            .bots
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .flat_map(|b| {
                [b.work_dir(), private_root(b)].map(|root| Inbox::new(root).dir().join(".incoming"))
            })
            .collect();
        for dir in dirs {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                // Upload ids only, plain files only (never a link).
                let ours = entry.file_name().to_str().is_some_and(attach::is_upload_id)
                    && entry.file_type().is_ok_and(|t| t.is_file());
                let stale = entry
                    .metadata()
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.elapsed().ok())
                    .is_some_and(|age| age >= self.config.keep_unsent);
                if ours && stale {
                    tracing::info!(path = %path.display(), "deleting a partial upload");
                    let _ = std::fs::remove_file(&path);
                }
            }
        }
    }

    /// Where the file `id`, sent with message `seq` of `bot`, is saved, for
    /// `viewer` to open. Found from the stored message, never from a path
    /// anyone sends, and only inside the bot's inbox.
    ///
    /// # Errors
    /// Fails if they can't see the bot, or it isn't there.
    pub fn attachment_file(
        &self,
        bot: &str,
        seq: u64,
        id: &str,
        viewer: &Viewer,
    ) -> Result<(Attachment, PathBuf), HubError> {
        let bot = self.bot_for(bot, viewer)?;
        let missing = || HubError::NotAllowed("That file isn't there any more.".to_owned());
        let Some(event) = self.store.event(&bot.id, seq)? else {
            return Err(missing());
        };
        let EventKind::UserMessage { files, .. } = event.kind else {
            return Err(missing());
        };
        let file = files.into_iter().find(|f| f.id == id).ok_or_else(missing)?;
        let root = if file.private {
            private_root(&bot)
        } else {
            bot.work_dir()
        };
        let path = Inbox::new(root)
            .resolve(&file.path)
            .filter(|p| p.is_file())
            .ok_or_else(missing)?;
        Ok((file, path))
    }
}

/// Where a claimed upload for `bot` is saved now (in its private or work
/// folder, as placed), to copy into Family Notes.
pub(super) fn upload_file(bot: &Bot, f: &Attachment) -> Option<PathBuf> {
    let root = if f.private {
        private_root(bot)
    } else {
        bot.work_dir()
    };
    Inbox::new(root).resolve(&f.path)
}

impl Hub {
    /// Marks upload `id` as filed in Family Notes as `note`, so a send
    /// tried again doesn't file it twice.
    pub(super) fn mark_filed(&self, id: &str, note: i64) {
        if let Some(p) = lock(&self.uploads).get_mut(id) {
            p.accepted.attachment.filed = Some(note);
        }
    }
}

/// Where a bot's uploads wait, and its private files stay: `private/` in
/// its folder, beside (never inside) `work/`.
pub(crate) fn private_root(bot: &Bot) -> PathBuf {
    bot.dir.join("private")
}

fn gone() -> HubError {
    HubError::NotAllowed(
        "A file you attached is no longer waiting to be sent (unsent files are cleared after an hour). Attach it again."
            .to_owned(),
    )
}
