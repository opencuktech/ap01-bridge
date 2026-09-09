//! 写锁的拒绝、恢复、释放、只读隔离与跨进程并发回归测试。

mod common;

use ap01_bridge_kernel::{gif, store};
use ap01_gif::testkit::{Frame, GifBuilder, quota_gif};
use common::{TempDir, bridge};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

const NOW: u64 = 1_788_668_411;
const BUSY: &str = "另一个写命令正在运行，请稍后重试";

fn command(dir: &Path) -> Command {
    let mut command = bridge();
    command
        .arg("--data-dir")
        .arg(dir)
        .arg("--json")
        .env("AP01_BRIDGE_FAKE_NOW", NOW.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

fn invoke(dir: &Path, args: &[&str]) -> Output {
    command(dir).args(args).output().unwrap()
}

fn result(output: Output, code: i32) -> Value {
    assert_eq!(output.status.code(), Some(code), "命令结果：{output:?}");
    assert!(output.stderr.is_empty(), "标准错误：{output:?}");
    assert_eq!(String::from_utf8_lossy(&output.stdout).lines().count(), 1);
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["ok"], code == 0);
    if code != 0 {
        assert_eq!(value["error"]["code"], code);
    }
    value
}

fn busy(output: Output) {
    assert_eq!(
        result(output, 4),
        json!({"ok": false, "error": {"code": 4, "message": BUSY}})
    );
}

fn fixture(root: &Path, name: &str, bytes: &[u8]) -> String {
    let path = root.join(name);
    fs::write(&path, bytes).unwrap();
    path.to_str().unwrap().into()
}

fn frame(delay: u16) -> Vec<u8> {
    GifBuilder::default()
        .frame(Frame {
            delay_cs: Some(delay),
            ..Frame::default()
        })
        .build()
}

fn snapshot(dir: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    fn walk(root: &Path, path: &Path, entries: &mut BTreeMap<PathBuf, Option<Vec<u8>>>) {
        // Windows 排他锁可能拒绝读取锁文件；这里只比较业务状态。
        if path == root.join("write.lock") || !path.exists() {
            return;
        }
        let relative = path.strip_prefix(root).unwrap().to_path_buf();
        if path.is_dir() {
            entries.insert(relative, None);
            for entry in fs::read_dir(path).unwrap() {
                walk(root, &entry.unwrap().path(), entries);
            }
        } else {
            entries.insert(relative, Some(fs::read(path).unwrap()));
        }
    }
    let mut entries = BTreeMap::new();
    walk(dir, dir, &mut entries);
    entries
}

fn lock(dir: &Path) -> File {
    fs::create_dir_all(dir).unwrap();
    let file = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.join("write.lock"))
        .unwrap();
    file.try_lock().unwrap();
    file
}

#[test]
fn held_lock_rejects_all_writers_without_mutation_or_collection() {
    for populated in [false, true] {
        let temp = TempDir::new();
        let dir = temp.0.join("data");
        let input = fixture(&temp.0, "new.gif", &frame(1));
        if populated {
            let original = fixture(&temp.0, "original.gif", &quota_gif());
            result(invoke(&dir, &["fallback", "set", "boot", &original]), 0);
            result(invoke(&dir, &["publish", &original]), 0);
            // 预置无引用内容，确认拒绝路径不执行回收。
            let orphan = frame(2);
            store::store_content(&dir, &orphan, &gif::validate(&orphan).sha256).unwrap();
        }
        let held = lock(&dir);
        let before = snapshot(&dir);
        for args in [
            vec!["publish", &input],
            vec!["fallback", "set", "boot", &input],
            vec!["fallback", "set", "new", &input],
            vec!["fallback", "rm", "boot"],
        ] {
            busy(invoke(&dir, &args));
            assert_eq!(snapshot(&dir), before);
            assert!(dir.join("write.lock").is_file());
        }
        held.unlock().unwrap();
    }
}

// 断言失败时也终止并回收辅助进程，避免遗留持锁进程。
struct LockHolder(Child);

impl Drop for LockHolder {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
#[ignore = "仅由 killed_lock_holder_releases_lock_immediately 启动的辅助进程"]
fn lock_holder_process() {
    let root = PathBuf::from(std::env::var_os("AP01_BRIDGE_TEST_LOCK_ROOT").unwrap());
    let held = lock(&root.join("data"));
    // 标记写在业务目录之外，父进程看到标记后才执行持锁断言和 kill。
    fs::write(root.join("ready"), b"").unwrap();
    let mut byte = [0];
    let _ = std::io::stdin().read(&mut byte);
    drop(held);
}

#[test]
fn killed_lock_holder_releases_lock_immediately() {
    let temp = TempDir::new();
    let dir = temp.0.join("data");
    let input = fixture(&temp.0, "input.gif", &quota_gif());
    let mut holder = LockHolder(
        Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "lock_holder_process", "--ignored"])
            .env("AP01_BRIDGE_TEST_LOCK_ROOT", &temp.0)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while !temp.0.join("ready").exists() {
        assert!(holder.0.try_wait().unwrap().is_none(), "持锁子进程提前退出");
        assert!(Instant::now() < deadline, "持锁子进程未及时就绪");
        thread::sleep(Duration::from_millis(5));
    }
    busy(invoke(&dir, &["publish", &input]));
    holder.0.kill().unwrap();
    assert!(!holder.0.wait().unwrap().success());
    // 强制终止并回收后立即发布，不等待超时，也不清理锁文件。
    let actual = result(invoke(&dir, &["publish", &input]), 0);
    let unlocked = temp.0.join("unlocked");
    assert_eq!(actual, result(invoke(&unlocked, &["publish", &input]), 0));
    assert_eq!(snapshot(&dir), snapshot(&unlocked));
    assert_eq!(fs::read(dir.join("write.lock")).unwrap(), b"");
}

#[test]
fn failed_content_write_releases_publish_and_set_locks() {
    for publish in [true, false] {
        let temp = TempDir::new();
        let dir = temp.0.join("data");
        let bytes = quota_gif();
        let input = fixture(&temp.0, "input.gif", &bytes);
        let occupied = store::content_path(&dir, &gif::validate(&bytes).sha256);
        fs::create_dir_all(&occupied).unwrap();
        let args = if publish {
            vec!["publish", &input]
        } else {
            vec!["fallback", "set", "boot", &input]
        };
        let error = result(invoke(&dir, &args), 4);
        assert_eq!(
            error["error"]["message"],
            format!("内容路径不是文件：{}", occupied.display())
        );
        assert!(dir.join("write.lock").is_file());
        assert!(!dir.join("current.json").exists());
        assert!(!dir.join("fallbacks/boot.json").exists());
        let next = fixture(&temp.0, "next.gif", &frame(1));
        result(invoke(&dir, &["publish", &next]), 0);
    }
}

#[test]
fn fallback_removal_checks_current_under_lock_and_releases_on_failure() {
    let temp = TempDir::new();
    let dir = temp.0.join("data");
    let input = fixture(&temp.0, "input.gif", &quota_gif());
    result(invoke(&dir, &["fallback", "set", "boot", &input]), 0);
    fs::write(dir.join("current.json"), b"{").unwrap();
    let held = lock(&dir);
    busy(invoke(&dir, &["fallback", "rm", "boot"]));
    drop(held);
    let error = result(invoke(&dir, &["fallback", "rm", "boot"]), 4);
    assert!(
        error["error"]["message"]
            .as_str()
            .unwrap()
            .contains("无法确认引用关系")
    );
    assert!(dir.join("fallbacks/boot.json").exists());
    fs::remove_file(dir.join("current.json")).unwrap();
    result(invoke(&dir, &["fallback", "rm", "boot"]), 0);
    assert!(dir.join("write.lock").is_file());
    assert!(!dir.join("fallbacks/boot.json").exists());
}

#[test]
fn status_and_readonly_validation_ignore_held_lock() {
    let temp = TempDir::new();
    let dir = temp.0.join("data");
    let input = fixture(&temp.0, "input.gif", &quota_gif());
    let invalid = fixture(&temp.0, "invalid.gif", &GifBuilder::default().build());
    result(invoke(&dir, &["status"]), 0);
    assert!(!dir.exists());
    result(invoke(&dir, &["publish", &input]), 0);
    let expected = invoke(&dir, &["status"]);
    assert_eq!(expected.status.code(), Some(0));
    let held = lock(&dir);
    let before = snapshot(&dir);
    let actual = invoke(&dir, &["status"]);
    assert_eq!(actual.stdout, expected.stdout);
    result(actual, 0);
    for (args, code) in [
        (vec!["publish", &invalid], 3),
        (vec!["publish", &input, "--fallback", "Bad"], 2),
        (vec!["fallback", "set", "Bad", &input], 2),
        (vec!["fallback", "set", "boot", &invalid], 3),
        (vec!["fallback", "rm", "Bad"], 2),
    ] {
        result(invoke(&dir, &args), code);
        assert_eq!(snapshot(&dir), before);
    }
    // 不存在的合法槽也必须先取锁，不能提前做存在性检查。
    busy(invoke(&dir, &["publish", &input, "--fallback", "missing"]));
    assert_eq!(snapshot(&dir), before);
    drop(held);
    let error = result(
        invoke(&dir, &["publish", &input, "--fallback", "missing"]),
        4,
    );
    assert_eq!(error["error"]["message"], "回退槽 missing 不存在");
    assert_eq!(snapshot(&dir), before);
}

#[test]
fn concurrent_publish_and_fallback_removal_preserve_references() {
    let temp = TempDir::new();
    let input = fixture(&temp.0, "input.gif", &quota_gif());
    for round in 0..20 {
        let dir = temp.0.join(format!("round-{round}"));
        result(invoke(&dir, &["fallback", "set", "x", &input]), 0);
        let mut publish = command(&dir);
        publish.args(["publish", &input, "--fallback", "x"]);
        let mut remove = command(&dir);
        remove.args(["fallback", "rm", "x"]);
        // 交替启动顺序，全部 spawn 后才等待，覆盖两种竞争方向。
        let (publisher, remover) = if round % 2 == 0 {
            let publisher = publish.spawn().unwrap();
            (publisher, remove.spawn().unwrap())
        } else {
            let remover = remove.spawn().unwrap();
            (publish.spawn().unwrap(), remover)
        };
        let published = publisher.wait_with_output().unwrap();
        let removed = remover.wait_with_output().unwrap();
        let publish_code = published.status.code().unwrap();
        let remove_code = removed.status.code().unwrap();
        assert!(matches!(publish_code, 0 | 4), "发布结果：{published:?}");
        assert!(matches!(remove_code, 0 | 4), "删除结果：{removed:?}");
        result(published, publish_code);
        result(removed, remove_code);
        assert!(
            publish_code != 0 || remove_code != 0,
            "第 {round} 轮两个写者同时成功"
        );
        if publish_code == 0 {
            assert!(dir.join("fallbacks/x.json").is_file());
            let current: Value =
                serde_json::from_slice(&fs::read(dir.join("current.json")).unwrap()).unwrap();
            assert_eq!(current["fallback"], "x");
        }
        if remove_code == 0 {
            assert_eq!(publish_code, 4);
            assert!(!dir.join("fallbacks/x.json").exists());
            if dir.join("current.json").exists() {
                let current: Value =
                    serde_json::from_slice(&fs::read(dir.join("current.json")).unwrap()).unwrap();
                assert_ne!(current["fallback"], "x");
            }
        }
    }
}

#[test]
fn concurrent_publishers_leave_current_content_present() {
    let temp = TempDir::new();
    let dir = temp.0.join("data");
    let inputs: Vec<_> = (0..4)
        .map(|index| {
            let bytes = frame(index);
            assert!(gif::validate(&bytes).ok);
            fixture(&temp.0, &format!("input-{index}.gif"), &bytes)
        })
        .collect();
    // 必须先启动全部进程，再等待结果，避免把并发场景写成串行发布。
    let children: Vec<_> = inputs
        .iter()
        .map(|input| command(&dir).args(["publish", input]).spawn().unwrap())
        .collect();
    let outputs: Vec<_> = children
        .into_iter()
        .map(|child| child.wait_with_output().unwrap())
        .collect();
    let mut successes = 0;
    for output in outputs {
        match output.status.code() {
            Some(0) => {
                result(output, 0);
                successes += 1;
            }
            Some(4) => busy(output),
            _ => panic!("发布退出码应为 0 或 4：{output:?}"),
        }
    }
    assert!(successes >= 1);
    let current: Value =
        serde_json::from_slice(&fs::read(dir.join("current.json")).unwrap()).unwrap();
    let bytes = fs::read(store::content_path(&dir, current["gif"].as_str().unwrap())).unwrap();
    assert_eq!(bytes.len() as u64, current["bytes"].as_u64().unwrap());
    assert_eq!(gif::validate(&bytes).sha256, current["gif"]);
    assert_eq!(fs::read(dir.join("write.lock")).unwrap(), b"");
}

#[test]
fn existing_lock_is_preserved_without_truncation_or_replacement() {
    let temp = TempDir::new();
    let dir = temp.0.join("data");
    fs::create_dir(&dir).unwrap();
    let path = dir.join("write.lock");
    fs::write(&path, "已有锁文件内容").unwrap();
    let original = File::options().read(true).write(true).open(&path).unwrap();
    let input = fixture(&temp.0, "input.gif", &quota_gif());
    result(invoke(&dir, &["publish", &input]), 0);
    assert_eq!(fs::read_to_string(&path).unwrap(), "已有锁文件内容");
    // 对原句柄加锁后仍能拒绝写者，证明成功路径没有删除并重建文件。
    original.try_lock().unwrap();
    busy(invoke(&dir, &["publish", &input]));
    original.unlock().unwrap();
}

#[test]
fn lock_acquisition_failure_reports_path_without_mutation() {
    for blocked_directory in [true, false] {
        let temp = TempDir::new();
        let dir = temp.0.join("data");
        if blocked_directory {
            fs::write(&dir, "占据目录位置").unwrap();
        } else {
            fs::create_dir_all(dir.join("write.lock")).unwrap();
        }
        let before = snapshot(&dir);
        let input = fixture(&temp.0, "input.gif", &quota_gif());
        let error = result(invoke(&dir, &["publish", &input]), 4);
        assert_eq!(
            error["error"]["message"],
            format!("无法获取写锁：{}", dir.join("write.lock").display())
        );
        assert_eq!(snapshot(&dir), before);
    }
}
