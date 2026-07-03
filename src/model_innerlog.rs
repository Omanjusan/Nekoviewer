//! GUI表示用のグローバルログリングバッファ。log_key!/log_common! から書き込まれ、
//! view_innerlog がステータス窓に表示する。ターミナル(eprintln!)出力とは別経路で、
//! perfログ(高頻度)はここには流さない（config.rs の log_perf! 参照）。

use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};

const CAPACITY: usize = 1000;

static LOG: OnceLock<Mutex<VecDeque<String>>> = OnceLock::new();

fn log() -> &'static Mutex<VecDeque<String>> {
    LOG.get_or_init(|| Mutex::new(VecDeque::with_capacity(CAPACITY)))
}

pub fn push(msg: &str) {
    let mut buf = log().lock().unwrap();
    if buf.len() >= CAPACITY {
        buf.pop_front();
    }
    buf.push_back(msg.to_string());
}

pub fn snapshot() -> Vec<String> {
    log().lock().unwrap().iter().cloned().collect()
}
