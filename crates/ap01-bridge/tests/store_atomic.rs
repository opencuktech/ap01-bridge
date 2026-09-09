//! 原子写入的并发可见性、同名残留和失败清理。

mod common;
use ap01_bridge_kernel::store::atomic;
use common::{TempDir, bridge};
use std::{
    fs,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
};

#[test]
fn atomic_files_are_complete_collisions_are_preserved_and_failures_clean_up() {
    let temp = TempDir::new();
    let final_path = temp.0.join("nested/current.json");
    fs::create_dir(final_path.parent().unwrap()).unwrap();
    let collision =
        final_path.with_file_name(format!(".current.json.tmp-{}-0", std::process::id()));
    fs::write(&collision, "残留".as_bytes()).unwrap();
    let old = vec![1; 262_144];
    let new = vec![2; 262_144];
    let done = Arc::new(AtomicBool::new(false));
    let (reader_path, reader_done) = (final_path.clone(), done.clone());
    let reader = thread::spawn(move || {
        let mut reads = 0;
        while !reader_done.load(Ordering::Acquire) || reads == 0 {
            match fs::read(&reader_path) {
                Ok(bytes) => {
                    assert!(bytes == vec![1; 262_144] || bytes == vec![2; 262_144]);
                    reads += 1;
                }
                Err(error) => assert_eq!(error.kind(), std::io::ErrorKind::NotFound),
            }
        }
    });
    atomic::write_file(&final_path, &old).unwrap();
    assert_eq!(fs::read(&collision).unwrap(), "残留".as_bytes());
    for index in 0..40 {
        atomic::write_file(&final_path, if index % 2 == 0 { &old } else { &new }).unwrap();
    }
    done.store(true, Ordering::Release);
    reader.join().unwrap();
    assert_eq!(fs::read(&final_path).unwrap(), new);
    let blocked = temp.0.join("blocked");
    fs::create_dir(&blocked).unwrap();
    assert!(atomic::write_file(&blocked, "完整内容".as_bytes()).is_err());
    assert!(final_path.is_file());
    assert!(blocked.is_dir());
    for directory in [&temp.0, final_path.parent().unwrap()] {
        for entry in fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            // 预置的碰撞文件必须保留，其余原子写临时文件不得残留。
            if path != collision {
                let name = path.file_name().unwrap().to_string_lossy();
                assert!(!(name.starts_with('.') && name.contains(".tmp-")));
            }
        }
    }
    // 同时确认共享 helper 启动的真实命令不会把残留当作状态。
    let output = bridge()
        .args(["status", "--json", "--data-dir"])
        .arg(&temp.0)
        .output()
        .unwrap();
    assert!(output.status.success());
}
