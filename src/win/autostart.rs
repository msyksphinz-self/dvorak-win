//! タスクスケジューラによる自動起動 (管理者権限)
//!
//! スタートアップフォルダではなくタスクスケジューラを使う理由:
//! * 「最上位の特権で実行」にできる → 管理者権限のウィンドウ (タスクマネージャー等) でも Dvorak が効く
//!   (通常権限のフックは、管理者権限のウィンドウが前面のとき呼ばれない)
//! * タスクの既定値の「3 日で強制終了」「バッテリー駆動時は起動しない」「優先度: 低」を無効化できる
//! * 異常終了時に自動再起動できる

use std::os::windows::process::CommandExt;
use std::process::Command;

use super::util::exe_path;

pub const TASK_NAME: &str = "DvorakWin";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn task_xml() -> String {
    let user = match (std::env::var("USERDOMAIN"), std::env::var("USERNAME")) {
        (Ok(d), Ok(u)) => format!("{d}\\{u}"),
        (_, Ok(u)) => u,
        _ => String::new(),
    };
    let user = xml_escape(&user);
    let exe = xml_escape(&exe_path().to_string_lossy());
    format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <Description>Dvorak-Win: Dvorak keyboard remapper</Description>
  </RegistrationInfo>
  <Triggers>
    <LogonTrigger>
      <Enabled>true</Enabled>
      <UserId>{user}</UserId>
    </LogonTrigger>
  </Triggers>
  <Principals>
    <Principal id="Author">
      <UserId>{user}</UserId>
      <LogonType>InteractiveToken</LogonType>
      <RunLevel>HighestAvailable</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <AllowHardTerminate>true</AllowHardTerminate>
    <StartWhenAvailable>false</StartWhenAvailable>
    <RunOnlyIfNetworkAvailable>false</RunOnlyIfNetworkAvailable>
    <IdleSettings>
      <StopOnIdleEnd>false</StopOnIdleEnd>
      <RestartOnIdle>false</RestartOnIdle>
    </IdleSettings>
    <AllowStartOnDemand>true</AllowStartOnDemand>
    <Enabled>true</Enabled>
    <Hidden>false</Hidden>
    <RunOnlyIfIdle>false</RunOnlyIfIdle>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Priority>4</Priority>
    <RestartOnFailure>
      <Interval>PT1M</Interval>
      <Count>999</Count>
    </RestartOnFailure>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>"{exe}"</Command>
    </Exec>
  </Actions>
</Task>
"#
    )
}

fn schtasks(args: &[&str]) -> std::io::Result<bool> {
    Ok(Command::new("schtasks.exe")
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .output()?
        .status
        .success())
}

pub fn is_installed() -> bool {
    schtasks(&["/Query", "/TN", TASK_NAME]).unwrap_or(false)
}

/// 管理者権限で呼ぶこと
pub fn install() -> Result<(), String> {
    let path = std::env::temp_dir().join("dvorak-win-task.xml");
    // schtasks は UTF-16 (BOM 付き) の XML を確実に読める
    let mut bytes = vec![0xFF, 0xFE];
    for u in task_xml().encode_utf16() {
        bytes.extend_from_slice(&u.to_le_bytes());
    }
    std::fs::write(&path, bytes).map_err(|e| e.to_string())?;
    let p = path.to_string_lossy().to_string();
    let r = schtasks(&["/Create", "/TN", TASK_NAME, "/XML", &p, "/F"]);
    let _ = std::fs::remove_file(&path);
    match r {
        Ok(true) => Ok(()),
        Ok(false) => Err("schtasks /Create に失敗しました".into()),
        Err(e) => Err(e.to_string()),
    }
}

pub fn uninstall() -> Result<(), String> {
    match schtasks(&["/Delete", "/TN", TASK_NAME, "/F"]) {
        Ok(true) => Ok(()),
        Ok(false) => Err("schtasks /Delete に失敗しました".into()),
        Err(e) => Err(e.to_string()),
    }
}
