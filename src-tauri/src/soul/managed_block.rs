// Idempotent delimited-region editing for the global agent instruction files
// (Phase 7, tracking issue #27). Ported from
// `electron/main/soul/managed-block.ts`. Each managed region is bounded by a
// stable HTML-comment marker pair keyed by an id (`managed`, `memory`), so
// repeated writes replace the region in place instead of duplicating it, and
// hand-written content surrounding the markers is preserved untouched.

fn open_marker(id: &str) -> String {
    format!("<!-- HEARTH:{id} — generated, do not edit by hand -->")
}

fn close_marker(id: &str) -> String {
    format!("<!-- /HEARTH:{id} -->")
}

/// Extracts the body text between the `id` markers, or `None` if absent.
pub fn read_block(content: &str, id: &str) -> Option<String> {
    let open = open_marker(id);
    let close = close_marker(id);
    let open_idx = content.find(&open)?;
    let mut body_start = open_idx + open.len();
    if content[body_start..].starts_with('\n') {
        body_start += 1;
    }
    let rel_close = content[body_start..].find(&close)?;
    let close_idx = body_start + rel_close;
    let body = &content[body_start..close_idx];
    Some(body.strip_suffix('\n').unwrap_or(body).to_string())
}

/// Removes the entire `id` block (markers, body, and the blank-line run
/// immediately before/after it) from `content`, replacing it with a single
/// newline — mirrors `blockRegex(id)`'s `\n*...\n*` span and its
/// `content.replace(blockRegex(id), '\n')` call.
fn strip_block(content: &str, id: &str) -> String {
    let open = open_marker(id);
    let close = close_marker(id);
    let Some(open_idx) = content.find(&open) else {
        return content.to_string();
    };
    let Some(rel_close) = content[open_idx..].find(&close) else {
        return content.to_string();
    };
    let close_idx = open_idx + rel_close;
    let mut end = close_idx + close.len();
    while content[end..].starts_with('\n') {
        end += 1;
    }
    let mut start = open_idx;
    while start > 0 && content.as_bytes()[start - 1] == b'\n' {
        start -= 1;
    }
    format!("{}\n{}", &content[..start], &content[end..])
}

fn collapse_blank_runs(content: &str) -> String {
    let mut out = String::with_capacity(content.len());
    let mut newline_run = 0;
    for c in content.chars() {
        if c == '\n' {
            newline_run += 1;
            if newline_run <= 2 {
                out.push(c);
            }
        } else {
            newline_run = 0;
            out.push(c);
        }
    }
    out.trim_end().to_string()
}

/// Inserts, replaces, or (when `body` is blank) removes the `id` block.
/// Mirrors `upsertBlock`: an empty body deletes the region and restores
/// whatever surrounded it, rather than writing an empty block — this is how
/// clearing memory actually removes the `memory` block.
pub fn upsert_block(content: &str, id: &str, body: &str) -> String {
    let stripped = strip_block(content, id);
    let cleaned = collapse_blank_runs(&stripped);
    let trimmed_body = body.trim();
    if trimmed_body.is_empty() {
        return if cleaned.is_empty() {
            cleaned
        } else {
            format!("{cleaned}\n")
        };
    }
    let block = format!(
        "{}\n{}\n{}\n",
        open_marker(id),
        trimmed_body,
        close_marker(id)
    );
    if cleaned.is_empty() {
        block
    } else {
        format!("{cleaned}\n\n{block}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_preserves_existing_content_with_bounded_markers() {
        let result = upsert_block("existing content", "managed", "new body");
        assert!(result.starts_with("existing content"));
        assert!(result.contains(&open_marker("managed")));
        assert!(result.contains(&close_marker("managed")));
        assert!(result.contains("new body"));
    }

    #[test]
    fn replacing_in_place_does_not_duplicate_the_open_marker() {
        let first = upsert_block("top", "managed", "body one");
        let second = upsert_block(&first, "managed", "body two");
        assert_eq!(second.matches(&open_marker("managed")).count(), 1);
        assert!(!second.contains("body one"));
        assert!(second.contains("body two"));
        assert!(second.starts_with("top"));
    }

    #[test]
    fn two_distinct_block_ids_coexist_and_preserve_surrounding_content() {
        let with_managed = upsert_block("top content", "managed", "managed body");
        let with_both = upsert_block(&with_managed, "memory", "memory body");
        assert!(with_both.contains("top content"));
        assert!(with_both.contains("managed body"));
        assert!(with_both.contains("memory body"));

        let cleared_memory = upsert_block(&with_both, "memory", "");
        assert!(cleared_memory.contains("top content"));
        assert!(cleared_memory.contains("managed body"));
        assert!(!cleared_memory.contains("memory body"));
    }

    #[test]
    fn empty_body_removes_the_block_and_restores_prior_content() {
        let with_block = upsert_block("top", "managed", "body");
        let cleared = upsert_block(&with_block, "managed", "");
        assert!(!cleared.contains(&open_marker("managed")));
        assert!(cleared.contains("top"));
    }

    #[test]
    fn read_block_returns_none_when_absent() {
        assert_eq!(read_block("no markers here", "managed"), None);
    }

    #[test]
    fn read_block_round_trips_the_written_body() {
        let content = upsert_block("", "managed", "hello world");
        assert_eq!(
            read_block(&content, "managed"),
            Some("hello world".to_string())
        );
    }
}
