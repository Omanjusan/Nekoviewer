//! 評価ソート・評価フィルタの性能確認用フィクスチャー（debug ビルド限定）。
//! アイテムカードの右クリックから、表示中の全件の評価を一括で書き換える。
//! 実DBの評価を書き換えるので、リリースビルドには含めない（`mod` 宣言側で cfg している）。

use std::path::PathBuf;

use super::*;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum RatingFixture {
    AllFive,
    AllUnrated,
    Random,
}

impl RatingFixture {
    pub(super) const MENU: [(RatingFixture, &'static str); 3] = [
        (RatingFixture::AllFive, "全件 ★5.0 にする"),
        (RatingFixture::AllUnrated, "全件 未評価にする"),
        (RatingFixture::Random, "全件 ランダム評価にする"),
    ];

    fn value(self, rng: &mut u64) -> u8 {
        match self {
            Self::AllFive => 10,
            Self::AllUnrated => 0,
            Self::Random => next_random_half(rng),
        }
    }
}

/// xorshift64 で 0..=10（未評価〜★5.0）の半星値を返す
fn next_random_half(state: &mut u64) -> u8 {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    *state = x;
    (x % 11) as u8
}

impl NekoviewApp {
    /// 表示中（フィルタ通過後）の全アーカイブの評価を書き換える。生画像は評価の対象外なので除く。
    pub(super) fn apply_rating_fixture(&mut self, fixture: RatingFixture) {
        let Some(db) = self.spread_db.clone() else { return };
        let mut rng = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0x9E37_79B9_7F4A_7C15, |d| d.as_nanos() as u64)
            | 1;
        let targets: Vec<PathBuf> = self.filtered_indices.iter()
            .filter_map(|&i| self.archives.get(i))
            .filter(|p| !self.raw_image_files.contains(*p))
            .cloned()
            .collect();
        let items: Vec<(PathBuf, u8)> = targets.iter().map(|p| (p.clone(), fixture.value(&mut rng))).collect();
        let written = crate::spread_state::write_archive_ratings_bulk(&db, &items);

        // キャッシュを捨ててフォルダ単位で取り込み直し、並び・フィルタへ反映する
        for p in &targets {
            self.archive_rating_cache.remove(p);
        }
        self.preload_archive_ratings();
        self.resort_keeping_selection();
        self.recompute_filter();
        self.app_toast = Some((
            format!("[debug] 評価フィクスチャー: {written}/{}件を書き換えた", targets.len()),
            std::time::Instant::now(),
        ));
        self.egui_ctx.request_repaint();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn random_half_stays_in_range_and_covers_every_value() {
        let mut rng = 0x1234_5678_9ABC_DEF1_u64;
        let mut seen = [false; 11];
        for _ in 0..2000 {
            let v = next_random_half(&mut rng);
            assert!(v <= 10);
            seen[v as usize] = true;
        }
        assert!(seen.iter().all(|&s| s), "0..=10 が全て出る");
    }

    #[test]
    fn fixed_fixtures_ignore_the_rng() {
        let mut rng = 1;
        assert_eq!(RatingFixture::AllFive.value(&mut rng), 10);
        assert_eq!(RatingFixture::AllUnrated.value(&mut rng), 0);
    }
}
