//! 重複・曖昧の解決UI（レイアウトのモック）。レイアウト仕様は docs/features/fingerprint-resolution-ui.md。
//!
//! 文言は「このアプリの管理データ（DB）内」の話だと必ず分かるようにする（ストレージ全体の重複検索と誤解させない）。
//!
//! 今は見た目の確認用で、未解決の項目は仮データ（DBとは結び付いていない）。「確定」「引き継がない」は
//! 仮データの行を消すだけ。本実装（R5）で、未解決の記録層（R1）・データ操作（R3）へ結び付け、
//! 文言を i18n へ移す（ここでは日本語直書き）。

use egui::{Align, Color32, Layout, RichText};

use super::NekoviewApp;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum PendingKind {
    CopySource,
    MoveTarget,
    FavoriteHandover,
}

impl PendingKind {
    const ALL: [PendingKind; 3] = [Self::CopySource, Self::MoveTarget, Self::FavoriteHandover];

    fn label(self) -> &'static str {
        match self {
            Self::CopySource => "引き継ぎ元の選択",
            Self::MoveTarget => "移動元の選択",
            Self::FavoriteHandover => "お気に入りの引き継ぎ",
        }
    }

    fn index(self) -> usize {
        Self::ALL.iter().position(|k| *k == self).unwrap_or(0)
    }

    /// 何が起きたか（入口その1）。
    fn situation(self, target: &str, n: usize) -> String {
        match self {
            Self::CopySource => format!(
                "「{target}」は、管理データにすでにある {n} 個のファイルと同じ内容です。コピーされたファイルかもしれません。"
            ),
            Self::MoveTarget => format!(
                "新しく見つかった「{target}」は、管理データにある「見つからなくなった」{n} 個のファイルと同じ内容です。移動または名前の変更で現れたのかもしれません。"
            ),
            Self::FavoriteHandover => format!(
                "お気に入りに登録されていた「{target}」が見つからなくなりました。管理データには、同じ内容のファイルが別の場所に {n} 個あります。"
            ),
        }
    }

    /// なぜ自動で決められないか（入口その2）。
    fn reason(self) -> &'static str {
        match self {
            Self::CopySource => {
                "コピー元の候補が複数あり、評価やタグなどの記録がそれぞれ違うため、どの記録を引き継ぐべきかをアプリは判断できません。誤って引き継ぐと、別のファイルの評価やタグが付いてしまいます。"
            }
            Self::MoveTarget => {
                "移動元の候補が複数あり、どのファイルが移動してきたのかをアプリは判断できません。誤って選ぶと、別のファイルの記録（評価・タグ・お気に入りなど）を取り違えてしまいます。"
            }
            Self::FavoriteHandover => {
                "別の場所へ移動しただけなのか、コピーしてから元を消したのかを、アプリは区別できません。移動しただけなら、そのファイルを開いた時に記録は自動で追従します。勝手にお気に入りを移すと、別のファイルに付けてしまうおそれがあります。"
            }
        }
    }

    /// あなたにしてほしいこと。
    fn ask(self) -> &'static str {
        match self {
            Self::CopySource => "記録を引き継ぎたいファイルを、下の一覧から 1 つ選んでください。",
            Self::MoveTarget => "移動元だったと思うファイルを、下の一覧から 1 つ選んでください。",
            Self::FavoriteHandover => "お気に入りを移したいファイルを、下の一覧から 1 つ選んでください。",
        }
    }

    /// 「確定する」を押すとどうなるか（出口その1）。
    fn on_confirm(self, target: &str) -> String {
        match self {
            Self::CopySource => format!(
                "確定すると、選んだファイルの★・タグ・しおり・見開き・ソート・登録サムネが「{target}」に複製されます。以後は別々に管理され、お気に入りは複製されません。"
            ),
            Self::MoveTarget => format!(
                "確定すると、選んだファイルの記録（お気に入りを含む）が「{target}」に移り、その分の「見つからないファイル」の記録が整理されます。選ばなかった候補は、保持日数が過ぎると自動で削除されます。"
            ),
            Self::FavoriteHandover => format!(
                "確定すると、「{target}」のお気に入りが選んだファイルに移り、お気に入り一覧で実在するファイルとして表示されます。"
            ),
        }
    }

    /// 「引き継がない」を押すとどうなるか（出口その2）。
    fn on_skip(self, target: &str) -> String {
        match self {
            Self::CopySource => format!("「{target}」は、記録のない新しいファイルとして扱います。"),
            Self::MoveTarget => format!(
                "「{target}」は記録のない新しいファイルとして扱い、候補の記録は「見つからないファイル」のまま残ります。"
            ),
            Self::FavoriteHandover => format!(
                "お気に入りは「{target}」のまま残り（実体なしで表示）、保持日数が過ぎると自動で削除されます。"
            ),
        }
    }

    fn candidates_heading(self) -> &'static str {
        match self {
            Self::CopySource => "管理データ内の、同じ内容のファイル（参照元の候補）",
            Self::MoveTarget => "管理データ内の、同じ内容の消えているファイル（移動元の候補）",
            Self::FavoriteHandover => "管理データ内の、同じ内容のファイル（お気に入りの移し先の候補）",
        }
    }
}

pub(super) struct MockCandidate {
    path: &'static str,
    present: bool,
    size: u64,
    mtime: i64,
    rating_half: u8,
    tag_count: usize,
    favorite: bool,
    /// 消えているファイルが最後に確認された日時（現存は None）。
    last_seen: Option<i64>,
}

pub(super) struct MockItem {
    kind: PendingKind,
    /// 対象（引き継ぎ元の選択・移動元の選択は新しいファイル、お気に入りの引き継ぎは実体の無い旧パス）。
    target: &'static str,
    target_size: u64,
    target_mtime: i64,
    detected_at: i64,
    candidates: Vec<MockCandidate>,
}

const DAY: i64 = 86_400;

pub(super) struct ResolveUiState {
    open: bool,
    filter: [bool; 3],
    selected: Option<usize>,
    second: Option<Second>,
    items: Vec<MockItem>,
}

struct Second {
    item: usize,
    chosen: Option<usize>,
}

impl Default for ResolveUiState {
    fn default() -> Self {
        Self {
            open: false,
            filter: [true; 3],
            selected: None,
            second: None,
            items: mock_items(crate::file_identity::now_unix()),
        }
    }
}

fn mock_items(now: i64) -> Vec<MockItem> {
    let cand = |path, present, size, days_ago: i64, rating_half, tag_count, favorite, gone_days: Option<i64>| MockCandidate {
        path,
        present,
        size,
        mtime: now - days_ago * DAY,
        rating_half,
        tag_count,
        favorite,
        last_seen: gone_days.map(|d| now - d * DAY),
    };
    vec![
        MockItem {
            kind: PendingKind::CopySource,
            target: "/home/user/書籍/整理/b_copy.zip",
            target_size: 12_900_000,
            target_mtime: now - DAY / 24,
            detected_at: now - DAY / 24,
            candidates: vec![
                cand("/home/user/書籍/原本/b.zip", true, 12_900_000, 2, 8, 2, true, None),
                cand("/home/user/バックアップ/b.zip", true, 12_900_000, 13, 4, 0, false, None),
            ],
        },
        MockItem {
            kind: PendingKind::MoveTarget,
            target: "/home/user/読書/新規/c_new.zip",
            target_size: 48_300_000,
            target_mtime: now - 3 * DAY,
            detected_at: now - DAY,
            candidates: vec![
                cand("/home/user/読書/旧/c.zip", false, 48_300_000, 40, 10, 3, true, Some(19)),
                cand("/home/user/読書/旧/c (1).zip", false, 48_300_000, 40, 0, 0, false, Some(50)),
                cand("/mnt/ext/c.zip", false, 48_300_000, 60, 6, 1, false, Some(58)),
            ],
        },
        MockItem {
            kind: PendingKind::FavoriteHandover,
            target: "/home/user/保管/d_old.zip",
            target_size: 5_200_000,
            target_mtime: now - 70 * DAY,
            detected_at: now - 2 * DAY,
            candidates: vec![cand("/home/user/読書/整理済み/d_old.zip", true, 5_200_000, 69, 7, 4, false, None)],
        },
        MockItem {
            kind: PendingKind::CopySource,
            target: "/home/user/漫画/シリーズA/x_dup.cbz",
            target_size: 88_000_000,
            target_mtime: now - 10 * DAY,
            detected_at: now - 4 * DAY,
            candidates: vec![
                cand("/home/user/漫画/シリーズA/x.cbz", true, 88_000_000, 30, 6, 1, false, None),
                cand("/home/user/漫画/まとめ/x.cbz", true, 88_000_000, 25, 9, 5, false, None),
                cand("/mnt/nas/漫画/x.cbz", true, 88_000_000, 20, 2, 0, false, None),
            ],
        },
        MockItem {
            kind: PendingKind::MoveTarget,
            target: "/home/user/ダウンロード/e_vol2.7z",
            target_size: 150_400_000,
            target_mtime: now - 8 * DAY,
            detected_at: now - 5 * DAY,
            candidates: vec![
                cand("/home/user/漫画/e_vol2.7z", false, 150_400_000, 90, 8, 2, true, Some(3)),
                cand("/home/user/漫画/旧/e_vol2.7z", false, 150_400_000, 91, 0, 0, false, Some(5)),
            ],
        },
    ]
}

// ---- 表示用の整形 ----

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// 「ファイル名（…/親フォルダ末尾）」。
fn short_path(path: &str) -> String {
    let p = std::path::Path::new(path);
    let name = p.file_name().and_then(|n| n.to_str()).unwrap_or(path);
    match p.parent().and_then(|d| d.file_name()).and_then(|n| n.to_str()) {
        Some(parent) => format!("{name}  (…/{parent})"),
        None => name.to_owned(),
    }
}

fn format_size(bytes: u64) -> String {
    if bytes >= 1_000_000_000 {
        format!("{:.1}GB", bytes as f64 / 1e9)
    } else if bytes >= 1_000_000 {
        format!("{:.1}MB", bytes as f64 / 1e6)
    } else {
        format!("{}KB", bytes / 1000)
    }
}

fn local_time(secs: i64) -> Option<time::OffsetDateTime> {
    let utc = time::OffsetDateTime::from_unix_timestamp(secs).ok()?;
    Some(
        time::UtcOffset::current_local_offset()
            .map(|o| utc.to_offset(o))
            .unwrap_or(utc),
    )
}

fn format_md(secs: i64) -> String {
    local_time(secs).map_or("-".into(), |t| format!("{:02}/{:02}", u8::from(t.month()), t.day()))
}

fn format_ymd_hm(secs: i64) -> String {
    local_time(secs).map_or("-".into(), |t| {
        format!("{:04}-{:02}-{:02} {:02}:{:02}", t.year(), u8::from(t.month()), t.day(), t.hour(), t.minute())
    })
}

fn format_rating(half: u8) -> String {
    match half {
        0 => "-".to_owned(),
        h if h % 2 == 0 => format!("★{}", h / 2),
        h => format!("★{}.5", h / 2),
    }
}

/// 削除まで残り何日か。実体が見つからないまま保持日数を過ぎると、日数による削除の対象になる。
/// `last_seen` が None（現存ファイル）は期限なし。
fn days_left(last_seen: Option<i64>, retention_days: u32, now: i64) -> Option<i64> {
    let since = last_seen?;
    let elapsed_days = (now - since).max(0) / DAY;
    Some(i64::from(retention_days) - elapsed_days)
}

/// 項目の「削除まで」。消えている候補のうち、最も早く削除されるものの残り日数。
/// お気に入りの引き継ぎは、対象（実体の無いお気に入り）自身の期限。
fn item_days_left(item: &MockItem, retention_days: u32, now: i64) -> Option<i64> {
    let own = (item.kind == PendingKind::FavoriteHandover)
        .then(|| days_left(Some(item.target_mtime), retention_days, now));
    if let Some(own) = own {
        return own;
    }
    item.candidates
        .iter()
        .filter_map(|c| days_left(c.last_seen, retention_days, now))
        .min()
}

fn days_left_text(days: Option<i64>) -> RichText {
    match days {
        None => RichText::new("期限なし"),
        Some(d) if d <= 0 => RichText::new("次回整理で削除").color(Color32::from_rgb(220, 90, 60)),
        Some(d) if d <= 7 => RichText::new(format!("残り {d} 日")).color(Color32::from_rgb(220, 150, 40)),
        Some(d) => RichText::new(format!("残り {d} 日")),
    }
}

/// 固定幅のセルを並べた、1行ぶんの選択可能な行。いずれかのセルのクリックで true。
fn selectable_row(ui: &mut egui::Ui, selected: bool, cells: &[(f32, RichText)]) -> egui::Response {
    const ROW_H: f32 = 22.0;
    let mut response: Option<egui::Response> = None;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        for (width, text) in cells {
            let r = ui.add_sized(
                [*width, ROW_H],
                egui::Button::selectable(selected, text.clone()).truncate(),
            );
            response = Some(match response.take() {
                Some(prev) => prev.union(r),
                None => r,
            });
        }
    });
    response.expect("row has cells")
}

fn header_row(ui: &mut egui::Ui, cells: &[(f32, &str)]) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        for (width, label) in cells {
            ui.add_sized([*width, 20.0], egui::Label::new(RichText::new(*label).strong()));
        }
    });
}

impl NekoviewApp {
    pub(super) fn open_identity_resolve(&mut self) {
        self.identity_resolve_ui.open = true;
        self.identity_resolve_ui.selected = None;
        self.identity_resolve_ui.second = None;
    }

    /// メニューバーの「重複の解決（n）」ボタン。
    pub(super) fn identity_resolve_button_label(&self) -> String {
        format!("DB内の重複の解決（{}）", self.identity_resolve_ui.items.len())
    }

    fn visible_resolve_items(&self) -> Vec<usize> {
        let ui = &self.identity_resolve_ui;
        ui.items
            .iter()
            .enumerate()
            .filter(|(_, item)| ui.filter[item.kind.index()])
            .map(|(i, _)| i)
            .collect()
    }

    pub(super) fn draw_identity_resolve_dialogs(&mut self, ctx: &egui::Context) {
        if !self.identity_resolve_ui.open {
            return;
        }
        self.draw_resolve_list_dialog(ctx);
        if self.identity_resolve_ui.second.is_some() {
            self.draw_resolve_detail_dialog(ctx);
        }
    }

    /// 第1ダイアログ: 未解決の項目の一覧。
    fn draw_resolve_list_dialog(&mut self, ctx: &egui::Context) {
        let retention = self.config.unconfirmed_retention_days;
        let now = crate::file_identity::now_unix();
        let visible = self.visible_resolve_items();
        // 絞り込みで見えなくなった行の選択は外す。
        if let Some(sel) = self.identity_resolve_ui.selected {
            if !visible.contains(&sel) {
                self.identity_resolve_ui.selected = None;
            }
        }
        let second_open = self.identity_resolve_ui.second.is_some();
        // ↑↓で選択を移動し、Enterで解決へ進む（第2ダイアログが開いている間は受けない）。
        let (mut key_move, mut key_enter) = (0i32, false);
        if !second_open {
            ctx.input(|i| {
                if i.key_pressed(egui::Key::ArrowDown) {
                    key_move = 1;
                }
                if i.key_pressed(egui::Key::ArrowUp) {
                    key_move = -1;
                }
                key_enter = i.key_pressed(egui::Key::Enter);
            });
        }
        if key_move != 0 && !visible.is_empty() {
            let pos = self.identity_resolve_ui.selected.and_then(|s| visible.iter().position(|v| *v == s));
            let next = match pos {
                None => 0,
                Some(p) => (p as i32 + key_move).clamp(0, visible.len() as i32 - 1) as usize,
            };
            self.identity_resolve_ui.selected = Some(visible[next]);
        }

        let widths = [150.0, 280.0, 44.0, 64.0, 110.0];
        let mut proceed = false;
        let mut close = false;
        let modal = egui::Modal::new(egui::Id::new("identity_resolve_list")).show(ctx, |ui| {
            ui.set_width(680.0);
            ui.heading("未解決の項目");
            ui.label("このアプリは、ファイルの内容で「同じファイル」かを見分け、移動や名前の変更のあとも評価・タグ・お気に入りなどを引き継いでいます。");
            ui.label("ただし、管理データ（DB）に同じ内容のファイルが複数あると、どれがどれの続きなのかを自動では決められないことがあります。ここで、その判断をしてください。");
            ui.weak("ストレージ全体の重複検索ではありません（管理データに記録のないファイルは対象外）。解決しなくてもアプリは使えますが、解決するまで該当ファイルは記録なしで表示されます。");
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label("絞り込み:");
                for kind in PendingKind::ALL {
                    ui.checkbox(&mut self.identity_resolve_ui.filter[kind.index()], kind.label());
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(format!("表示 {} / {} 件", visible.len(), self.identity_resolve_ui.items.len()));
                });
            });
            ui.add_space(4.0);
            header_row(
                ui,
                &[
                    (widths[0], "種別"),
                    (widths[1], "対象ファイル"),
                    (widths[2], "候補"),
                    (widths[3], "検出日"),
                    (widths[4], "削除まで"),
                ],
            );
            ui.separator();
            egui::ScrollArea::vertical().max_height(240.0).auto_shrink([false, true]).show(ui, |ui| {
                if visible.is_empty() {
                    ui.add_space(24.0);
                    ui.vertical_centered(|ui| ui.weak("未解決の項目はありません"));
                    ui.add_space(24.0);
                }
                for &i in &visible {
                    let item = &self.identity_resolve_ui.items[i];
                    let selected = self.identity_resolve_ui.selected == Some(i);
                    let resp = selectable_row(
                        ui,
                        selected,
                        &[
                            (widths[0], RichText::new(item.kind.label())),
                            (widths[1], RichText::new(short_path(item.target))),
                            (widths[2], RichText::new(item.candidates.len().to_string())),
                            (widths[3], RichText::new(format_md(item.detected_at))),
                            (widths[4], days_left_text(item_days_left(item, retention, now))),
                        ],
                    )
                    .on_hover_text(item.target);
                    if resp.clicked() {
                        self.identity_resolve_ui.selected = Some(i);
                    }
                    if resp.double_clicked() {
                        proceed = true;
                    }
                }
            });
            ui.separator();
            ui.weak("「削除まで」: 見つからないファイルの記録が自動で削除されるまでの日数です（設定→その他で変更）。削除されると、その候補は選べなくなります。");
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let can_proceed = self.identity_resolve_ui.selected.is_some();
                if ui.add_enabled(can_proceed, egui::Button::new("解決へ進む")).clicked() {
                    proceed = true;
                }
                if ui.button("閉じる").clicked() {
                    close = true;
                }
            });
        });
        if key_enter && self.identity_resolve_ui.selected.is_some() {
            proceed = true;
        }
        if proceed {
            if let Some(item) = self.identity_resolve_ui.selected {
                self.identity_resolve_ui.second = Some(Second { item, chosen: None });
            }
        }
        if (close || modal.should_close()) && !second_open {
            self.identity_resolve_ui.open = false;
        }
    }

    /// 第2ダイアログ: 参照元の選択。
    fn draw_resolve_detail_dialog(&mut self, ctx: &egui::Context) {
        let Some(second) = &self.identity_resolve_ui.second else { return };
        let item_index = second.item;
        let chosen = second.chosen;
        let retention = self.config.unconfirmed_retention_days;
        let now = crate::file_identity::now_unix();
        let Some(item) = self.identity_resolve_ui.items.get(item_index) else {
            self.identity_resolve_ui.second = None;
            return;
        };
        let widths = [48.0, 250.0, 56.0, 64.0, 108.0, 44.0, 44.0, 70.0, 96.0];
        let mut new_choice = chosen;
        let mut confirm = false;
        let mut skip = false;
        let mut back = false;
        let target_name = file_name(item.target).to_owned();
        let modal = egui::Modal::new(egui::Id::new("identity_resolve_detail")).show(ctx, |ui| {
            ui.set_width(800.0);
            ui.heading(format!("参照元の選択 ─ {}", item.kind.label()));
            ui.add_space(4.0);
            ui.label(RichText::new("対象ファイル").strong());
            ui.label(format!(
                "{}   {}   {}",
                item.target,
                format_size(item.target_size),
                format_ymd_hm(item.target_mtime)
            ));
            ui.add_space(8.0);
            ui.group(|ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new("何が起きたか").strong());
                ui.label(item.kind.situation(&target_name, item.candidates.len()));
                ui.add_space(4.0);
                ui.label(RichText::new("なぜ自動では決められないか").strong());
                ui.label(item.kind.reason());
                ui.add_space(4.0);
                ui.label(RichText::new("していただくこと").strong());
                ui.label(item.kind.ask());
            });
            ui.add_space(8.0);
            ui.label(RichText::new(item.kind.candidates_heading()).strong());
            header_row(
                ui,
                &[
                    (widths[0], "選択"),
                    (widths[1], "パス"),
                    (widths[2], "状態"),
                    (widths[3], "サイズ"),
                    (widths[4], "更新日時"),
                    (widths[5], "★"),
                    (widths[6], "タグ"),
                    (widths[7], "お気に入り"),
                    (widths[8], "削除まで"),
                ],
            );
            ui.separator();
            egui::ScrollArea::vertical().max_height(150.0).auto_shrink([false, true]).show(ui, |ui| {
                for (ci, c) in item.candidates.iter().enumerate() {
                    let selected = new_choice == Some(ci);
                    let resp = selectable_row(
                        ui,
                        selected,
                        &[
                            (widths[0], RichText::new(if selected { "●" } else { "○" })),
                            (widths[1], RichText::new(c.path)),
                            (widths[2], RichText::new(if c.present { "現存" } else { "消えている" })),
                            (widths[3], RichText::new(format_size(c.size))),
                            (widths[4], RichText::new(format_ymd_hm(c.mtime))),
                            (widths[5], RichText::new(format_rating(c.rating_half))),
                            (widths[6], RichText::new(c.tag_count.to_string())),
                            (widths[7], RichText::new(if c.favorite { "あり" } else { "なし" })),
                            (widths[8], days_left_text(days_left(c.last_seen, retention, now))),
                        ],
                    )
                    .on_hover_text(c.path);
                    if resp.clicked() {
                        new_choice = Some(ci);
                    }
                }
            });
            ui.add_space(8.0);
            ui.group(|ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new("確定すると").strong());
                ui.label(item.kind.on_confirm(&target_name));
                ui.add_space(4.0);
                ui.label(RichText::new("「引き継がない（空のまま）」にすると").strong());
                ui.label(item.kind.on_skip(&target_name));
                ui.add_space(4.0);
                ui.label(RichText::new("「戻る」と").strong());
                ui.label("何も変えず一覧へ戻ります。保留の間、対象は記録のないまま表示されます（評価などを付ければ、そのファイルの記録になります）。");
            });
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(new_choice.is_some(), egui::Button::new("選択した項目を参照元として確定する"))
                    .clicked()
                {
                    confirm = true;
                }
                if ui.button("引き継がない（空のまま）").clicked() {
                    skip = true;
                }
                if ui.button("戻る").clicked() {
                    back = true;
                }
            });
        });
        if let Some(second) = &mut self.identity_resolve_ui.second {
            second.chosen = new_choice;
        }
        if confirm || skip {
            // モック: 実際の紐付けは行わず、仮データの行を消すだけ。
            self.identity_resolve_ui.items.remove(item_index);
            self.identity_resolve_ui.selected = None;
            self.identity_resolve_ui.second = None;
            self.set_toast(if confirm {
                format!("（モック）「{target_name}」の参照元を確定しました")
            } else {
                format!("（モック）「{target_name}」は引き継がずに確定しました")
            });
        } else if back || modal.should_close() {
            self.identity_resolve_ui.second = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn days_left_counts_down_from_retention() {
        let now = 100 * DAY;
        assert_eq!(days_left(None, 60, now), None, "現存ファイルは期限なし");
        assert_eq!(days_left(Some(now), 60, now), Some(60));
        assert_eq!(days_left(Some(now - 19 * DAY), 60, now), Some(41));
        assert_eq!(days_left(Some(now - 60 * DAY), 60, now), Some(0));
        assert_eq!(days_left(Some(now - 70 * DAY), 60, now), Some(-10));
        // 未来の日時（時計ずれ）は経過0日として扱う。
        assert_eq!(days_left(Some(now + 5 * DAY), 60, now), Some(60));
        assert_eq!(days_left(Some(now), 120, now), Some(120));
        assert_eq!(days_left(Some(now), 1, now), Some(1));
    }

    #[test]
    fn mock_items_cover_every_kind_with_candidates() {
        let items = mock_items(100 * DAY);
        for kind in PendingKind::ALL {
            assert!(items.iter().any(|i| i.kind == kind), "{kind:?}");
        }
        assert!(items.iter().all(|i| !i.candidates.is_empty()));
    }

    #[test]
    fn item_days_left_uses_the_soonest_missing_candidate_or_own_deadline() {
        let now = 100 * DAY;
        let items = mock_items(now);
        // 引き継ぎ元の選択: 候補は現存のみ＝期限なし。
        let copy = items.iter().find(|i| i.kind == PendingKind::CopySource).unwrap();
        assert_eq!(item_days_left(copy, 60, now), None);
        // 移動元の選択: 消えている候補のうち、最も早く期限が来るもの（最終確認が58日前）。
        let moved = items.iter().find(|i| i.kind == PendingKind::MoveTarget && i.candidates.len() == 3).unwrap();
        assert_eq!(item_days_left(moved, 60, now), Some(2));
        // お気に入りの引き継ぎ: 対象（実体の無いお気に入り）自身の期限。
        let fav = items.iter().find(|i| i.kind == PendingKind::FavoriteHandover).unwrap();
        assert_eq!(item_days_left(fav, 60, now), Some(-10));
    }

    #[test]
    fn formatters_are_stable() {
        assert_eq!(format_rating(0), "-");
        assert_eq!(format_rating(8), "★4");
        assert_eq!(format_rating(7), "★3.5");
        assert_eq!(format_size(12_900_000), "12.9MB");
        assert_eq!(format_size(150_400_000), "150.4MB");
        assert_eq!(format_size(2_500_000_000), "2.5GB");
        assert_eq!(format_size(4_200), "4KB");
        assert_eq!(short_path("/home/user/書籍/整理/b.zip"), "b.zip  (…/整理)");
        assert_eq!(file_name("/a/b/c.zip"), "c.zip");
    }

    #[test]
    fn every_kind_explains_cause_and_outcome_and_names_the_target() {
        for kind in PendingKind::ALL {
            assert!(kind.situation("T.zip", 2).contains("T.zip"), "{kind:?}");
            assert!(kind.situation("T.zip", 2).contains('2'), "{kind:?}");
            assert!(!kind.reason().is_empty() && !kind.ask().is_empty(), "{kind:?}");
            assert!(kind.on_confirm("T.zip").contains("T.zip"), "{kind:?}");
            assert!(kind.on_skip("T.zip").contains("T.zip"), "{kind:?}");
            // 候補の見出しは、管理データ内の話だと分かる。
            assert!(kind.candidates_heading().contains("管理データ内"), "{kind:?}");
        }
    }
}
