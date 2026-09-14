//! Windowsエクスプローラーの右クリックメニュー「Nekoviewerで開く」の登録/削除。
//!
//! HKEY_CURRENT_USER配下のみを操作するため管理者権限は不要。対応拡張子の各ファイルと
//! Directory（フォルダ）双方に `shell\Nekoviewer\command` を追加し、コマンドラインには
//! Nekoviewer本体を `"<exe>" "%1"` の形で渡すだけにする。渡されたパスの解釈（画像なら
//! 親フォルダを開いてジャンプ、アーカイブなら中に入る等）は起動時の
//! [`crate::config::AppConfig::resolve_cli_open_target`] 側の「賢く開く」ロジックに委ねる
//! （インストーラを持たない配布形態のため、この登録/削除は設定画面のボタンから呼ぶ想定）。

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::path::PathBuf;
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::ERROR_FILE_NOT_FOUND;
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_READ, KEY_WRITE, REG_OPTION_NON_VOLATILE, REG_SZ,
    RegCloseKey, RegCreateKeyExW, RegDeleteTreeW, RegOpenKeyExW, RegSetValueExW,
};

const CLASSES_ROOT: &str = "Software\\Classes\\";

fn to_wide(s: &str) -> Vec<u16> {
    OsStr::new(s).encode_wide().chain(std::iter::once(0)).collect()
}

/// 登録/削除対象の「単純な」拡張子一覧（ドットなし、小文字）。
/// 画像拡張子 + 単純アーカイブ拡張子（複合拡張子は対象外、[[simple_archive_extensions]]参照）。
fn target_extensions() -> Vec<&'static str> {
    let mut exts: Vec<&'static str> = crate::fs::archive::detect::IMAGE_EXTENSIONS.to_vec();
    exts.extend(crate::fs::dir::simple_archive_extensions());
    exts
}

/// `HKEY_CURRENT_USER\Software\Classes\<subkey>` を（無ければ作成して）開き、
/// 既定値へ文字列を書き込む。
fn set_default_value(subkey: &str, value: &str) -> Result<(), String> {
    let full = format!("{CLASSES_ROOT}{subkey}");
    let subkey_w = to_wide(&full);
    let value_w = to_wide(value);

    unsafe {
        let mut hkey: HKEY = 0;
        let status = RegCreateKeyExW(
            HKEY_CURRENT_USER,
            subkey_w.as_ptr(),
            0,
            null(),
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            null(),
            &mut hkey,
            null_mut(),
        );
        if status != 0 {
            return Err(format!("RegCreateKeyExW failed ({status}): {full}"));
        }

        let data_ptr = value_w.as_ptr() as *const u8;
        let data_len = (value_w.len() * 2) as u32;
        let status = RegSetValueExW(hkey, null(), 0, REG_SZ, data_ptr, data_len);
        RegCloseKey(hkey);
        if status != 0 {
            return Err(format!("RegSetValueExW failed ({status}): {full}"));
        }
    }
    Ok(())
}

/// `HKEY_CURRENT_USER\Software\Classes\<subkey>` をサブキーごと削除する。
/// 元々存在しない場合（ERROR_FILE_NOT_FOUND）は成功扱いにする（冪等な削除）。
fn delete_tree(subkey: &str) -> Result<(), String> {
    let full = format!("{CLASSES_ROOT}{subkey}");
    let subkey_w = to_wide(&full);
    unsafe {
        let status = RegDeleteTreeW(HKEY_CURRENT_USER, subkey_w.as_ptr());
        if status != 0 && status != ERROR_FILE_NOT_FOUND {
            return Err(format!("RegDeleteTreeW failed ({status}): {full}"));
        }
    }
    Ok(())
}

/// 登録対象キー（`Software\Classes\`からの相対パス、`\shell\Nekoviewer`まで）一覧。
fn target_keys() -> Vec<String> {
    let mut keys: Vec<String> = target_extensions()
        .into_iter()
        .map(|ext| format!(".{ext}\\shell\\Nekoviewer"))
        .collect();
    keys.push("Directory\\shell\\Nekoviewer".to_string());
    keys
}

fn exe_command() -> Result<String, String> {
    let exe: PathBuf = std::env::current_exe()
        .map_err(|e| format!("実行ファイルパスの取得に失敗: {e}"))?;
    Ok(format!("\"{}\" \"%1\"", exe.display()))
}

/// 右クリックメニューへの登録を行う。既に登録済みの項目は上書きする（exeパス変更等に追従）。
pub fn register() -> Result<(), String> {
    let command = exe_command()?;
    let label = crate::i18n::t().windows_context_menu_label();

    for key in target_keys() {
        set_default_value(&key, label)?;
        set_default_value(&format!("{key}\\command"), &command)?;
    }
    Ok(())
}

/// 右クリックメニューへの登録を削除する。未登録の項目があっても無視して続行する。
pub fn unregister() -> Result<(), String> {
    for key in target_keys() {
        delete_tree(&key)?;
    }
    Ok(())
}

/// 現在登録済みかどうか（代表として`Directory\shell\Nekoviewer\command`の存在を見る）。
pub fn is_registered() -> bool {
    let full = format!("{CLASSES_ROOT}Directory\\shell\\Nekoviewer\\command");
    let subkey_w = to_wide(&full);
    unsafe {
        let mut hkey: HKEY = 0;
        let status = RegOpenKeyExW(HKEY_CURRENT_USER, subkey_w.as_ptr(), 0, KEY_READ, &mut hkey);
        if status == 0 {
            RegCloseKey(hkey);
            true
        } else {
            false
        }
    }
}
