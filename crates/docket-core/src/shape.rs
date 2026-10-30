//! The read contract's pure half ([ADR-0029](../../../docs/decisions/ADR-0029-the-core-owns-the-read-contract.md)):
//! narrowing a page of rows to the fields a caller named and to a character budget.
//!
//! The core knows rows, fields and characters -- never tokens. These functions work on already-serialised rows
//! (`serde_json::Value` objects) so every list endpoint shares one implementation.

use serde_json::Value;

/// Splits a `fields=a,b,c` query value into names, dropping blanks.
pub fn parse_fields(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|f| !f.is_empty())
        .map(str::to_string)
        .collect()
}

/// Rejects a requested field that is not one of `valid`, naming the valid ones -- a typo would otherwise
/// silently return rows with the column missing.
pub fn check_fields(fields: &[String], valid: &[&str]) -> Result<(), String> {
    match fields.iter().find(|f| !valid.contains(&f.as_str())) {
        None => Ok(()),
        Some(bad) => Err(format!(
            "unknown field `{bad}`; valid fields: {}",
            valid.join(", ")
        )),
    }
}

/// Keeps, per object row, only the named `fields` plus every name in `always`.
pub fn project(rows: Vec<Value>, fields: &[String], always: &[&str]) -> Vec<Value> {
    if fields.is_empty() {
        return rows;
    }
    rows.into_iter()
        .map(|row| match row {
            Value::Object(map) => Value::Object(
                map.into_iter()
                    .filter(|(k, _)| always.contains(&k.as_str()) || fields.iter().any(|f| f == k))
                    .collect(),
            ),
            other => other,
        })
        .collect()
}

/// Keeps rows from the front while their serialised length (plus a separator) stays within `max_chars`.
/// The first row is always kept, so a budget smaller than a row cannot leave the caller with an empty page
/// and no way forward. Returns the kept rows and how many were dropped.
pub fn within(rows: Vec<Value>, max_chars: Option<usize>) -> (Vec<Value>, usize) {
    let Some(budget) = max_chars else {
        return (rows, 0);
    };
    let total = rows.len();
    let mut used = 0usize;
    let mut kept = 0usize;
    for row in &rows {
        let cost = row.to_string().chars().count() + usize::from(kept > 0);
        if kept > 0 && used + cost > budget {
            break;
        }
        used += cost;
        kept += 1;
    }
    (rows.into_iter().take(kept).collect(), total - kept)
}

/// Narrows a whole-table listing by `q` (a case-insensitive substring of the row's name -- the first
/// string found under `name_keys` -- or of any of its `aliases`) and caps it at `limit` rows. Returns the
/// kept rows, how many matched, and how many `limit` held back. A `limit` of 0 still returns one row, for
/// the same reason `within` does.
pub fn narrow_table(
    rows: Vec<Value>,
    q: Option<&str>,
    limit: Option<usize>,
    name_keys: &[&str],
) -> (Vec<Value>, usize, usize) {
    let needle = q
        .map(str::trim)
        .filter(|q| !q.is_empty())
        .map(str::to_lowercase);
    let matches = |row: &Value| -> bool {
        let Some(needle) = needle.as_deref() else {
            return true;
        };
        // Only a string counts as the name: an identity row carries a numeric `topic` count.
        let name = name_keys
            .iter()
            .find_map(|k| row.get(*k).and_then(Value::as_str))
            .unwrap_or_default();
        let aliases = row.get("aliases").and_then(Value::as_array);
        std::iter::once(name)
            .chain(aliases.into_iter().flatten().filter_map(Value::as_str))
            .any(|s| s.to_lowercase().contains(needle))
    };
    let matched: Vec<Value> = rows.into_iter().filter(|r| matches(r)).collect();
    let total = matched.len();
    let cap = limit.map_or(total, |l| l.max(1));
    let kept: Vec<Value> = matched.into_iter().take(cap).collect();
    let omitted = total - kept.len();
    (kept, total, omitted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn rows(n: usize) -> Vec<Value> {
        (0..n)
            .map(|i| json!({ "id": format!("id-{i}"), "title": format!("title {i}"), "state": "open", "tags": ["a"] }))
            .collect()
    }

    #[test]
    fn parse_fields_splits_on_commas_and_drops_blanks() {
        assert_eq!(
            parse_fields("title, state,,tags "),
            ["title", "state", "tags"]
        );
        assert!(parse_fields("").is_empty());
    }

    #[test]
    fn check_fields_names_the_valid_choices() {
        let valid = ["id", "title", "state"];
        assert!(check_fields(&["title".into()], &valid).is_ok());
        let err = check_fields(&["titel".into()], &valid).unwrap_err();
        assert!(
            err.contains("titel") && err.contains("title") && err.contains("state"),
            "{err}"
        );
    }

    #[test]
    fn project_keeps_the_named_fields_and_the_always_set() {
        let out = project(rows(2), &["title".to_string()], &["id"]);
        for row in &out {
            let mut keys: Vec<&str> = row
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect();
            keys.sort();
            assert_eq!(keys, ["id", "title"]);
        }
        // No fields asked for: untouched.
        assert_eq!(project(rows(1), &[], &["id"]), rows(1));
    }

    #[test]
    fn within_holds_rows_back_at_the_budget_and_counts_them() {
        let all = rows(6);
        let one = all[0].to_string().chars().count();
        let (kept, dropped) = within(all, Some(2 * one + 3));
        assert_eq!((kept.len(), dropped), (2, 4));
    }

    #[test]
    fn within_always_keeps_the_first_row_and_is_quiet_without_a_budget() {
        let (kept, dropped) = within(rows(3), Some(1));
        assert_eq!((kept.len(), dropped), (1, 2));
        let (kept, dropped) = within(rows(3), None);
        assert_eq!((kept.len(), dropped), (3, 0));
        let (kept, dropped) = within(rows(3), Some(1_000_000));
        assert_eq!((kept.len(), dropped), (3, 0));
        let (kept, dropped) = within(Vec::new(), Some(10));
        assert_eq!((kept.len(), dropped), (0, 0));
    }

    fn table(n: usize) -> Vec<Value> {
        (0..n)
            .map(|i| json!({ "topic": format!("iyulab/repo-{i}"), "count": i, "aliases": [format!("short-{i}")] }))
            .collect()
    }

    #[test]
    fn narrow_table_matches_the_name_or_an_alias_ignoring_case() {
        let mut rows = table(3);
        rows.push(json!({ "topic": "acme/IronHive", "count": 9, "aliases": [] }));
        let (kept, total, omitted) = narrow_table(rows.clone(), Some("ironhive"), None, &["topic"]);
        assert_eq!((kept.len(), total, omitted), (1, 1, 0));
        let (kept, total, _) = narrow_table(rows, Some("SHORT-2"), None, &["topic"]);
        assert_eq!((kept.len(), total), (1, 1), "an alias is searchable too");
    }

    #[test]
    fn narrow_table_reads_only_a_string_as_the_name() {
        // An identity row carries a numeric `topic` count next to its string `identity`.
        let rows = vec![json!({ "identity": "acme/filer", "topic": 0, "aliases": [] })];
        let (kept, total, _) = narrow_table(rows, Some("filer"), None, &["topic", "identity"]);
        assert_eq!((kept.len(), total), (1, 1));
    }

    #[test]
    fn narrow_table_caps_without_a_default_and_counts_what_it_held_back() {
        let (kept, total, omitted) = narrow_table(table(120), None, None, &["topic"]);
        assert_eq!(
            (kept.len(), total, omitted),
            (120, 120, 0),
            "no default cap on the server"
        );
        let (kept, total, omitted) = narrow_table(table(120), None, Some(7), &["topic"]);
        assert_eq!((kept.len(), total, omitted), (7, 120, 113));
        let (kept, _, omitted) = narrow_table(table(5), None, Some(0), &["topic"]);
        assert_eq!((kept.len(), omitted), (1, 4));
    }

    #[test]
    fn narrow_table_keeps_the_original_order() {
        let (kept, _, _) = narrow_table(table(10), Some("repo"), Some(3), &["topic"]);
        let names: Vec<&str> = kept.iter().map(|r| r["topic"].as_str().unwrap()).collect();
        assert_eq!(names, ["iyulab/repo-0", "iyulab/repo-1", "iyulab/repo-2"]);
    }
}
