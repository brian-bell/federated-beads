//! Pure copy-context string builders and the OSC 52 clipboard wire format.
//!
//! `y`/`Y` copy an actionable string for the selected issue: `shell_command`
//! builds a runnable `cd <repo> && bd show <id>` (or the `bd -C <hub> show <id>`
//! fallback for an unattributed issue), `markdown_block` a shareable snippet.
//! `osc52` frames a payload as the terminal clipboard escape (via a
//! dependency-free `base64_encode`), which the runtime writes to the tty. Every
//! function is pure — no I/O, no clock — so the whole surface is unit-tested; the
//! runtime resolves the repo path and performs the write. See
//! `plans/slices/slice-12.md`.

use std::path::Path;

use crate::bd::Issue;
use crate::cli::sanitize;

/// Build the runnable shell command that takes the user to the selected issue.
///
/// With a resolved source `repo`: `cd <repo> && bd show <id>` — drops the user
/// into the repo and shows the issue there. Without one (an id matching no
/// configured prefix, or a collided/ambiguous one): `bd -C <hub> show <id>`,
/// which is always correct because the hub holds every hydrated issue.
///
/// Every interpolated argument is [`shell_quote`]d so the command stays runnable
/// and safe when pasted: a repo path containing a space would otherwise break
/// `cd`, and a shell metacharacter (`;`, `$()`, quotes) in a path or id would
/// otherwise execute. The id is additionally [`sanitize`]d (it is bd-sourced).
///
/// A repo path that is not valid UTF-8 (Unix paths may hold arbitrary bytes)
/// cannot be represented in the command string without a lossy substitution that
/// would `cd` to the wrong directory, so it falls back to the always-runnable hub
/// form rather than emit a subtly-wrong command.
pub fn shell_command(repo: Option<&Path>, hub: &Path, id: &str) -> String {
    let id = shell_quote(&sanitize(id));
    match repo.and_then(Path::to_str) {
        Some(repo) => format!("cd {} && bd show {}", shell_quote(repo), id),
        // No repo (unattributed) or a non-UTF-8 repo path: the hub form. The hub
        // path is fbd-owned under the XDG data dir; `to_string_lossy` is the last
        // resort if even it is non-UTF-8, where no faithful representation exists.
        None => format!("bd -C {} show {}", shell_quote(&hub.to_string_lossy()), id),
    }
}

/// POSIX shell-quote `s` so it interpolates as a single, literal argument. A word
/// made only of unambiguous, metacharacter-free characters is returned bare (so
/// the common `cd /Users/me/dev/repo && bd show mc-abc` reads cleanly); anything
/// else is wrapped in single quotes, with any embedded `'` closed-escaped-reopened
/// (`'\''`), which makes every other byte — spaces, `;`, `$`, `` ` ``, newlines —
/// literal.
fn shell_quote(s: &str) -> String {
    fn is_safe(c: char) -> bool {
        c.is_ascii_alphanumeric()
            || matches!(c, '.' | '_' | '-' | '/' | '@' | '%' | '+' | ',' | ':' | '=')
    }
    if !s.is_empty() && s.chars().all(is_safe) {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Build a shareable markdown block for the issue: a title heading, an id/repo
/// meta list, and the description when present. The single-line fields (title,
/// id, repo) are [`sanitize`]d; the description keeps its paragraph/list/code
/// line breaks via [`sanitize_multiline`] so a multi-line description pastes as
/// real markdown, not one mangled line. Either way terminal escape controls are
/// stripped (the block may be pasted into a terminal).
pub fn markdown_block(issue: &Issue, repo_name: &str) -> String {
    let mut block = format!(
        "## {}\n\n- **id:** {}\n- **repo:** {}\n",
        sanitize(&issue.title),
        sanitize(&issue.id),
        sanitize(repo_name),
    );
    if let Some(desc) = &issue.description {
        block.push('\n');
        block.push_str(&sanitize_multiline(desc));
        block.push('\n');
    }
    block
}

/// Like [`sanitize`] but for multi-line text: keep `\n`/`\t` (normalizing `\r\n`
/// and lone `\r` to `\n`) so markdown structure survives, while still replacing
/// every other control character — ESC, BEL, C0/C1, DEL — with U+FFFD so a pasted
/// description cannot drive the terminal.
fn sanitize_multiline(s: &str) -> String {
    s.replace("\r\n", "\n")
        .replace('\r', "\n")
        .chars()
        .map(|c| {
            if c == '\n' || c == '\t' {
                c
            } else if c.is_control() {
                '\u{FFFD}'
            } else {
                c
            }
        })
        .collect()
}

/// A one-line, length-capped summary of a copied payload for the status bar: the
/// first line, truncated with an ellipsis when it exceeds `max` characters.
pub fn summarize(payload: &str, max: usize) -> String {
    let first = payload.lines().next().unwrap_or("");
    if first.chars().count() <= max {
        return first.to_string();
    }
    // Reserve one char for the ellipsis so the whole summary fits in `max`.
    let keep = max.saturating_sub(1);
    let mut out: String = first.chars().take(keep).collect();
    out.push('…');
    out
}

/// Frame `payload` as an OSC 52 clipboard-set escape sequence:
/// `ESC ] 52 ; c ; <base64(payload)> BEL`. Writing this to the tty asks the
/// terminal to set the system clipboard — the portable, dependency-free path
/// that also works over ssh and (with clipboard passthrough enabled) tmux.
pub fn osc52(payload: &str) -> String {
    format!("\x1b]52;c;{}\x07", base64_encode(payload.as_bytes()))
}

/// Standard (RFC 4648 §4) base64 with `=` padding. Small and self-contained so
/// fbd takes no clipboard/base64 dependency for the one place it needs encoding.
pub fn base64_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        // Pack up to three bytes into a 24-bit group, then read four 6-bit
        // indices; positions past the input's end become `=` padding.
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let group = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[(group >> 18) as usize & 0x3f] as char);
        out.push(ALPHABET[(group >> 12) as usize & 0x3f] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(group >> 6) as usize & 0x3f] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[group as usize & 0x3f] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn issue(id: &str, title: &str, description: Option<&str>) -> Issue {
        Issue {
            id: id.to_string(),
            title: title.to_string(),
            status: "open".into(),
            priority: 1,
            description: description.map(str::to_string),
            issue_type: None,
            owner: None,
            labels: Vec::new(),
            created_at: None,
            created_by: None,
            updated_at: None,
            dependency_count: None,
            dependent_count: None,
            comment_count: None,
        }
    }

    #[test]
    fn builds_cd_command() {
        let cmd = shell_command(
            Some(Path::new("/Users/x/dev/megaclock")),
            Path::new("/hub"),
            "mc-abc",
        );
        assert_eq!(cmd, "cd /Users/x/dev/megaclock && bd show mc-abc");
    }

    #[test]
    fn unattributed_issue_falls_back_to_hub_show() {
        // No source repo (unknown/collided prefix): the hub form, always runnable.
        let cmd = shell_command(None, Path::new("/data/hub"), "mc-abc");
        assert_eq!(cmd, "bd -C /data/hub show mc-abc");
    }

    #[test]
    fn shell_quotes_paths_with_spaces() {
        // A repo path with a space would break `cd` unquoted; it must be quoted so
        // the pasted command still runs.
        let cmd = shell_command(Some(Path::new("/Users/x/my repo")), Path::new("/h"), "mc-1");
        assert_eq!(cmd, "cd '/Users/x/my repo' && bd show mc-1");
    }

    #[test]
    fn shell_quotes_metacharacters() {
        // Metacharacters in a path or id must not execute when pasted.
        let cmd = shell_command(
            Some(Path::new("/tmp/a;rm -rf b")),
            Path::new("/h"),
            "x$(id)",
        );
        assert!(
            cmd.starts_with("cd '/tmp/a;rm -rf b' && bd show "),
            "the dangerous path is single-quoted: {cmd:?}"
        );
        assert!(
            cmd.contains("'x$(id)'"),
            "the dangerous id is single-quoted: {cmd:?}"
        );
        // An embedded single quote is closed-escaped-reopened, not left dangling.
        let q = shell_command(Some(Path::new("/tmp/it's")), Path::new("/h"), "mc-1");
        assert!(q.contains(r"'/tmp/it'\''s'"), "single quote escaped: {q:?}");
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_repo_path_falls_back_to_hub() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;
        // A repo path with invalid UTF-8 bytes cannot be a faithful command
        // string; the hub form is emitted instead of a lossy, wrong `cd`.
        let bad = std::path::PathBuf::from(OsStr::from_bytes(b"/tmp/\xff\xferepo"));
        let cmd = shell_command(Some(&bad), Path::new("/data/hub"), "mc-1");
        assert_eq!(
            cmd, "bd -C /data/hub show mc-1",
            "a non-UTF-8 repo path degrades to the always-runnable hub form"
        );
    }

    #[test]
    fn builds_markdown_block() {
        let md = markdown_block(
            &issue(
                "mc-abc",
                "Fix the clock drift",
                Some("It skews after sleep."),
            ),
            "megaclock",
        );
        assert!(md.contains("Fix the clock drift"), "title present: {md:?}");
        assert!(md.contains("mc-abc"), "id present: {md:?}");
        assert!(md.contains("megaclock"), "repo present: {md:?}");
        assert!(md.contains("It skews after sleep."), "desc present: {md:?}");
    }

    #[test]
    fn markdown_block_without_description() {
        let md = markdown_block(&issue("mc-abc", "Title only", None), "megaclock");
        assert!(md.contains("Title only") && md.contains("mc-abc") && md.contains("megaclock"));
        // No stray empty description section beyond the meta list.
        assert!(md.trim_end().ends_with("megaclock"), "ends at meta: {md:?}");
    }

    #[test]
    fn markdown_preserves_multiline_description() {
        // A multi-paragraph / list description must keep its line breaks (and
        // tabs) so it pastes as real markdown, not one mangled line.
        let md = markdown_block(
            &issue("mc-1", "T", Some("first para\n\n- item one\n- item two")),
            "megaclock",
        );
        assert!(
            md.contains("first para\n\n- item one\n- item two"),
            "line breaks are preserved in the description: {md:?}"
        );
        // But an escape control inside the description is still neutralized.
        let evil = markdown_block(&issue("mc-1", "T", Some("ok\u{1b}]52;c;x\u{07}line2")), "r");
        assert!(
            !evil.contains('\u{1b}') && !evil.contains('\u{07}'),
            "escape controls stripped from a multi-line description: {evil:?}"
        );
    }

    #[test]
    fn sanitizes_control_chars() {
        // A hostile title/id: an OSC 52 escape + newline that must not survive
        // into the clipboard payload (which a user may paste into a terminal).
        let hostile = "pwn\u{1b}]52;c;aGk=\u{07}\nrow";
        let cmd = shell_command(Some(Path::new("/r")), Path::new("/h"), hostile);
        assert!(
            !cmd.contains('\u{1b}') && !cmd.contains('\u{07}') && !cmd.contains('\n'),
            "no raw control chars in the command: {cmd:?}"
        );
        let md = markdown_block(&issue(hostile, hostile, Some(hostile)), hostile);
        assert!(
            !md.contains('\u{1b}') && !md.contains('\u{07}'),
            "no raw escape/BEL in the markdown: {md:?}"
        );
    }

    #[test]
    fn base64_encode_matches_known_vectors() {
        // RFC 4648 §10 test vectors.
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn osc52_wraps_base64_payload() {
        assert_eq!(osc52("hi"), "\u{1b}]52;c;aGk=\u{07}");
    }

    #[test]
    fn summarize_truncates_first_line() {
        assert_eq!(summarize("short", 20), "short");
        // Multi-line payload: only the first line.
        assert_eq!(summarize("first\nsecond", 20), "first");
        // Past the cap: truncated with an ellipsis, total length == max.
        let long = "a".repeat(50);
        let s = summarize(&long, 10);
        assert_eq!(s.chars().count(), 10, "capped to max chars: {s:?}");
        assert!(s.ends_with('…'), "ellipsis marks truncation: {s:?}");
    }
}
