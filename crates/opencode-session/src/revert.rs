use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

use crate::session::{FileDiff as SessionFileDiff, Session, SessionManager};
use crate::snapshot::Snapshot;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RevertInput {
    pub session_id: String,
    pub message_id: String,
    pub part_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RevertInfo {
    pub message_id: String,
    pub part_id: Option<String>,
    pub snapshot: Option<String>,
    pub diff: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RevertSummary {
    pub additions: u64,
    pub deletions: u64,
    pub files: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileDiff {
    pub path: String,
    pub additions: u64,
    pub deletions: u64,
}

impl From<SessionFileDiff> for FileDiff {
    fn from(d: SessionFileDiff) -> Self {
        Self {
            path: d.path,
            additions: d.additions,
            deletions: d.deletions,
        }
    }
}

pub struct RevertManager {
    worktree: PathBuf,
}

impl RevertManager {
    pub fn new(worktree: PathBuf) -> Self {
        Self { worktree }
    }

    pub fn revert(
        &self,
        input: &RevertInput,
        manager: &mut SessionManager,
    ) -> Result<Option<Session>> {
        let session = match manager.get(&input.session_id) {
            Some(s) => s.clone(),
            None => return Ok(None),
        };

        let messages = &session.messages;
        let mut last_user_id: Option<String> = None;
        let mut revert: Option<RevertInfo> = None;

        for msg in messages {
            if msg.role == crate::MessageRole::User {
                last_user_id = Some(msg.id.clone());
            }

            if revert.is_some() {
                continue;
            }

            if msg.id == input.message_id && input.part_id.is_none() {
                revert = Some(RevertInfo {
                    message_id: last_user_id.clone().unwrap_or_else(|| msg.id.clone()),
                    part_id: None,
                    snapshot: None,
                    diff: None,
                });
            }

            for part in &msg.parts {
                if Some(&part.id) == input.part_id.as_ref() {
                    revert = Some(RevertInfo {
                        message_id: msg.id.clone(),
                        part_id: input.part_id.clone(),
                        snapshot: None,
                        diff: None,
                    });
                }
            }
        }

        if let Some(mut rev) = revert {
            let existing_snapshot = session.revert.as_ref().and_then(|r| r.snapshot.clone());

            // Snapshot::track の失敗を空文字スナップショットへ変換して保存すると、
            // 復元不可能な revert 状態が残る。失敗は伝播させ、状態を書き込まない。
            let snapshot = match existing_snapshot {
                Some(snapshot) => snapshot,
                None => Snapshot::track(&self.worktree)?,
            };
            rev.snapshot = Some(snapshot);

            if let Some(ref snapshot) = rev.snapshot {
                let diffs = Snapshot::diff(&self.worktree, snapshot)?;
                rev.diff = if diffs.is_empty() {
                    None
                } else {
                    Some(serde_json::to_string(&diffs)?)
                };
            }

            if let Some(session) = manager.get_mut(&input.session_id) {
                let session_revert = crate::session::SessionRevert {
                    message_id: rev.message_id.clone(),
                    part_id: rev.part_id.clone(),
                    snapshot: rev.snapshot.clone(),
                    diff: rev.diff.clone(),
                };
                session.set_revert(session_revert);
                return Ok(Some(session.clone()));
            }
        }

        Ok(Some(session))
    }

    pub fn unrevert(
        &self,
        session_id: &str,
        manager: &mut SessionManager,
    ) -> Result<Option<Session>> {
        let session = match manager.get(session_id) {
            Some(s) => s.clone(),
            None => return Ok(None),
        };

        let revert = match &session.revert {
            Some(r) => r.clone(),
            None => return Ok(Some(session)),
        };

        if let Some(ref snapshot) = revert.snapshot {
            // Propagate restore failures so the revert state is preserved and
            // the caller can retry. `?` returns before clear_revert() on error.
            Snapshot::restore(&self.worktree, snapshot)?;
        }

        if let Some(session) = manager.get_mut(session_id) {
            session.clear_revert();
            return Ok(Some(session.clone()));
        }

        Ok(Some(session))
    }

    pub fn cleanup(
        &self,
        session_id: &str,
        revert: &RevertInfo,
        manager: &mut SessionManager,
    ) -> Result<()> {
        let session = match manager.get(session_id) {
            Some(s) => s.clone(),
            None => return Ok(()),
        };

        let message_id = &revert.message_id;
        let messages = &session.messages;

        // 本番のメッセージ ID は `msg_<uuid v4>` で、辞書順は時刻順と無関係。
        // 文字列比較ではなく Vec 上の位置で対象メッセージを特定する。
        let target_pos = messages.iter().position(|m| &m.id == message_id);

        // 対象が見つからない場合は何も削除しない（全メッセージを保持）。
        let mut preserve: Vec<crate::SessionMessage> = match target_pos {
            Some(pos) => messages.iter().take(pos).cloned().collect(),
            None => messages.clone(),
        };

        if let (Some(pos), Some(part_id)) = (target_pos, &revert.part_id) {
            let mut target = messages[pos].clone();
            if let Some(start) = target.parts.iter().position(|p| &p.id == part_id) {
                let preserve_parts: Vec<crate::MessagePart> =
                    target.parts.iter().take(start).cloned().collect();
                target.parts = preserve_parts;
            }
            // part_id が見つからない場合も target をそのまま保持する（M-64）。
            preserve.push(target);
        }

        if let Some(session) = manager.get_mut(session_id) {
            session.messages = preserve;
            session.clear_revert();
        }

        Ok(())
    }

    /// Compute file diffs from session messages.
    ///
    /// Mirrors the TS `SessionSummary.computeDiff` behavior:
    /// 1. Scans messages for snapshot hashes stored in metadata
    ///    (keys `"step_start_snapshot"` / `"step_finish_snapshot"`).
    ///    If both an earliest "from" and a latest "to" snapshot are found,
    ///    delegates to `Snapshot::diff_full` (git diff between two refs).
    /// 2. Falls back to aggregating `Patch` parts by filepath, counting
    ///    per-line additions and deletions from `old_string` / `new_string`.
    pub fn compute_diff(&self, messages: &[crate::SessionMessage]) -> Result<Vec<FileDiff>> {
        // --- Strategy 1: snapshot-based diff (matches TS step-start / step-finish) ---
        let mut from_snapshot: Option<String> = None;
        let mut to_snapshot: Option<String> = None;

        for msg in messages {
            // Check message-level metadata for snapshot hashes
            if from_snapshot.is_none() {
                if let Some(serde_json::Value::String(s)) = msg.metadata.get("step_start_snapshot")
                {
                    if !s.is_empty() {
                        from_snapshot = Some(s.clone());
                    }
                }
            }
            if let Some(serde_json::Value::String(s)) = msg.metadata.get("step_finish_snapshot") {
                if !s.is_empty() {
                    to_snapshot = Some(s.clone());
                }
            }

            // Also scan parts: StepStart / StepFinish carry id+name / id+output,
            // but the metadata on the *message* is the canonical place for snapshots
            // in the v1 format. We also check part-level metadata stored in the
            // message metadata under "snapshot" as a generic key.
            if from_snapshot.is_none() {
                if let Some(serde_json::Value::String(s)) = msg.metadata.get("snapshot") {
                    if !s.is_empty() {
                        from_snapshot = Some(s.clone());
                    }
                }
            }
        }

        if let (Some(ref from), Some(ref to)) = (&from_snapshot, &to_snapshot) {
            let diffs = Snapshot::diff_full(&self.worktree, from, to)?;
            if !diffs.is_empty() {
                return Ok(diffs.into_iter().map(FileDiff::from).collect());
            }
        }

        // --- Strategy 2: aggregate Patch parts ---
        let mut file_stats: HashMap<String, (u64, u64)> = HashMap::new();

        for msg in messages {
            for part in &msg.parts {
                if let crate::PartType::Patch {
                    ref old_string,
                    ref new_string,
                    ref filepath,
                } = part.part_type
                {
                    let (additions, deletions) = count_line_changes(old_string, new_string);
                    let entry = file_stats.entry(filepath.clone()).or_insert((0, 0));
                    entry.0 += additions;
                    entry.1 += deletions;
                }
            }
        }

        let mut diffs: Vec<FileDiff> = file_stats
            .into_iter()
            .map(|(path, (additions, deletions))| FileDiff {
                path,
                additions,
                deletions,
            })
            .collect();

        // Sort by path for deterministic output
        diffs.sort_by(|a, b| a.path.cmp(&b.path));

        Ok(diffs)
    }
}

/// Count line-level additions and deletions between two strings.
///
/// Uses a simple longest-common-subsequence (LCS) diff on lines:
/// lines present only in `new` are additions, lines present only in `old`
/// are deletions.
fn count_line_changes(old: &str, new: &str) -> (u64, u64) {
    if old == new {
        return (0, 0);
    }

    let old_lines: Vec<&str> = old.lines().collect();
    let new_lines: Vec<&str> = new.lines().collect();

    // Fast path: entirely new content (old was empty)
    if old_lines.is_empty() {
        return (new_lines.len() as u64, 0);
    }
    // Fast path: content deleted entirely
    if new_lines.is_empty() {
        return (0, old_lines.len() as u64);
    }

    // Myers-style simple diff via LCS length.
    // We only need counts, not the actual edit script.
    let lcs_len = lcs_length(&old_lines, &new_lines);
    let deletions = old_lines.len() as u64 - lcs_len;
    let additions = new_lines.len() as u64 - lcs_len;

    (additions, deletions)
}

/// Compute the length of the longest common subsequence of two line slices.
/// Uses the classic DP approach with O(min(m,n)) space.
fn lcs_length(a: &[&str], b: &[&str]) -> u64 {
    let (short, long) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    let n = short.len();
    let mut prev = vec![0u64; n + 1];
    let mut curr = vec![0u64; n + 1];

    for long_line in long {
        for (j, short_line) in short.iter().enumerate() {
            if long_line == short_line {
                curr[j + 1] = prev[j] + 1;
            } else {
                curr[j + 1] = curr[j].max(prev[j + 1]);
            }
        }
        std::mem::swap(&mut prev, &mut curr);
        curr.iter_mut().for_each(|v| *v = 0);
    }

    *prev.last().unwrap_or(&0)
}

impl Default for RevertManager {
    fn default() -> Self {
        Self::new(PathBuf::from("."))
    }
}

pub fn revert(input: &RevertInput, manager: &mut SessionManager) -> Result<Option<Session>> {
    RevertManager::default().revert(input, manager)
}

pub fn unrevert(session_id: &str, manager: &mut SessionManager) -> Result<Option<Session>> {
    RevertManager::default().unrevert(session_id, manager)
}

pub fn cleanup(session_id: &str, revert: &RevertInfo, manager: &mut SessionManager) -> Result<()> {
    RevertManager::default().cleanup(session_id, revert, manager)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn count_line_changes_identical() {
        assert_eq!(count_line_changes("a\nb", "a\nb"), (0, 0));
    }

    #[test]
    fn count_line_changes_old_empty() {
        assert_eq!(count_line_changes("", "a\nb\nc"), (3, 0));
    }

    #[test]
    fn count_line_changes_new_empty() {
        assert_eq!(count_line_changes("a\nb", ""), (0, 2));
    }

    #[test]
    fn count_line_changes_single_replacement() {
        assert_eq!(count_line_changes("old\nkeep", "new\nkeep"), (1, 1));
    }

    #[test]
    fn count_line_changes_append() {
        assert_eq!(count_line_changes("a\n", "a\nb\n"), (1, 0));
    }

    #[test]
    fn count_line_changes_delete() {
        assert_eq!(count_line_changes("a\nb\n", "a\n"), (0, 1));
    }

    #[test]
    fn lcs_length_handles_swapped_arguments() {
        let a = ["a", "b", "c"];
        let b = ["a", "c", "d"];
        assert_eq!(lcs_length(&a, &b), 2);
        assert_eq!(lcs_length(&b, &a), 2);
    }

    #[test]
    fn lcs_length_empty_input() {
        assert_eq!(lcs_length(&[], &["a"]), 0);
        assert_eq!(lcs_length(&["a"], &[]), 0);
    }

    /// Successful restore: revert state is cleared and the session is returned.
    #[test]
    fn unrevert_clears_revert_on_successful_restore() {
        let temp = tempfile::tempdir().expect("temp dir");
        let root = temp.path();
        let file = root.join("unrevert_restore.txt");

        std::fs::write(&file, "original\n").expect("write initial file");
        let snapshot = Snapshot::track(root).expect("track snapshot");
        std::fs::write(&file, "modified\n").expect("modify file");

        let mut manager = SessionManager::new();
        let session = manager.create("project", root.to_string_lossy().to_string());
        let session_id = session.id.clone();
        manager.set_revert(
            &session_id,
            crate::session::SessionRevert {
                message_id: "msg-1".into(),
                part_id: None,
                snapshot: Some(snapshot),
                diff: None,
            },
        );

        let revert_manager = RevertManager::new(root.to_path_buf());
        let result = revert_manager.unrevert(&session_id, &mut manager);

        assert!(result.is_ok(), "unrevert should succeed on valid snapshot");
        assert!(
            result.unwrap().is_some(),
            "existing session should be returned"
        );

        let updated = manager.get(&session_id).expect("session exists");
        assert!(
            updated.revert.is_none(),
            "revert state should be cleared after a successful restore"
        );

        let restored = std::fs::read_to_string(&file).expect("read restored file");
        assert_eq!(
            restored, "original\n",
            "file should be restored from snapshot"
        );
    }

    /// Failed restore: error is propagated and the revert state is preserved
    /// so the caller can retry instead of losing the only snapshot reference.
    #[test]
    fn unrevert_keeps_revert_on_failed_restore() {
        let temp = tempfile::tempdir().expect("temp dir");
        let root = temp.path();
        let bad_snapshot = "ffffffffffffffffffffffffffffffffffffffff".to_string();

        let mut manager = SessionManager::new();
        let session = manager.create("project", root.to_string_lossy().to_string());
        let session_id = session.id.clone();
        manager.set_revert(
            &session_id,
            crate::session::SessionRevert {
                message_id: "msg-1".into(),
                part_id: None,
                snapshot: Some(bad_snapshot.clone()),
                diff: None,
            },
        );

        let revert_manager = RevertManager::new(root.to_path_buf());
        let result = revert_manager.unrevert(&session_id, &mut manager);

        assert!(
            result.is_err(),
            "unrevert should propagate a restore failure"
        );

        let updated = manager.get(&session_id).expect("session exists");
        let revert = updated
            .revert
            .as_ref()
            .expect("revert state must be preserved when restore fails");
        assert_eq!(
            revert.snapshot.as_deref(),
            Some(bad_snapshot.as_str()),
            "the snapshot reference must remain available for retry"
        );
    }

    /// Snapshot::track が失敗した場合、空ハッシュを状態保存せずエラーを伝播すること。
    #[test]
    fn revert_propagates_track_failure_without_saving_empty_snapshot() {
        let temp = tempfile::tempdir().expect("temp dir");
        // 通常ファイルはスナップショット用ディレクトリをホストできないため、
        // Snapshot::track が確実に失敗する。
        let worktree = temp.path().join("not-a-directory");
        std::fs::write(&worktree, "not a directory").expect("write file");

        let mut manager = SessionManager::new();
        let session = manager.create("project", worktree.to_string_lossy().to_string());
        let session_id = session.id.clone();
        let message = crate::SessionMessage::user(&session_id, "text");
        let message_id = message.id.clone();
        manager.update_message(&session_id, message);

        let revert_manager = RevertManager::new(worktree);
        let result = revert_manager.revert(
            &RevertInput {
                session_id: session_id.clone(),
                message_id,
                part_id: None,
            },
            &mut manager,
        );

        assert!(result.is_err(), "track failure must be propagated as Err");
        let updated = manager.get(&session_id).expect("session exists");
        assert!(
            updated.revert.is_none(),
            "no revert state (especially an empty snapshot) may be saved on track failure"
        );
    }

    /// テスト用メッセージ。ID を明示して cleanup の辞書順比較を安定させる。
    fn test_message(id: &str, session_id: &str, part_ids: &[&str]) -> crate::SessionMessage {
        let mut msg = crate::SessionMessage::user(session_id, "text");
        msg.id = id.to_string();
        let template = msg.parts[0].clone();
        msg.parts = part_ids
            .iter()
            .map(|part_id| {
                let mut part = template.clone();
                part.id = (*part_id).to_string();
                part
            })
            .collect();
        msg
    }

    /// msg-0001 / msg-0002 / msg-0003 を保持するセッションを用意する。
    fn manager_with_three_messages(second_part_ids: &[&str]) -> (SessionManager, String) {
        let mut manager = SessionManager::new();
        let session = manager.create("project", "/tmp");
        let session_id = session.id.clone();
        let fixtures = [
            ("msg-0001", vec!["prt-0001"]),
            ("msg-0002", second_part_ids.to_vec()),
            ("msg-0003", vec!["prt-0003"]),
        ];
        for (id, parts) in fixtures {
            manager.update_message(&session_id, test_message(id, &session_id, &parts));
        }
        (manager, session_id)
    }

    /// 部分一致なし（part_id=None）: 対象メッセージ以降を削除し、以前のみ保持する。
    #[test]
    fn cleanup_without_part_id_removes_target_and_later_messages() {
        let (mut manager, session_id) = manager_with_three_messages(&["prt-a", "prt-b"]);
        let revert = RevertInfo {
            message_id: "msg-0002".into(),
            part_id: None,
            snapshot: None,
            diff: None,
        };

        cleanup(&session_id, &revert, &mut manager).expect("cleanup should succeed");

        let session = manager.get(&session_id).expect("session exists");
        let ids: Vec<_> = session.messages.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["msg-0001"]);
        assert!(session.revert.is_none(), "revert state should be cleared");
    }

    /// 部分一致あり: 一致した part 以降を切り詰め、target を保持する。
    #[test]
    fn cleanup_with_matching_part_id_preserves_prefix_parts() {
        let (mut manager, session_id) = manager_with_three_messages(&["prt-a", "prt-b", "prt-c"]);
        let revert = RevertInfo {
            message_id: "msg-0002".into(),
            part_id: Some("prt-b".into()),
            snapshot: None,
            diff: None,
        };

        cleanup(&session_id, &revert, &mut manager).expect("cleanup should succeed");

        let session = manager.get(&session_id).expect("session exists");
        let ids: Vec<_> = session.messages.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["msg-0001", "msg-0002"]);

        let target = session.messages.last().expect("target preserved");
        let part_ids: Vec<_> = target.parts.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(part_ids, ["prt-a"]);
        assert!(session.revert.is_none(), "revert state should be cleared");
    }

    /// 部分不一致（M-64）: part が見つからなくても target を無変更で保持する。
    #[test]
    fn cleanup_with_unmatched_part_id_preserves_whole_target_message() {
        let (mut manager, session_id) = manager_with_three_messages(&["prt-a", "prt-b"]);
        let revert = RevertInfo {
            message_id: "msg-0002".into(),
            part_id: Some("prt-missing".into()),
            snapshot: None,
            diff: None,
        };

        cleanup(&session_id, &revert, &mut manager).expect("cleanup should succeed");

        let session = manager.get(&session_id).expect("session exists");
        let ids: Vec<_> = session.messages.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["msg-0001", "msg-0002"]);

        let target = session.messages.last().expect("target must not be dropped");
        let part_ids: Vec<_> = target.parts.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(part_ids, ["prt-a", "prt-b"], "parts must be unchanged");
        assert!(session.revert.is_none(), "revert state should be cleared");
    }

    /// 本番 ID は `msg_<uuid v4>` で辞書順が時刻順と無関係（R5）。
    /// 時刻順 [first, target, last] を辞書順が target < last < first になる
    /// ID で構成し、target より前の first が preserve されることを固定する。
    #[test]
    fn cleanup_uses_message_order_not_lexicographic_id_order() {
        let mut manager = SessionManager::new();
        let session = manager.create("project", "/tmp");
        let session_id = session.id.clone();

        // 時刻順は first -> target -> last。辞書順は target < last < first。
        let first = "msg_ffffffff-0000-4000-8000-000000000000";
        let target = "msg_00000000-0000-4000-8000-000000000000";
        let last = "msg_aaaaaaaa-0000-4000-8000-000000000000";

        assert!(
            target < last && last < first,
            "fixture must invert lexicographic vs time order"
        );

        manager.update_message(
            &session_id,
            test_message(first, &session_id, &["prt-first"]),
        );
        manager.update_message(
            &session_id,
            test_message(target, &session_id, &["prt-1", "prt-2"]),
        );
        manager.update_message(&session_id, test_message(last, &session_id, &["prt-last"]));

        let revert = RevertInfo {
            message_id: target.into(),
            part_id: Some("prt-2".into()),
            snapshot: None,
            diff: None,
        };

        cleanup(&session_id, &revert, &mut manager).expect("cleanup should succeed");

        let session = manager.get(&session_id).expect("session exists");
        let ids: Vec<_> = session.messages.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(
            ids,
            [first, target],
            "messages before the target must be preserved in time order"
        );

        let preserved = session.messages.last().expect("target preserved");
        let part_ids: Vec<_> = preserved.parts.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(part_ids, ["prt-1"]);
        assert!(session.revert.is_none(), "revert state should be cleared");
    }
}
