//! GPUテクスチャを保持するページの窓（表示中のページの前後）を、VRAM予算に合わせて決める。
//! 先読みしたデコード結果はRAM（PageCache）に持っているので、GPUへ上げるのは表示ページと
//! その周辺だけで足りる。1ページが大きい（原寸デコードなど）ほど窓を絞り、VRAMの使いすぎを防ぐ。
//! 窓の外へ出たテクスチャは解放されるが、RAMのデコード結果は残るので、再表示時はGPUへ
//! 上げ直すだけで済む。

use crate::cache::{PREFETCH_AHEAD, PREFETCH_BEHIND, PREFETCH_WINDOW};

/// VRAM予算の既定(MB)。
pub const DEFAULT_VRAM_BUDGET_MB: u32 = 512;
const VRAM_BUDGET_MB_RANGE: (u32, u32) = (64, 16384);

/// 表示中でないページのテクスチャを、1フレームにGPUへ上げてよい枚数の既定。
/// 窓が動いたとき（ページ送り・窓の拡大・解像度の切替）に、アップロードが1フレームに
/// 集中してカクつくのを避ける。表示中のページは対象外（すぐに上げる）。
pub const DEFAULT_BACKGROUND_UPLOADS_PER_FRAME: u32 = 1;
const BACKGROUND_UPLOADS_RANGE: (u32, u32) = (1, 16);

/// テクスチャ窓の設定値。setter は「範囲に丸めて適用し、適用後の値を返す」。
/// GUI設定へ昇格するときはこの setter を呼ぶだけで済む（永続化は現状スコープ外）。
#[derive(Clone, Debug, PartialEq)]
pub struct TextureWindowConfig {
    vram_budget_mb: u32,
    background_uploads_per_frame: u32,
}

impl Default for TextureWindowConfig {
    fn default() -> Self {
        Self {
            vram_budget_mb: DEFAULT_VRAM_BUDGET_MB,
            background_uploads_per_frame: DEFAULT_BACKGROUND_UPLOADS_PER_FRAME,
        }
    }
}

impl TextureWindowConfig {
    pub fn vram_budget_mb(&self) -> u32 { self.vram_budget_mb }
    pub fn vram_budget_bytes(&self) -> u64 { self.vram_budget_mb as u64 * 1024 * 1024 }
    pub fn background_uploads_per_frame(&self) -> u32 { self.background_uploads_per_frame }

    pub fn set_vram_budget_mb(&mut self, mb: u32) -> u32 {
        self.vram_budget_mb = mb.clamp(VRAM_BUDGET_MB_RANGE.0, VRAM_BUDGET_MB_RANGE.1);
        self.vram_budget_mb
    }

    pub fn set_background_uploads_per_frame(&mut self, n: u32) -> u32 {
        self.background_uploads_per_frame = n.clamp(BACKGROUND_UPLOADS_RANGE.0, BACKGROUND_UPLOADS_RANGE.1);
        self.background_uploads_per_frame
    }
}

/// GPUへ保持するページ数。1ページのVRAM使用量(`page_bytes`)で予算を割り、
/// 「表示中(`visible_span`枚) の前に1枚・後ろに `visible_span` 枚」を下限、
/// 先読み窓(`PREFETCH_WINDOW`)を上限にする。ページの大きさが不明(0)なら上限。
pub fn window_pages(page_bytes: u64, budget_bytes: u64, visible_span: usize) -> usize {
    let min_pages = 1 + 2 * visible_span.max(1);
    if page_bytes == 0 {
        return PREFETCH_WINDOW;
    }
    let by_budget = usize::try_from(budget_bytes / page_bytes).unwrap_or(usize::MAX);
    by_budget.clamp(min_pages, PREFETCH_WINDOW.max(min_pages))
}

/// 現在ページ `anchor` を基準にした窓 [start, end)。`pages` が先読み窓以上なら従来どおり
/// （後ろ5枚・前10枚）。絞るときは、後ろを1/4・前を3/4に配分し、必ず表示中のページを含める。
pub fn window_bounds(anchor: usize, total: usize, pages: usize, visible_span: usize) -> (usize, usize) {
    if pages >= PREFETCH_WINDOW {
        return (anchor.saturating_sub(PREFETCH_BEHIND), (anchor + PREFETCH_AHEAD + 1).min(total));
    }
    let span = visible_span.max(1);
    let behind = ((pages.saturating_sub(span)) / 4).max(1);
    let start = anchor.saturating_sub(behind);
    let end = (start + pages).min(total);
    // 末尾付近で前側が余るぶんは、後ろ側へ回して枚数を保つ。
    let start = if end - start < pages { end.saturating_sub(pages) } else { start };
    (start, end)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MB: u64 = 1024 * 1024;

    #[test]
    fn config_defaults_and_clamping() {
        let mut c = TextureWindowConfig::default();
        assert_eq!(c.vram_budget_mb(), 512);
        assert_eq!(c.vram_budget_bytes(), 512 * MB);
        assert_eq!(c.background_uploads_per_frame(), 1);
        assert_eq!(c.set_vram_budget_mb(1), 64);
        assert_eq!(c.set_vram_budget_mb(100_000), 16384);
        assert_eq!(c.set_vram_budget_mb(1024), 1024);
        assert_eq!(c.set_background_uploads_per_frame(0), 1);
        assert_eq!(c.set_background_uploads_per_frame(99), 16);
    }

    #[test]
    fn small_pages_keep_the_full_prefetch_window() {
        // 1920px級（約10MB）: 512MB ÷ 10MB = 51枚 → 上限 16枚。
        assert_eq!(window_pages(10 * MB, 512 * MB, 1), PREFETCH_WINDOW);
        // 大きさ不明（テクスチャ未取得）は上限。
        assert_eq!(window_pages(0, 512 * MB, 1), PREFETCH_WINDOW);
    }

    #[test]
    fn large_pages_shrink_the_window() {
        // 4000px級（約45MB）: 512MB ÷ 45MB = 11枚。
        assert_eq!(window_pages(45 * MB, 512 * MB, 1), 11);
        // 7680px級（約167MB）: 3枚。
        assert_eq!(window_pages(167 * MB, 512 * MB, 1), 3);
    }

    #[test]
    fn window_never_drops_below_visible_plus_neighbors() {
        // 予算を超える大きさでも、単ページは3枚・見開きは5枚を下回らない。
        assert_eq!(window_pages(900 * MB, 512 * MB, 1), 3);
        assert_eq!(window_pages(900 * MB, 512 * MB, 2), 5);
        // 極端な入力でもあふれない。
        assert_eq!(window_pages(1, u64::MAX, 1), PREFETCH_WINDOW);
    }

    #[test]
    fn full_window_matches_the_existing_prefetch_bounds() {
        assert_eq!(window_bounds(20, 100, PREFETCH_WINDOW, 1), (15, 31));
        // 先頭・末尾はクランプ。
        assert_eq!(window_bounds(2, 100, PREFETCH_WINDOW, 1), (0, 13));
        assert_eq!(window_bounds(98, 100, PREFETCH_WINDOW, 1), (93, 100));
    }

    #[test]
    fn narrowed_window_leans_forward_and_contains_the_visible_pages() {
        // 11枚: 後ろ2・前8（現在ページ含む）。
        let (start, end) = window_bounds(20, 100, 11, 1);
        assert_eq!((start, end), (18, 29));
        assert!((start..end).contains(&20));
        // 3枚: 後ろ1・現在・前1。
        assert_eq!(window_bounds(20, 100, 3, 1), (19, 22));
        // 見開き5枚（表示は anchor と anchor+1）。前に2枚の先読みがある。
        let (start, end) = window_bounds(20, 100, 5, 2);
        assert_eq!((start, end), (19, 24));
        assert!((start..end).contains(&20) && (start..end).contains(&21));
    }

    #[test]
    fn narrowed_window_keeps_its_size_at_the_edges() {
        // 先頭: 後ろが足りないぶん、前へ延ばす。
        assert_eq!(window_bounds(0, 100, 11, 1), (0, 11));
        // 末尾: 前が足りないぶん、後ろへ延ばす。
        assert_eq!(window_bounds(99, 100, 11, 1), (89, 100));
        // 総ページ数が窓より少ない。
        assert_eq!(window_bounds(1, 4, 11, 1), (0, 4));
    }
}
