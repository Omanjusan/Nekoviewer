//! 起動時の注意ダイアログ（ファイルID管理への移行の案内）を出すかどうかの判定。
//!
//! 旧版からの更新時に1回だけ出す。新規インストール（DBなし）と、既に対象バージョン以降で起動済みの場合は出さない。
//! 前回起動バージョンは state の `app_last_run_version`（[AppConfig::last_run_version](crate::config::AppConfig)）。

/// 注意ダイアログの対象になる最初のバージョン（major, minor, patch）。
pub const NOTICE_VERSION: (u32, u32, u32) = (1, 11, 0);

/// `"1.11.0"` や `"1.11.0-rc1"` を (major, minor, patch) に読む。読めなければ None。
/// 先頭の `v`、`-` 以降（rc等）は無視する。欠けた桁は0。
fn parse_version(s: &str) -> Option<(u32, u32, u32)> {
    let core = s.trim().trim_start_matches('v').split(['-', '+']).next()?;
    let mut it = core.split('.');
    let major = it.next()?.parse().ok()?;
    let minor = it.next().map_or(Some(0), |p| p.parse().ok())?;
    let patch = it.next().map_or(Some(0), |p| p.parse().ok())?;
    Some((major, minor, patch))
}

/// 注意ダイアログを出すか。
/// - `last_run`: 前回起動バージョン（空=記録なし）。
/// - `current`: 今のバージョン（`CARGO_PKG_VERSION`）。
/// - `db_existed`: 起動時点で `nekoviewer_spread.redb` が既にあったか（新規インストールの除外）。
///
/// 出す条件: DBが既にあり、今が対象バージョン以降で、前回が対象バージョン未満（記録なしを含む）。
pub fn should_show(last_run: &str, current: &str, db_existed: bool) -> bool {
    if !db_existed {
        return false;
    }
    let Some(cur) = parse_version(current) else { return false };
    if cur < NOTICE_VERSION {
        return false;
    }
    match parse_version(last_run) {
        // 記録なし（1.11より前の版は書かない）。読めない値も未記録と同じ扱い
        None => true,
        Some(last) => last < NOTICE_VERSION,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_version_reads_plain_rc_and_prefixed() {
        assert_eq!(parse_version("1.11.0"), Some((1, 11, 0)));
        assert_eq!(parse_version("1.11.0-rc1"), Some((1, 11, 0)));
        assert_eq!(parse_version("v1.10.0"), Some((1, 10, 0)));
        assert_eq!(parse_version("1.11"), Some((1, 11, 0)));
        assert_eq!(parse_version(""), None);
        assert_eq!(parse_version("abc"), None);
        assert_eq!(parse_version("1.x.0"), None);
    }

    #[test]
    fn shows_for_upgrade_from_older_or_unrecorded() {
        assert!(should_show("", "1.11.0", true), "記録なし=1.10以前からの更新");
        assert!(should_show("1.10.0", "1.11.0", true));
        assert!(should_show("1.9.0", "1.11.0", true));
        assert!(should_show("garbage", "1.11.0", true), "読めない値は未記録扱い");
    }

    #[test]
    fn hidden_for_new_install_and_after_first_run() {
        assert!(!should_show("", "1.11.0", false), "新規インストール（DBなし）");
        assert!(!should_show("1.11.0", "1.11.0", true), "既に1.11で起動済み");
        assert!(!should_show("1.12.0", "1.12.1", true));
    }

    #[test]
    fn hidden_when_running_a_version_before_the_notice() {
        assert!(!should_show("", "1.10.0", true), "対象バージョン未満の版では出さない");
        assert!(!should_show("1.9.0", "1.10.5", true));
    }

    #[test]
    fn rc_of_the_notice_version_counts_as_the_notice_version() {
        assert!(should_show("1.10.0", "1.11.0-rc1", true));
        assert!(!should_show("1.11.0-rc1", "1.11.0", true), "rcで見たなら本番では出さない");
    }
}
