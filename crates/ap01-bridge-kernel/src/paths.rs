//! 使用注入的环境变量解析数据目录，不访问进程环境。

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::KernelError;

/// 数据目录默认规则所对应的平台。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    MacOs,
    Linux,
    Windows,
}

impl Platform {
    /// 返回当前编译目标的平台，其它类 Unix 目标沿用 Linux 规则。
    pub fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::MacOs
        } else if cfg!(target_os = "windows") {
            Self::Windows
        } else {
            Self::Linux
        }
    }
}

/// 按显式参数、环境覆盖、平台默认的顺序定位，不创建或规范化目录。
///
/// 空环境变量视为未设置，显式路径则原样保留。
pub fn resolve_data_dir(
    explicit: Option<&Path>,
    env: &dyn Fn(&str) -> Option<OsString>,
    platform: Platform,
) -> Result<PathBuf, KernelError> {
    if let Some(path) = explicit {
        return Ok(path.to_path_buf());
    }
    let nonempty_env = |name| env(name).filter(|value| !value.is_empty());
    if let Some(path) = nonempty_env("AP01_BRIDGE_DATA_DIR") {
        return Ok(PathBuf::from(path));
    }
    let required = |name| {
        nonempty_env(name)
            .map(PathBuf::from)
            .ok_or_else(|| KernelError::State(format!("无法确定数据目录：未设置环境变量 {name}")))
    };
    let base = match platform {
        Platform::MacOs => required("HOME")?
            .join("Library")
            .join("Application Support"),
        Platform::Linux => match nonempty_env("XDG_DATA_HOME") {
            Some(path) => PathBuf::from(path),
            None => required("HOME")?.join(".local").join("share"),
        },
        Platform::Windows => required("LOCALAPPDATA")?,
    };
    Ok(base.join("cuktech-ap01-bridge"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn resolve(platform: Platform, vars: &[(&str, &str)]) -> Result<PathBuf, KernelError> {
        let vars: HashMap<_, _> = vars.iter().copied().collect();
        resolve_data_dir(None, &|key| vars.get(key).map(OsString::from), platform)
    }

    #[test]
    fn defaults_for_all_platforms() {
        for (platform, vars, expected) in [
            (
                Platform::MacOs,
                vec![("HOME", "home")],
                Path::new("home")
                    .join("Library")
                    .join("Application Support"),
            ),
            (
                Platform::Linux,
                vec![("HOME", "home")],
                Path::new("home").join(".local").join("share"),
            ),
            (
                Platform::Windows,
                vec![("LOCALAPPDATA", "local")],
                PathBuf::from("local"),
            ),
        ] {
            assert_eq!(
                resolve(platform, &vars).unwrap(),
                expected.join("cuktech-ap01-bridge")
            );
        }
    }

    #[test]
    fn xdg_overrides_home_and_does_not_require_home() {
        for vars in [
            vec![("XDG_DATA_HOME", "xdg"), ("HOME", "home")],
            vec![("XDG_DATA_HOME", "xdg")],
        ] {
            assert_eq!(
                resolve(Platform::Linux, &vars).unwrap(),
                Path::new("xdg").join("cuktech-ap01-bridge")
            );
        }
    }

    #[test]
    fn environment_and_explicit_path_priorities() {
        let vars = HashMap::from([
            ("AP01_BRIDGE_DATA_DIR", OsString::from("override")),
            ("HOME", OsString::from("home")),
            ("XDG_DATA_HOME", OsString::from("xdg")),
            ("LOCALAPPDATA", OsString::from("local")),
        ]);
        for platform in [Platform::MacOs, Platform::Linux, Platform::Windows] {
            let env = |key: &str| vars.get(key).cloned();
            assert_eq!(
                resolve_data_dir(None, &env, platform).unwrap(),
                PathBuf::from("override")
            );
            assert_eq!(
                resolve_data_dir(Some(Path::new("explicit")), &env, platform).unwrap(),
                PathBuf::from("explicit")
            );
            assert_eq!(
                resolve_data_dir(
                    Some(Path::new("explicit")),
                    &|_| panic!("显式参数不应查询环境"),
                    platform
                )
                .unwrap(),
                PathBuf::from("explicit")
            );
        }
    }

    #[test]
    fn missing_default_environment_is_a_state_error() {
        for platform in [Platform::MacOs, Platform::Linux, Platform::Windows] {
            let error = resolve(platform, &[]).unwrap_err();
            assert!(matches!(error, KernelError::State(_)));
            assert_eq!(error.exit_code(), 4);
        }
    }

    #[test]
    fn empty_environment_uses_fallback() {
        assert_eq!(
            resolve(
                Platform::Linux,
                &[
                    ("AP01_BRIDGE_DATA_DIR", ""),
                    ("XDG_DATA_HOME", ""),
                    ("HOME", "home")
                ]
            )
            .unwrap(),
            Path::new("home")
                .join(".local")
                .join("share")
                .join("cuktech-ap01-bridge")
        );
        assert!(resolve(Platform::Windows, &[("LOCALAPPDATA", "")]).is_err());
    }
}
